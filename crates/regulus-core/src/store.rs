//! The store (docs/02-architecture.md §6): SQLite, one database for what belongs to no
//! project and one per project. It keeps executions, their checkpoints, the bus journal,
//! and a key-value space per module. It never calls back into the core.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::config::{StoreConfig, Synchronous};
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
];

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
/// found on start or first used.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    config: StoreConfig,
    global: Arc<Db>,
    projects: Mutex<BTreeMap<String, Arc<Db>>>,
}

impl Store {
    /// Opens (or creates) the store in `dir`, with every project database already there,
    /// and migrates them all.
    pub fn open(dir: &Path, config: &StoreConfig) -> Result<Self, StoreError> {
        let global = Arc::new(Db::open(&dir.join(GLOBAL_FILE), config)?);
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
                projects.insert(id.to_string(), Arc::new(Db::open(&path, config)?));
            }
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            config: config.clone(),
            global,
            projects: Mutex::new(projects),
        })
    }

    fn projects(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Db>>> {
        self.projects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The database of `project`, or the global one; a project's is created on first use.
    pub(crate) fn db(&self, project: Option<&str>) -> Result<Arc<Db>, StoreError> {
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

    /// The schema version every database is at.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        self.global.schema_version()
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
        assert_eq!(store.projects().len(), 1);
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
