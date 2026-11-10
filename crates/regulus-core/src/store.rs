//! The store (docs/02-architecture.md §6): SQLite, one database for what belongs to no
//! project and one per project. It keeps executions, their checkpoints, the bus journal,
//! and a key-value space per module. It never calls back into the core.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use regulus_proto::{BusEvent, EventListParams, Execution, ExecutionState, PortRef, Workflow};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::config::{Resume, StoreConfig, Synchronous};
use crate::module::ModuleError;
use crate::registry::now_ms;
use crate::service::path_segment;

/// The database of what belongs to no project, inside the store directory.
const GLOBAL_FILE: &str = "global.db";
/// The databases of projects, `<id>.db` each, inside the store directory.
const PROJECTS_DIR: &str = "projects";
const DB_EXTENSION: &str = "db";

/// Forward migrations, in order: entry `i` takes a database from version `i` to `i + 1`.
/// Never edit one that shipped; add the next.
const MIGRATIONS: &[&str] = &[
    // 1: executions, checkpoints, the bus journal and the modules' key-value space.
    "CREATE TABLE executions (
        id          TEXT PRIMARY KEY,
        number      INTEGER NOT NULL,
        project_id  TEXT,
        profile_id  TEXT NOT NULL,
        workflow_id TEXT NOT NULL,
        workflow    TEXT NOT NULL,
        status      TEXT NOT NULL,
        reason      TEXT,
        disabled    TEXT NOT NULL,
        parent      TEXT,
        started     INTEGER NOT NULL,
        finished    INTEGER
    );
    CREATE INDEX executions_status ON executions (status);
    CREATE TABLE checkpoints (
        execution_id TEXT NOT NULL REFERENCES executions (id) ON DELETE CASCADE,
        seq          INTEGER NOT NULL,
        node_id      TEXT NOT NULL,
        state        BLOB NOT NULL,
        created      INTEGER NOT NULL,
        PRIMARY KEY (execution_id, seq)
    );
    CREATE TABLE events (
        seq          INTEGER PRIMARY KEY,
        at           INTEGER NOT NULL,
        execution_id TEXT NOT NULL,
        project_id   TEXT,
        event        INTEGER NOT NULL,
        from_node    TEXT NOT NULL,
        from_port    TEXT NOT NULL,
        capability   TEXT NOT NULL,
        depth        INTEGER NOT NULL,
        kind         TEXT NOT NULL,
        data         TEXT NOT NULL
    );
    CREATE INDEX events_execution ON events (execution_id, seq);
    CREATE TABLE kv (
        namespace TEXT NOT NULL,
        key       TEXT NOT NULL,
        value     BLOB NOT NULL,
        updated   INTEGER NOT NULL,
        PRIMARY KEY (namespace, key)
    ) WITHOUT ROWID;",
    // 2: the node of the parent a child execution answers to, so a resumed parent still
    // hears that its child was interrupted.
    "ALTER TABLE executions ADD COLUMN caller TEXT;",
    // 3: what the caller of a child tagged it with (`ChildOptions::tag`).
    "ALTER TABLE executions ADD COLUMN tag TEXT;",
];

/// An execution as the store keeps it: with its number (the `N` of `exec-N`, unique in a
/// home) and the copy of the workflow it runs.
#[derive(Debug, Clone)]
pub struct StoredExecution {
    pub number: u64,
    pub execution: Execution,
    pub workflow: Workflow,
    /// For a child: the node of its parent that called it.
    pub caller: Option<String>,
}

/// The `status` column and the `reason` column of a state, as the protocol spells them.
fn status_of(state: &ExecutionState) -> (String, Option<String>) {
    let value = serde_json::to_value(state).unwrap_or_default();
    let status = value["status"].as_str().unwrap_or_default().to_string();
    let reason = value
        .get("reason")
        .and_then(Value::as_str)
        .map(String::from);
    (status, reason)
}

fn state_of(status: String, reason: Option<String>) -> rusqlite::Result<ExecutionState> {
    let mut value = json!({ "status": status });
    if let Some(reason) = reason {
        value["reason"] = reason.into();
    }
    serde_json::from_value(value).map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))
}

fn json_column<T: DeserializeOwned>(row: &Row, index: usize) -> rusqlite::Result<T> {
    let text: String = row.get(index)?;
    serde_json::from_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, e.into())
    })
}

fn to_json(value: &impl Serialize) -> rusqlite::Result<String> {
    serde_json::to_string(value).map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))
}

/// The columns `stored_execution` reads, in order.
const EXECUTION_COLUMNS: &str =
    "number, id, project_id, profile_id, workflow_id, status, reason, disabled, workflow, parent, caller, \
     tag";

fn stored_execution(row: &Row) -> rusqlite::Result<StoredExecution> {
    let number: i64 = row.get(0)?;
    Ok(StoredExecution {
        number: number.max(0) as u64,
        execution: Execution {
            id: row.get(1)?,
            project_id: row.get(2)?,
            profile_id: row.get(3)?,
            workflow_id: row.get(4)?,
            state: state_of(row.get(5)?, row.get(6)?)?,
            disabled: json_column(row, 7)?,
            parent: row.get(9)?,
            caller: row.get(10)?,
            tag: row.get(11)?,
        },
        workflow: json_column(row, 8)?,
        caller: row.get(10)?,
    })
}

fn bus_event(row: &Row) -> rusqlite::Result<BusEvent> {
    let unsigned = |index| row.get::<_, i64>(index).map(|n| n.max(0) as u64);
    Ok(BusEvent {
        seq: unsigned(0)?,
        at: unsigned(1)?,
        execution_id: row.get(2)?,
        project_id: row.get(3)?,
        event: unsigned(4)?,
        from: PortRef {
            node_id: row.get(5)?,
            port_id: row.get(6)?,
        },
        capability: row.get(7)?,
        depth: row.get(8)?,
        kind: json_column(row, 9)?,
    })
}

/// SQLite integers are `i64`; ours are unsigned and never get near the top.
fn int(n: impl TryInto<i64>) -> i64 {
    n.try_into().unwrap_or(i64::MAX)
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {source}")]
    Sqlite {
        path: PathBuf,
        source: rusqlite::Error,
    },
    #[error("cannot create {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "{path} is at schema version {found}, newer than the {known} this core knows: \
         a newer Regulus wrote it"
    )]
    Newer {
        path: PathBuf,
        found: u32,
        known: u32,
    },
    #[error("`{0}` cannot name a project database")]
    Project(String),
    #[error("{0}")]
    Data(String),
}

impl From<StoreError> for ModuleError {
    fn from(error: StoreError) -> Self {
        ModuleError::Failed(error.to_string())
    }
}

/// One SQLite database and its single connection.
#[derive(Debug)]
pub(crate) struct Db {
    path: PathBuf,
    conn: Mutex<Connection>,
    /// Bus events kept; 0 keeps them all.
    max_events: usize,
    /// Events written since the last prune.
    inserted: AtomicUsize,
}

impl Db {
    fn open(path: &Path, config: &StoreConfig) -> Result<Self, StoreError> {
        let db = Self::open_with(path, config, MIGRATIONS)?;
        db.prune()?;
        Ok(db)
    }

    fn open_with(
        path: &Path,
        config: &StoreConfig,
        migrations: &[&str],
    ) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let sqlite = |source| StoreError::Sqlite {
            path: path.to_path_buf(),
            source,
        };
        let mut conn = Connection::open(path).map_err(sqlite)?;
        conn.busy_timeout(Duration::from_millis(config.busy_timeout_ms))
            .map_err(sqlite)?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))
            .map_err(sqlite)?;
        let synchronous = match config.synchronous {
            Synchronous::Off => "OFF",
            Synchronous::Normal => "NORMAL",
            Synchronous::Full => "FULL",
            Synchronous::Extra => "EXTRA",
        };
        conn.pragma_update(None, "synchronous", synchronous)
            .map_err(sqlite)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(sqlite)?;
        migrate(path, &mut conn, migrations)?;
        Ok(Self {
            path: path.to_path_buf(),
            conn: Mutex::new(conn),
            max_events: config.max_events,
            inserted: AtomicUsize::new(0),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Runs `f` on the connection, naming this database in its errors.
    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, StoreError> {
        f(&self.lock()).map_err(|source| StoreError::Sqlite {
            path: self.path.clone(),
            source,
        })
    }

    /// Drops the oldest bus events past `max_events`.
    fn prune(&self) -> Result<(), StoreError> {
        if self.max_events == 0 {
            return Ok(());
        }
        self.inserted.store(0, Ordering::Relaxed);
        self.with(|conn| {
            conn.execute(
                "DELETE FROM events WHERE seq <= \
                 (SELECT seq FROM events ORDER BY seq DESC LIMIT 1 OFFSET ?1)",
                [int(self.max_events)],
            )
        })?;
        Ok(())
    }

    fn schema_version(&self) -> Result<u32, StoreError> {
        self.with(current_version)
    }
}

fn current_version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |row| row.get(0),
    )
}

/// Brings a database up to the last of `migrations`, one transaction per step. A database
/// newer than this core is left untouched.
fn migrate(path: &Path, conn: &mut Connection, migrations: &[&str]) -> Result<(), StoreError> {
    let sqlite = |source| StoreError::Sqlite {
        path: path.to_path_buf(),
        source,
    };
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY,
            applied INTEGER NOT NULL
        )",
    )
    .map_err(sqlite)?;
    let found = current_version(conn).map_err(sqlite)?;
    let known = migrations.len() as u32;
    if found > known {
        return Err(StoreError::Newer {
            path: path.to_path_buf(),
            found,
            known,
        });
    }
    for (step, sql) in migrations.iter().enumerate().skip(found as usize) {
        let tx = conn.transaction().map_err(sqlite)?;
        tx.execute_batch(sql).map_err(sqlite)?;
        tx.execute(
            "INSERT INTO schema_version (version, applied) VALUES (?1, ?2)",
            params![step as u32 + 1, int(now_ms())],
        )
        .map_err(sqlite)?;
        tx.commit().map_err(sqlite)?;
    }
    Ok(())
}

/// All the databases of one home: the global one, and one per project, opened as they are
/// found on start or first used. Shared by the store and its writer thread.
#[derive(Debug)]
struct Databases {
    dir: PathBuf,
    config: StoreConfig,
    global: Arc<Db>,
    projects: Mutex<BTreeMap<String, Arc<Db>>>,
}

/// What the writer thread is asked to do, in the order it was asked.
enum Write {
    Insert {
        number: u64,
        execution: Execution,
        workflow: String,
        caller: Option<String>,
        at: u64,
    },
    Update {
        execution: Execution,
        at: u64,
    },
    Event(BusEvent),
    Checkpoint {
        project: Option<String>,
        execution_id: String,
        node_id: String,
        state: Vec<u8>,
        at: u64,
        done: SyncSender<Result<(), StoreError>>,
    },
    /// Answers once everything asked before it is written.
    Flush(SyncSender<()>),
}

/// The name of the thread that writes to the store.
const WRITER_THREAD: &str = "regulus-store";

/// The store: reads answer from the databases; writes of executions and bus events are
/// queued, in order, to one writer thread, so whoever writes never waits for the disk or
/// for a database another connection holds.
#[derive(Debug)]
pub struct Store {
    dbs: Arc<Databases>,
    writer: Mutex<Option<Sender<Write>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    /// Read on open: the numbers to go on from and what the last core left running.
    first_execution: u64,
    first_seq: u64,
    left_running: Vec<StoredExecution>,
    /// What open skipped: project databases it could not open, rows it could not read.
    warnings: Vec<String>,
}

impl Databases {
    fn projects(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Db>>> {
        self.projects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The database of `project`, or the global one; a project's is created on first use.
    fn db(&self, project: Option<&str>) -> Result<Arc<Db>, StoreError> {
        let Some(project) = project else {
            return Ok(self.global.clone());
        };
        let mut projects = self.projects();
        if let Some(db) = projects.get(project) {
            return Ok(db.clone());
        }
        let file = path_segment(project)
            .map_err(|_| StoreError::Project(project.into()))?
            .to_string();
        let path = self
            .dir
            .join(PROJECTS_DIR)
            .join(format!("{file}.{DB_EXTENSION}"));
        let db = Arc::new(Db::open(&path, &self.config)?);
        projects.insert(project.into(), db.clone());
        Ok(db)
    }

    /// Whether `project` has a database: every one on disk opens with the store.
    fn has(&self, project: &str) -> bool {
        self.projects().contains_key(project)
    }

    /// The global database and every project database open now.
    fn all(&self) -> Vec<Arc<Db>> {
        let mut all = vec![self.global.clone()];
        all.extend(self.projects().values().cloned());
        all
    }

    /// The databases a `project_id` filter reaches: that project's, or all of them.
    fn reached_by(&self, project: Option<&str>) -> Vec<Arc<Db>> {
        match project {
            None => self.all(),
            Some(project) => self.projects().get(project).cloned().into_iter().collect(),
        }
    }

    fn apply(&self, write: Write) {
        let result = match write {
            Write::Insert {
                number,
                execution,
                workflow,
                caller,
                at,
            } => self.insert_execution(number, &execution, &workflow, caller.as_deref(), at),
            Write::Update { execution, at } => self.update_execution(&execution, at),
            Write::Event(event) => self.append_event(&event),
            Write::Checkpoint {
                project,
                execution_id,
                node_id,
                state,
                at,
                done,
            } => {
                let written =
                    self.checkpoint(project.as_deref(), &execution_id, &node_id, &state, at);
                // The module waiting for it may have given up; nothing to report then.
                let _ = done.send(written);
                Ok(())
            }
            Write::Flush(done) => {
                let _ = done.send(());
                Ok(())
            }
        };
        if let Err(error) = result {
            eprintln!("regulus: store: {error}");
        }
    }

    fn insert_execution(
        &self,
        number: u64,
        execution: &Execution,
        workflow: &str,
        caller: Option<&str>,
        at: u64,
    ) -> Result<(), StoreError> {
        let (status, reason) = status_of(&execution.state);
        self.db(execution.project_id.as_deref())?.with(|conn| {
            conn.execute(
                "INSERT INTO executions (id, number, project_id, profile_id, workflow_id,
                     workflow, status, reason, disabled, parent, started, caller, tag)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    execution.id,
                    int(number),
                    execution.project_id,
                    execution.profile_id,
                    execution.workflow_id,
                    workflow,
                    status,
                    reason,
                    to_json(&execution.disabled)?,
                    execution.parent,
                    int(at),
                    caller,
                    execution.tag,
                ],
            )
        })?;
        Ok(())
    }

    fn update_execution(&self, execution: &Execution, at: u64) -> Result<(), StoreError> {
        let (status, reason) = status_of(&execution.state);
        self.db(execution.project_id.as_deref())?.with(|conn| {
            conn.execute(
                "UPDATE executions SET status = ?2, reason = ?3, disabled = ?4,
                     finished = CASE WHEN ?5 THEN NULL ELSE COALESCE(finished, ?6) END
                 WHERE id = ?1",
                params![
                    execution.id,
                    status,
                    reason,
                    to_json(&execution.disabled)?,
                    execution.state.is_live(),
                    int(at),
                ],
            )
        })?;
        Ok(())
    }

    fn checkpoint(
        &self,
        project: Option<&str>,
        execution_id: &str,
        node_id: &str,
        state: &[u8],
        at: u64,
    ) -> Result<(), StoreError> {
        self.db(project)?.with(|conn| {
            conn.execute(
                "INSERT INTO checkpoints (execution_id, seq, node_id, state, created)
                 SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4
                 FROM checkpoints WHERE execution_id = ?1",
                params![execution_id, node_id, state, int(at)],
            )?;
            conn.execute(
                "DELETE FROM checkpoints WHERE execution_id = ?1 AND node_id = ?2
                 AND seq < (SELECT MAX(seq) FROM checkpoints WHERE execution_id = ?1)",
                params![execution_id, node_id],
            )
        })?;
        Ok(())
    }

    fn append_event(&self, event: &BusEvent) -> Result<(), StoreError> {
        let kind =
            serde_json::to_value(&event.kind).map_err(|e| StoreError::Data(e.to_string()))?;
        let db = self.db(event.project_id.as_deref())?;
        db.with(|conn| {
            conn.prepare_cached(
                "INSERT INTO events (seq, at, execution_id, project_id, event, from_node,
                     from_port, capability, depth, kind, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?
            .execute(params![
                int(event.seq),
                int(event.at),
                event.execution_id,
                event.project_id,
                int(event.event),
                event.from.node_id,
                event.from.port_id,
                event.capability,
                event.depth,
                kind["kind"].as_str().unwrap_or_default(),
                kind.to_string(),
            ])
        })?;
        if db.max_events > 0 && db.inserted.fetch_add(1, Ordering::Relaxed) + 1 >= db.max_events {
            db.prune()?;
        }
        Ok(())
    }

    /// One past the highest value `sql` finds in any database.
    fn next_after(&self, sql: &str) -> Result<u64, StoreError> {
        let mut highest = 0;
        for db in self.all() {
            let found: i64 = db.with(|conn| conn.query_row(sql, [], |r| r.get(0)))?;
            highest = highest.max(found.max(0) as u64);
        }
        Ok(highest + 1)
    }

    /// The executions `filter` selects, by number, and a line for each row that could
    /// not be read (an unknown status, broken JSON): those are skipped, not fatal.
    fn select_executions(
        &self,
        project: Option<&str>,
        filter: &str,
    ) -> Result<(Vec<StoredExecution>, Vec<String>), StoreError> {
        let sql = format!("SELECT {EXECUTION_COLUMNS} FROM executions {filter} ORDER BY number");
        let (mut all, mut skipped) = (Vec::new(), Vec::new());
        for db in self.reached_by(project) {
            let rows = db.with(|conn| {
                conn.prepare_cached(&sql)?
                    .query_map([], |row| {
                        Ok(stored_execution(row).map_err(|e| {
                            let id: String = row.get(1).unwrap_or_default();
                            format!("{}: skipping execution `{id}`: {e}", db.path.display())
                        }))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?;
            for row in rows {
                match row {
                    Ok(execution) => all.push(execution),
                    Err(line) => skipped.push(line),
                }
            }
        }
        all.sort_by_key(|e| e.number);
        Ok((all, skipped))
    }
}

impl Store {
    /// Opens (or creates) the store in `dir`, with every project database already there,
    /// migrates them all and starts the writer thread.
    pub fn open(dir: &Path, config: &StoreConfig) -> Result<Self, StoreError> {
        let global = Arc::new(Db::open(&dir.join(GLOBAL_FILE), config)?);
        let mut warnings = Vec::new();
        let mut projects = BTreeMap::new();
        let projects_dir = dir.join(PROJECTS_DIR);
        if let Ok(entries) = fs::read_dir(&projects_dir) {
            for path in entries.flatten().map(|entry| entry.path()) {
                if path.extension().and_then(|e| e.to_str()) != Some(DB_EXTENSION) {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                // One broken project must not take the others down: it is skipped.
                match Db::open(&path, config) {
                    Ok(db) => {
                        projects.insert(id.to_string(), Arc::new(db));
                    }
                    Err(error) => warnings.push(format!("store: skipping project `{id}`: {error}")),
                }
            }
        }
        let dbs = Arc::new(Databases {
            dir: dir.to_path_buf(),
            config: config.clone(),
            global,
            projects: Mutex::new(projects),
        });
        // Without numbers to go on from, ids and cursors would repeat: no store then.
        let first_execution = dbs.next_after("SELECT COALESCE(MAX(number), 0) FROM executions")?;
        let first_seq = dbs.next_after("SELECT COALESCE(MAX(seq), 0) FROM events")?;
        let (starting, _) = status_of(&ExecutionState::Starting);
        let (running, _) = status_of(&ExecutionState::Running);
        let (left_running, skipped) = dbs.select_executions(
            None,
            &format!("WHERE status IN ('{starting}', '{running}')"),
        )?;
        warnings.extend(skipped);
        let (sender, queue) = mpsc::channel::<Write>();
        let writing = dbs.clone();
        let thread = std::thread::Builder::new()
            .name(WRITER_THREAD.into())
            .spawn(move || {
                for write in queue {
                    writing.apply(write);
                }
            })
            .map_err(|source| StoreError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
        Ok(Self {
            dbs,
            writer: Mutex::new(Some(sender)),
            thread: Mutex::new(Some(thread)),
            first_execution,
            first_seq,
            left_running,
            warnings,
        })
    }

    /// What open skipped, one line each.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The number for the first execution of this core: one past the highest stored.
    pub fn first_execution(&self) -> u64 {
        self.first_execution
    }

    /// The `seq` for the first bus event of this core: one past the highest stored.
    pub fn first_seq(&self) -> u64 {
        self.first_seq
    }

    /// The executions the last core left starting or running, by number.
    pub fn left_running(&self) -> Vec<StoredExecution> {
        self.left_running.clone()
    }

    fn send(&self, write: Write) -> Result<(), StoreError> {
        let writer = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer
            .as_ref()
            .and_then(|sender| sender.send(write).ok())
            .ok_or_else(|| StoreError::Data("the store writer is gone".into()))
    }

    /// Waits until everything queued so far is written.
    pub fn flush(&self) {
        let (done, wait) = mpsc::sync_channel(1);
        if self.send(Write::Flush(done)).is_ok() {
            let _ = wait.recv();
        }
    }

    pub(crate) fn db(&self, project: Option<&str>) -> Result<Arc<Db>, StoreError> {
        self.dbs.db(project)
    }

    /// Whether `project` has a database here, without creating one.
    pub(crate) fn has_project(&self, project: &str) -> bool {
        self.dbs.has(project)
    }

    /// The key-value space `namespace` in the database of `project` (or the global one).
    pub(crate) fn module_store(
        &self,
        project: Option<&str>,
        namespace: &str,
    ) -> Result<ModuleStore, StoreError> {
        Ok(ModuleStore {
            db: self.db(project)?,
            namespace: namespace.into(),
        })
    }

    /// What happens on start to executions left running.
    pub fn resume(&self) -> Resume {
        self.dbs.config.resume
    }

    /// Queues a new execution with the workflow copy it runs.
    pub fn insert_execution(
        &self,
        number: u64,
        execution: &Execution,
        workflow: &Workflow,
    ) -> Result<(), StoreError> {
        self.insert_child(number, execution, workflow, None)
    }

    /// [`Store::insert_execution`] for a child, with the node of its parent that called it.
    pub fn insert_child(
        &self,
        number: u64,
        execution: &Execution,
        workflow: &Workflow,
        caller: Option<&str>,
    ) -> Result<(), StoreError> {
        let workflow =
            serde_json::to_string(workflow).map_err(|e| StoreError::Data(e.to_string()))?;
        self.send(Write::Insert {
            number,
            execution: execution.clone(),
            workflow,
            caller: caller.map(Into::into),
            at: now_ms(),
        })
    }

    /// Queues the state and disabled nodes of an execution; `finished` is stamped the
    /// first time it stops being live.
    pub fn update_execution(&self, execution: &Execution) -> Result<(), StoreError> {
        self.send(Write::Update {
            execution: execution.clone(),
            at: now_ms(),
        })
    }

    /// Every execution in the databases `project` reaches, by number. Rows that cannot
    /// be read are reported and left out.
    pub fn executions(&self, project: Option<&str>) -> Result<Vec<StoredExecution>, StoreError> {
        let (executions, skipped) = self.dbs.select_executions(project, "")?;
        for line in skipped {
            eprintln!("regulus: {line}");
        }
        Ok(executions)
    }

    pub fn execution(&self, id: &str) -> Result<Option<StoredExecution>, StoreError> {
        for db in self.dbs.all() {
            let found = db.with(|conn| {
                conn.query_row(
                    &format!("SELECT {EXECUTION_COLUMNS} FROM executions WHERE id = ?1"),
                    [id],
                    stored_execution,
                )
                .optional()
            })?;
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    /// Saves the state of a node of an execution; only its latest is kept. It goes
    /// through the writer, after the execution it belongs to, and waits until written.
    pub fn checkpoint(
        &self,
        project: Option<&str>,
        execution_id: &str,
        node_id: &str,
        state: &Value,
    ) -> Result<(), StoreError> {
        let state = serde_json::to_vec(state).map_err(|e| StoreError::Data(e.to_string()))?;
        let (done, wait) = mpsc::sync_channel(1);
        self.send(Write::Checkpoint {
            project: project.map(String::from),
            execution_id: execution_id.into(),
            node_id: node_id.into(),
            state,
            at: now_ms(),
            done,
        })?;
        wait.recv()
            .map_err(|_| StoreError::Data("the store writer is gone".into()))?
    }

    /// The latest checkpoint of each node of an execution.
    pub fn checkpoints(
        &self,
        project: Option<&str>,
        execution_id: &str,
    ) -> Result<HashMap<String, Value>, StoreError> {
        let rows: Vec<(String, Vec<u8>)> = self.db(project)?.with(|conn| {
            conn.prepare_cached(
                "SELECT node_id, state FROM checkpoints WHERE execution_id = ?1 ORDER BY seq",
            )?
            .query_map([execution_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
        })?;
        rows.into_iter()
            .map(|(node, state)| {
                serde_json::from_slice(&state)
                    .map(|state| (node, state))
                    .map_err(|e| StoreError::Data(format!("checkpoint of `{execution_id}`: {e}")))
            })
            .collect()
    }

    /// Queues a bus event for its project's database; past `2 × max_events` there, the
    /// oldest go.
    pub fn append_event(&self, event: &BusEvent) -> Result<(), StoreError> {
        self.send(Write::Event(event.clone()))
    }

    /// At most `limit` of the kept bus events `params` asks for with `seq` up to `upto`,
    /// oldest first, from every database it reaches. `params.limit` is the caller's to
    /// clamp and pass as `limit`.
    pub fn events(
        &self,
        params: &EventListParams,
        upto: u64,
        limit: Option<usize>,
    ) -> Result<Vec<BusEvent>, StoreError> {
        let sql_limit = limit.map_or(-1, int);
        let mut all = Vec::new();
        for db in self.dbs.reached_by(params.project_id.as_deref()) {
            let rows = db.with(|conn| {
                conn.prepare_cached(
                    "SELECT seq, at, execution_id, project_id, event, from_node, from_port,
                         capability, depth, data
                     FROM events
                     WHERE seq > ?1 AND seq <= ?5 AND (?2 IS NULL OR execution_id = ?2)
                         AND (?3 IS NULL OR project_id = ?3)
                     ORDER BY seq LIMIT ?4",
                )?
                .query_map(
                    params![
                        int(params.since),
                        params.execution_id,
                        params.project_id,
                        sql_limit,
                        int(upto)
                    ],
                    bus_event,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()
            })?;
            all.extend(rows);
        }
        all.sort_by_key(|e| e.seq);
        if let Some(limit) = limit {
            all.truncate(limit);
        }
        Ok(all)
    }

    /// The schema version every database is at.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        self.dbs.global.schema_version()
    }
}

/// Dropping the store writes what is still queued before it goes.
impl Drop for Store {
    fn drop(&mut self) {
        let writer = self.writer.get_mut().map(Option::take);
        drop(writer);
        let thread = self.thread.get_mut().ok().and_then(Option::take);
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

/// A module's own key-value space in one database (`ModuleCtx::store`). The core fixes the
/// namespace to the module's id; there is no way to name another.
#[derive(Debug, Clone)]
pub struct ModuleStore {
    db: Arc<Db>,
    namespace: String,
}

impl ModuleStore {
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ModuleError> {
        Ok(self.db.with(|conn| {
            conn.query_row(
                "SELECT value FROM kv WHERE namespace = ?1 AND key = ?2",
                params![self.namespace, key],
                |row| row.get(0),
            )
            .optional()
        })?)
    }

    pub fn set(&self, key: &str, value: &[u8]) -> Result<(), ModuleError> {
        self.db.with(|conn| {
            conn.execute(
                "INSERT INTO kv (namespace, key, value, updated) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (namespace, key) DO UPDATE
                 SET value = excluded.value, updated = excluded.updated",
                params![self.namespace, key, value, int(now_ms())],
            )
        })?;
        Ok(())
    }

    /// Whether there was something to delete.
    pub fn delete(&self, key: &str) -> Result<bool, ModuleError> {
        let deleted = self.db.with(|conn| {
            conn.execute(
                "DELETE FROM kv WHERE namespace = ?1 AND key = ?2",
                params![self.namespace, key],
            )
        })?;
        Ok(deleted > 0)
    }

    /// Every key starting with `prefix`, with its value, in key order.
    pub fn list(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, ModuleError> {
        let rows = self.db.with(|conn| {
            let mut statement = conn.prepare_cached(
                "SELECT key, value FROM kv WHERE namespace = ?1 AND key >= ?2 ORDER BY key",
            )?;
            let mut found = Vec::new();
            let mut rows = statement.query(params![self.namespace, prefix])?;
            while let Some(row) = rows.next()? {
                let key: String = row.get(0)?;
                if !key.starts_with(prefix) {
                    break;
                }
                found.push((key, row.get(1)?));
            }
            Ok(found)
        })?;
        Ok(rows)
    }

    pub fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, ModuleError> {
        self.get(key)?
            .map(|bytes| serde_json::from_slice(&bytes))
            .transpose()
            .map_err(|e| ModuleError::Failed(format!("`{key}` is not that JSON: {e}")))
    }

    pub fn set_json<T: Serialize>(&self, key: &str, value: &T) -> Result<(), ModuleError> {
        let bytes = serde_json::to_vec(value).map_err(|e| ModuleError::Failed(e.to_string()))?;
        self.set(key, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "regulus-store-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn version(path: &Path) -> u32 {
        current_version(&Connection::open(path).unwrap()).unwrap()
    }

    #[test]
    fn a_new_database_gets_every_migration_and_reopening_changes_nothing() {
        let dir = temp_dir("fresh");
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as u32);
        drop(store);
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as u32);
        let rows: u32 = store
            .dbs
            .global
            .with(|c| c.query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(rows, MIGRATIONS.len() as u32, "each step is recorded once");
    }

    #[test]
    fn migrations_only_run_forward_from_where_a_database_is() {
        let dir = temp_dir("forward");
        let path = dir.join("a.db");
        let config = StoreConfig::default();
        let first = ["CREATE TABLE one (x INTEGER)"];
        drop(Db::open_with(&path, &config, &first).unwrap());
        assert_eq!(version(&path), 1);
        // Step 1 is not run again: it would fail on the existing table.
        let both = [
            "CREATE TABLE one (x INTEGER)",
            "CREATE TABLE two (y INTEGER)",
        ];
        drop(Db::open_with(&path, &config, &both).unwrap());
        assert_eq!(version(&path), 2);
    }

    #[test]
    fn a_failed_step_leaves_the_database_at_the_last_good_version() {
        let dir = temp_dir("failed");
        let path = dir.join("a.db");
        let steps = ["CREATE TABLE one (x INTEGER)", "NOT SQL"];
        assert!(Db::open_with(&path, &StoreConfig::default(), &steps).is_err());
        assert_eq!(version(&path), 1);
    }

    #[test]
    fn a_database_from_a_newer_core_is_refused_untouched() {
        let dir = temp_dir("newer");
        let path = dir.join("a.db");
        let config = StoreConfig::default();
        drop(Db::open_with(&path, &config, &["SELECT 1", "SELECT 2"]).unwrap());
        let error = Db::open_with(&path, &config, &["SELECT 1"]).unwrap_err();
        assert!(
            matches!(
                error,
                StoreError::Newer {
                    found: 2,
                    known: 1,
                    ..
                }
            ),
            "{error}"
        );
        assert_eq!(version(&path), 2);
    }

    #[test]
    fn project_databases_are_found_on_open_and_ids_cannot_escape() {
        let dir = temp_dir("projects");
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        store.db(Some("alpha")).unwrap();
        assert!(store.db(Some("../evil")).is_err());
        assert!(store.db(Some("..")).is_err());
        drop(store);
        assert!(dir.join(PROJECTS_DIR).join("alpha.db").is_file());
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        assert_eq!(store.dbs.all().len(), 2);
    }

    fn execution(id: &str, project: Option<&str>, state: ExecutionState) -> Execution {
        Execution {
            id: id.into(),
            profile_id: "p".into(),
            workflow_id: "main".into(),
            project_id: project.map(String::from),
            state,
            disabled: Vec::new(),
            parent: None,
            caller: None,
            tag: None,
        }
    }

    fn workflow() -> Workflow {
        serde_json::from_value(json!({ "id": "main", "kind": "main" })).unwrap()
    }

    #[test]
    fn executions_round_trip_and_number_on_across_databases() {
        let dir = temp_dir("executions");
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        assert_eq!(store.first_execution(), 1);
        let mut one = execution("exec-1", None, ExecutionState::Running);
        store.insert_execution(1, &one, &workflow()).unwrap();
        let mut two = execution("exec-2", Some("proj"), ExecutionState::Starting);
        two.parent = Some("exec-1".into());
        store.insert_execution(2, &two, &workflow()).unwrap();
        one.state = ExecutionState::Failed {
            reason: "boom".into(),
        };
        store.update_execution(&one).unwrap();
        drop(store);

        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        assert_eq!(store.first_execution(), 3);
        let all: Vec<Execution> = store
            .executions(None)
            .unwrap()
            .into_iter()
            .map(|e| e.execution)
            .collect();
        assert_eq!(all, [one, two.clone()]);
        let in_project = store.executions(Some("proj")).unwrap();
        assert_eq!(in_project.len(), 1);
        let unfinished = store.left_running();
        assert_eq!(unfinished.len(), 1);
        assert_eq!(unfinished[0].execution, two);
        assert_eq!(unfinished[0].workflow, workflow());
        assert!(store.execution("exec-2").unwrap().is_some());
        assert!(store.execution("exec-9").unwrap().is_none());
        let finished: Option<i64> = store
            .dbs
            .global
            .with(|c| c.query_row("SELECT finished FROM executions", [], |r| r.get(0)))
            .unwrap();
        assert!(finished.is_some(), "a failed execution is finished");
    }

    #[test]
    fn only_the_latest_checkpoint_of_each_node_is_kept() {
        let dir = temp_dir("checkpoints");
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        let running = execution("exec-1", Some("proj"), ExecutionState::Running);
        store.insert_execution(1, &running, &workflow()).unwrap();
        let project = Some("proj");
        store.checkpoint(project, "exec-1", "a", &json!(1)).unwrap();
        store
            .checkpoint(project, "exec-1", "b", &json!("b1"))
            .unwrap();
        store.checkpoint(project, "exec-1", "a", &json!(2)).unwrap();
        let saved = store.checkpoints(project, "exec-1").unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved["a"], json!(2));
        assert_eq!(saved["b"], json!("b1"));
        let rows: u32 = store
            .db(project)
            .unwrap()
            .with(|c| c.query_row("SELECT COUNT(*) FROM checkpoints", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(rows, 2);
        // A checkpoint needs its execution.
        assert!(store.checkpoint(project, "exec-9", "a", &json!(0)).is_err());
    }

    fn event(seq: u64, execution: &str, project: Option<&str>) -> BusEvent {
        BusEvent {
            seq,
            at: seq * 10,
            execution_id: execution.into(),
            project_id: project.map(String::from),
            event: seq,
            from: PortRef {
                node_id: "a".into(),
                port_id: "out".into(),
            },
            capability: "text@1".into(),
            depth: 0,
            kind: regulus_proto::BusEventKind::Emitted {
                value: json!(format!("v{seq}")),
            },
        }
    }

    #[test]
    fn bus_events_are_kept_per_database_and_read_back_in_order() {
        let dir = temp_dir("events");
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        assert_eq!(store.first_seq(), 1);
        let written = [
            event(1, "exec-1", None),
            event(2, "exec-2", Some("p")),
            event(3, "exec-1", None),
            event(4, "exec-2", Some("p")),
        ];
        for e in &written {
            store.append_event(e).unwrap();
        }
        drop(store);
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        assert_eq!(store.first_seq(), 5);
        let all = store
            .events(&EventListParams::default(), u64::MAX, None)
            .unwrap();
        assert_eq!(all, written);
        let page = EventListParams {
            since: 1,
            limit: Some(2),
            ..EventListParams::default()
        };
        let seqs: Vec<u64> = store
            .events(&page, u64::MAX, page.limit)
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(seqs, [2, 3]);
        let of_project = EventListParams {
            project_id: Some("p".into()),
            ..EventListParams::default()
        };
        assert_eq!(
            store
                .events(&of_project, u64::MAX, of_project.limit)
                .unwrap(),
            [written[1].clone(), written[3].clone()]
        );
        let of_execution = EventListParams {
            execution_id: Some("exec-1".into()),
            ..EventListParams::default()
        };
        assert_eq!(
            store
                .events(&of_execution, u64::MAX, of_execution.limit)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn old_events_are_pruned_past_twice_the_limit() {
        let dir = temp_dir("prune");
        let config = StoreConfig {
            max_events: 3,
            ..StoreConfig::default()
        };
        let store = Store::open(&dir, &config).unwrap();
        for seq in 1..=5 {
            store.append_event(&event(seq, "exec-1", None)).unwrap();
        }
        store.flush();
        assert_eq!(
            store
                .events(&EventListParams::default(), u64::MAX, None)
                .unwrap()
                .len(),
            5
        );
        store.append_event(&event(6, "exec-1", None)).unwrap();
        store.flush();
        let left: Vec<u64> = store
            .events(&EventListParams::default(), u64::MAX, None)
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(left, [4, 5, 6]);
    }

    #[test]
    fn unreadable_rows_are_skipped_with_a_warning() {
        let dir = temp_dir("broken-rows");
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        let good = execution("exec-1", None, ExecutionState::Running);
        store.insert_execution(1, &good, &workflow()).unwrap();
        drop(store);
        let raw = Connection::open(dir.join(GLOBAL_FILE)).unwrap();
        raw.execute_batch(
            "INSERT INTO executions (id, number, profile_id, workflow_id, workflow, status,
                 disabled, started)
             VALUES ('exec-2', 2, 'p', 'main', '{broken', 'running', '[]', 0),
                    ('exec-3', 3, 'p', 'main', '{}', 'bogus', '[]', 0)",
        )
        .unwrap();
        drop(raw);
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        let left: Vec<String> = store
            .left_running()
            .into_iter()
            .map(|e| e.execution.id)
            .collect();
        assert_eq!(left, ["exec-1"]);
        assert_eq!(store.warnings().len(), 1, "{:?}", store.warnings());
        assert!(store.warnings()[0].contains("exec-2"));
        let listed = store.executions(None).unwrap();
        assert_eq!(listed.len(), 1, "the bogus status is left out too");
        assert_eq!(
            store.first_execution(),
            4,
            "broken rows still count for numbering"
        );
    }

    #[test]
    fn a_broken_project_database_is_skipped_not_fatal() {
        let dir = temp_dir("broken-project");
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        store.db(Some("good")).unwrap();
        drop(store);
        fs::write(
            dir.join(PROJECTS_DIR).join("bad.db"),
            b"not a database at all",
        )
        .unwrap();
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        assert_eq!(store.dbs.all().len(), 2, "global and `good`");
        assert_eq!(store.warnings().len(), 1);
        assert!(
            store.warnings()[0].contains("`bad`"),
            "{:?}",
            store.warnings()
        );
    }

    #[test]
    fn a_store_that_cannot_number_does_not_open() {
        let dir = temp_dir("no-numbers");
        drop(Store::open(&dir, &StoreConfig::default()).unwrap());
        let raw = Connection::open(dir.join(GLOBAL_FILE)).unwrap();
        raw.execute_batch("DROP TABLE events").unwrap();
        drop(raw);
        assert!(Store::open(&dir, &StoreConfig::default()).is_err());
    }

    #[test]
    fn a_module_sees_only_its_own_namespace() {
        let dir = temp_dir("kv");
        let store = Store::open(&dir, &StoreConfig::default()).unwrap();
        let a = store.module_store(None, "t/a").unwrap();
        let b = store.module_store(None, "t/b").unwrap();
        a.set("k/1", b"one").unwrap();
        a.set("k/2", b"two").unwrap();
        a.set("l/1", b"other").unwrap();
        b.set("k/1", b"theirs").unwrap();
        assert_eq!(a.get("k/1").unwrap().as_deref(), Some(&b"one"[..]));
        assert_eq!(b.get("k/1").unwrap().as_deref(), Some(&b"theirs"[..]));
        assert_eq!(b.get("k/2").unwrap(), None);
        let keys: Vec<String> = a.list("k/").unwrap().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["k/1", "k/2"]);
        assert_eq!(b.list("").unwrap().len(), 1);
        assert!(a.delete("k/1").unwrap());
        assert!(!a.delete("k/1").unwrap());
        assert_eq!(b.get("k/1").unwrap().as_deref(), Some(&b"theirs"[..]));
        a.set_json("n", &serde_json::json!({ "x": 1 })).unwrap();
        let n: serde_json::Value = a.get_json("n").unwrap().unwrap();
        assert_eq!(n["x"], 1);
        // A project's space is another database.
        let in_project = store.module_store(Some("p"), "t/a").unwrap();
        assert_eq!(in_project.get("k/2").unwrap(), None);
    }
}
