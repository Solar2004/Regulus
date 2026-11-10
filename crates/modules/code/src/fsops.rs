//! The file operations of a workspace: list, read, search, write and edit. Each one finds
//! its path through `Root::locate`, so none leaves the root, and each has a limit on what
//! it gives back or takes in. Nothing here runs a program.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use regex::{Regex, RegexBuilder};

use crate::contract::{Entry, Kind, Match};
use crate::place::{Place, Root};
use crate::Fault;

/// How much of a file is looked at to tell text from binary.
const SNIFF_BYTES: usize = 8192;
/// What marks a line that was cut.
const CUT_MARK: char = '…';

/// An I/O error on the path an operation was asked for.
fn gone(asked: &str, error: io::Error) -> Fault {
    match error.kind() {
        io::ErrorKind::NotFound => {
            Fault::Request(format!("there is no `{asked}` in the workspace"))
        }
        _ => Fault::Failed(format!("`{asked}`: {error}")),
    }
}

fn broken(asked: &str, error: io::Error) -> Fault {
    Fault::Failed(format!("`{asked}`: {error}"))
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(SNIFF_BYTES).any(|byte| *byte == 0)
}

/// `bytes` as text. `cut`: they stop in the middle of the file, so a character split at
/// the very end is dropped instead of refusing the file.
fn text_of(bytes: Vec<u8>, cut: bool, asked: &str) -> Result<String, Fault> {
    if bytes.contains(&0) {
        return Err(Fault::Request(format!(
            "`{asked}` is not a text file (it has binary content)"
        )));
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(error) => {
            let utf8 = error.utf8_error();
            if cut && utf8.error_len().is_none() {
                let valid = utf8.valid_up_to();
                let mut bytes = error.into_bytes();
                bytes.truncate(valid);
                return Ok(String::from_utf8(bytes).unwrap_or_default());
            }
            Err(Fault::Request(format!("`{asked}` is not UTF-8 text")))
        }
    }
}

/// `line` without its end, within `max` characters.
fn shorten(line: &str, max: usize) -> String {
    let line = line.trim_end();
    match line.char_indices().nth(max) {
        Some((at, _)) => format!("{}{CUT_MARK}", &line[..at]),
        None => line.to_string(),
    }
}

/// An entry of a folder, with what a walk needs to go on.
struct Child {
    entry: Entry,
    name: String,
    real: PathBuf,
    /// Reached through a link: shown, never entered.
    link: bool,
}

/// The entries of `dir` operations may see, by path, and whether it has more than the
/// `cap` that were looked at (a folder is never read without an end). A link counts as
/// what it leads to when that is inside the workspace; one that leaves it, a protected
/// name, a name that is not text and anything that is neither a file nor a folder are
/// left out.
fn children(root: &Root, dir: &Path, rel: &str, cap: usize) -> io::Result<(Vec<Child>, bool)> {
    let mut found = Vec::new();
    let mut more = false;
    for item in fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let Some(name) = item.file_name().to_str().map(String::from) else {
            continue;
        };
        if root.is_protected(&name) {
            continue;
        }
        let Ok(kind) = item.file_type() else { continue };
        let (real, link) = if kind.is_symlink() {
            let Ok(real) = item.path().canonicalize() else {
                continue;
            };
            if root.relative(&real).is_none() {
                continue;
            }
            (real, true)
        } else {
            (item.path(), false)
        };
        let Ok(meta) = fs::metadata(&real) else {
            continue;
        };
        let (kind, size) = if meta.is_dir() {
            (Kind::Directory, None)
        } else if meta.is_file() {
            (Kind::File, Some(meta.len()))
        } else {
            continue;
        };
        if found.len() >= cap {
            more = true;
            break;
        }
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        found.push(Child {
            entry: Entry { path, kind, size },
            name,
            real,
            link,
        });
    }
    found.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
    Ok((found, more))
}

/// A file an operation was asked for: there, inside, and a regular file. A pipe or a
/// device would never finish reading.
fn file_at(root: &Root, asked: &str) -> Result<(Place, fs::Metadata), Fault> {
    let place = root.locate(asked)?;
    let meta = fs::metadata(&place.real).map_err(|e| gone(asked, e))?;
    if meta.is_dir() {
        return Err(Fault::Request(format!(
            "`{asked}` is a folder, not a file: list it instead"
        )));
    }
    if !meta.is_file() {
        return Err(Fault::Request(format!("`{asked}` is not a regular file")));
    }
    Ok((place, meta))
}

#[derive(Debug)]
pub(crate) struct Listed {
    pub path: String,
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

/// The entries of the folder `asked`, `depth` levels down (1: its own), at most `max`,
/// nearest levels first, looking at no more than `folder_cap` entries of any one folder.
/// Folders named in `skip` are listed but not entered.
pub(crate) fn list(
    root: &Root,
    skip: &[String],
    asked: &str,
    depth: usize,
    max: usize,
    folder_cap: usize,
) -> Result<Listed, Fault> {
    let place = root.locate(asked)?;
    let meta = fs::metadata(&place.real).map_err(|e| gone(asked, e))?;
    if !meta.is_dir() {
        return Err(Fault::Request(format!(
            "`{asked}` is a file, not a folder: read it instead"
        )));
    }
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut queue = VecDeque::from([(place.real.clone(), place.rel.clone(), 1usize)]);
    'folders: while let Some((dir, rel, level)) = queue.pop_front() {
        let (found, more) = match children(root, &dir, &rel, folder_cap) {
            Ok(found) => found,
            // The folder asked for must be readable; one further down is only left out.
            Err(error) if level == 1 => return Err(broken(asked, error)),
            Err(_) => continue,
        };
        truncated |= more;
        for child in found {
            if entries.len() >= max {
                truncated = true;
                break 'folders;
            }
            let enter = child.entry.kind == Kind::Directory
                && level < depth
                && !child.link
                && !skip.contains(&child.name);
            if enter {
                queue.push_back((child.real, child.entry.path.clone(), level + 1));
            }
            entries.push(child.entry);
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Listed {
        path: place.rel,
        entries,
        truncated,
    })
}

#[derive(Debug)]
pub(crate) struct Head {
    pub path: String,
    pub text: String,
    pub truncated: bool,
    pub size: u64,
}

/// The file `asked` from its start, at most `max` bytes of it.
pub(crate) fn read_head(root: &Root, asked: &str, max: usize) -> Result<Head, Fault> {
    let (place, meta) = file_at(root, asked)?;
    let file = File::open(&place.real).map_err(|e| gone(asked, e))?;
    let mut bytes = Vec::new();
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| broken(asked, e))?;
    let truncated = bytes.len() > max;
    bytes.truncate(max);
    Ok(Head {
        path: place.rel,
        text: text_of(bytes, truncated, asked)?,
        truncated,
        size: meta.len(),
    })
}

/// Reads one line into `into`, keeping at most `keep` bytes of it, so a file that is one
/// endless line costs no memory. `None` at the end of the file; otherwise whether the
/// line was longer than what was kept.
fn bounded_line(
    reader: &mut impl BufRead,
    keep: usize,
    into: &mut Vec<u8>,
) -> io::Result<Option<bool>> {
    into.clear();
    let mut any = false;
    let mut cut = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return Ok(any.then_some(cut));
        }
        any = true;
        let (chunk, ended) = match available.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&available[..=at], true),
            None => (available, false),
        };
        let room = keep.saturating_sub(into.len());
        if chunk.len() > room {
            cut = true;
        }
        into.extend_from_slice(&chunk[..chunk.len().min(room)]);
        let used = chunk.len();
        reader.consume(used);
        if ended {
            return Ok(Some(cut));
        }
    }
}

#[derive(Debug)]
pub(crate) struct Lines {
    pub path: String,
    pub text: String,
    /// The first line in `text`, from 1.
    pub start_line: u64,
    /// The last line in `text`; one less than `start_line` when there is none.
    pub end_line: u64,
    /// The file goes on after `end_line`, or its last line here was cut.
    pub truncated: bool,
    pub size: u64,
}

/// Lines of the file `asked` from line `first` (from 1): at most `limit` of them and
/// `max` bytes, whole lines only. A single line longer than `max` comes cut.
pub(crate) fn read_lines(
    root: &Root,
    asked: &str,
    first: u64,
    limit: Option<u64>,
    max: usize,
) -> Result<Lines, Fault> {
    let (place, meta) = file_at(root, asked)?;
    let file = File::open(&place.real).map_err(|e| gone(asked, e))?;
    let mut reader = BufReader::new(file);
    let first = first.max(1);
    let (mut number, mut taken) = (0u64, 0u64);
    let mut text = Vec::new();
    let mut line = Vec::new();
    let mut truncated = false;
    let mut cut_line = false;
    loop {
        let wanted = number + 1 >= first;
        let full = limit.is_some_and(|limit| taken >= limit) || (taken > 0 && text.len() >= max);
        if wanted && full {
            truncated = !reader.fill_buf().map_err(|e| broken(asked, e))?.is_empty();
            break;
        }
        let room = if wanted {
            max.saturating_sub(text.len())
        } else {
            0
        };
        let Some(cut) = bounded_line(&mut reader, room, &mut line).map_err(|e| broken(asked, e))?
        else {
            break;
        };
        number += 1;
        if !wanted {
            continue;
        }
        if cut && taken > 0 {
            // It does not fit after the others: it is the first line of the next read.
            truncated = true;
            break;
        }
        text.extend_from_slice(&line);
        taken += 1;
        if cut {
            truncated = true;
            cut_line = true;
            break;
        }
    }
    Ok(Lines {
        path: place.rel,
        text: text_of(text, cut_line, asked)?,
        start_line: first,
        end_line: first + taken - 1,
        truncated,
        size: meta.len(),
    })
}

/// What to look for.
#[derive(Debug, Default)]
pub(crate) struct Query<'a> {
    pub text: &'a str,
    /// `text` is a regular expression instead of the text itself.
    pub regex: bool,
    pub ignore_case: bool,
    /// Only files whose path, relative to the root, matches.
    pub glob: Option<&'a str>,
    /// The folder, or the one file, to look in.
    pub path: &'a str,
}

/// Where a search stops.
#[derive(Debug, Clone)]
pub(crate) struct Bounds {
    pub matches: usize,
    pub line_chars: usize,
    pub files: usize,
    pub file_bytes: u64,
    pub time: Duration,
    /// Levels of folders it goes down below where it starts.
    pub depth: usize,
    /// Entries of any one folder it looks at.
    pub folder_entries: usize,
}

#[derive(Debug, Default)]
pub(crate) struct Hits {
    pub matches: Vec<Match>,
    pub truncated: bool,
}

struct Search<'a> {
    root: &'a Root,
    skip: &'a [String],
    matcher: Regex,
    glob: Option<glob::Pattern>,
    bounds: &'a Bounds,
    deadline: Instant,
    seen: usize,
    hits: Hits,
}

impl Search<'_> {
    /// Looks through a folder, in path order. False: the search is over. Whatever it does
    /// not look at (out of time, too deep, too many entries) leaves the answer `truncated`.
    fn folder(&mut self, dir: &Path, rel: &str, depth: usize) -> bool {
        // Checked per folder too: a tree of folders with no file in it has an end as well.
        if Instant::now() >= self.deadline {
            self.hits.truncated = true;
            return false;
        }
        let Ok((found, more)) = children(self.root, dir, rel, self.bounds.folder_entries) else {
            return true;
        };
        self.hits.truncated |= more;
        for child in found {
            // Links are not followed: a loop would never end, and one file is one match.
            if child.link {
                continue;
            }
            let more = match child.entry.kind {
                Kind::Directory if self.skip.contains(&child.name) => true,
                Kind::Directory if depth >= self.bounds.depth => {
                    self.hits.truncated = true;
                    true
                }
                Kind::Directory => self.folder(&child.real, &child.entry.path, depth + 1),
                Kind::File => self.file(
                    &child.real,
                    &child.entry.path,
                    child.entry.size.unwrap_or(0),
                ),
            };
            if !more {
                return false;
            }
        }
        true
    }

    /// Looks through one file. False: the search is over.
    fn file(&mut self, real: &Path, rel: &str, size: u64) -> bool {
        if self.seen >= self.bounds.files || Instant::now() >= self.deadline {
            self.hits.truncated = true;
            return false;
        }
        self.seen += 1;
        if self.glob.as_ref().is_some_and(|glob| !glob.matches(rel))
            || size > self.bounds.file_bytes
        {
            return true;
        }
        let Ok(bytes) = fs::read(real) else {
            return true;
        };
        if is_binary(&bytes) {
            return true;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (index, line) in text.lines().enumerate() {
            if !self.matcher.is_match(line) {
                continue;
            }
            if self.hits.matches.len() >= self.bounds.matches {
                self.hits.truncated = true;
                return false;
            }
            self.hits.matches.push(Match {
                path: rel.to_string(),
                line: index as u64 + 1,
                text: shorten(line, self.bounds.line_chars),
            });
        }
        true
    }
}

/// The lines that match `query`, in path order then line order, within `bounds`. Binary
/// files, files over `bounds.file_bytes`, links and the folders of `skip` are passed by.
pub(crate) fn search(
    root: &Root,
    skip: &[String],
    query: &Query,
    bounds: &Bounds,
) -> Result<Hits, Fault> {
    if query.text.is_empty() {
        return Err(Fault::Request("`query` is empty".into()));
    }
    let pattern = if query.regex {
        query.text.to_string()
    } else {
        regex::escape(query.text)
    };
    let matcher = RegexBuilder::new(&pattern)
        .case_insensitive(query.ignore_case)
        .build()
        .map_err(|e| Fault::Request(format!("`query` is not a regular expression: {e}")))?;
    let glob = match query.glob.map(str::trim).filter(|glob| !glob.is_empty()) {
        Some(glob) => Some(
            glob::Pattern::new(glob)
                .map_err(|e| Fault::Request(format!("`glob` is not a pattern: {e}")))?,
        ),
        None => None,
    };
    let place = root.locate(query.path)?;
    let meta = fs::metadata(&place.real).map_err(|e| gone(query.path, e))?;
    let mut search = Search {
        root,
        skip,
        matcher,
        glob,
        bounds,
        deadline: Instant::now() + bounds.time,
        seen: 0,
        hits: Hits::default(),
    };
    if meta.is_dir() {
        search.folder(&place.real, &place.rel, 0);
    } else if meta.is_file() {
        search.file(&place.real, &place.rel, meta.len());
    } else {
        return Err(Fault::Request(format!(
            "`{}` is not a regular file",
            query.path
        )));
    }
    Ok(search.hits)
}

/// Numbers the temporary files of this process.
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// One change at a time, in the whole process. An edit reads a file, changes its text and
/// puts it back: two at once on one file (two agents, an agent and its worker, two
/// workspace nodes over one folder) would each put back a file without the other's
/// change. Held from the read to the rename, and never while a value is emitted. A
/// program outside Regulus that writes the same file meanwhile is not covered.
static CHANGING: Mutex<()> = Mutex::new(());

fn changing() -> MutexGuard<'static, ()> {
    CHANGING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Puts `bytes` at `target` in one step: written beside it and renamed over it, so a
/// reader never sees half a file and a failure leaves what was there.
fn replace(target: &Path, bytes: &[u8], keep: Option<fs::Permissions>) -> io::Result<()> {
    let folder = target
        .parent()
        .ok_or_else(|| io::Error::other("no folder to write in"))?;
    let temp = folder.join(format!(
        ".regulus-code-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        // `create_new` never follows a link and never reuses a file that is there.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        if let Some(permissions) = keep {
            file.set_permissions(permissions)?;
        }
        drop(file);
        fs::rename(&temp, target)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

#[derive(Debug)]
pub(crate) struct Written {
    pub path: String,
    pub bytes: usize,
    /// The file was not there before.
    pub created: bool,
}

/// Writes `content` as the whole file `asked`, creating it and the folders above it.
pub(crate) fn write(root: &Root, asked: &str, content: &str, max: usize) -> Result<Written, Fault> {
    if content.len() > max {
        return Err(Fault::Request(format!(
            "the content is {} bytes; this workspace writes files of at most {max} (`max_write_bytes`)",
            content.len()
        )));
    }
    let place = root.locate(asked)?;
    if place.rel.is_empty() {
        return Err(Fault::Request(
            "`path` names the workspace folder itself, not a file in it".into(),
        ));
    }
    let _changing = changing();
    let existing = fs::metadata(&place.real).ok();
    if existing.as_ref().is_some_and(|meta| meta.is_dir()) {
        return Err(Fault::Request(format!("`{asked}` is a folder")));
    }
    if existing.as_ref().is_some_and(|meta| !meta.is_file()) {
        return Err(Fault::Request(format!("`{asked}` is not a regular file")));
    }
    let folder = place
        .real
        .parent()
        .ok_or_else(|| Fault::Request(format!("`{asked}` has no folder")))?;
    fs::create_dir_all(folder).map_err(|e| broken(asked, e))?;
    // The folders just made must still be inside: a link put there meanwhile is refused.
    let inside = folder
        .canonicalize()
        .ok()
        .is_some_and(|real| root.relative(&real).is_some());
    if !inside {
        return Err(Fault::Request(format!(
            "`{asked}` is outside the workspace"
        )));
    }
    let keep = existing.as_ref().map(fs::Metadata::permissions);
    replace(&place.real, content.as_bytes(), keep).map_err(|e| broken(asked, e))?;
    Ok(Written {
        path: place.rel,
        bytes: content.len(),
        created: existing.is_none(),
    })
}

#[derive(Debug)]
pub(crate) struct Edited {
    pub path: String,
    pub replacements: usize,
    pub bytes: usize,
}

/// Replaces `old` with `new` in the file `asked`. `old` must be in the file exactly, and
/// exactly once unless `all`: anything else is a conflict that changes nothing, so an
/// edit made from a stale read never lands in the wrong place.
pub(crate) fn edit(
    root: &Root,
    asked: &str,
    old: &str,
    new: &str,
    all: bool,
    max: usize,
) -> Result<Edited, Fault> {
    if old.is_empty() {
        return Err(Fault::Request(
            "`old_text` is empty: to create a file or replace all of one, write it instead".into(),
        ));
    }
    if old == new {
        return Err(Fault::Request(
            "`old_text` and `new_text` are the same: there is nothing to change".into(),
        ));
    }
    let _changing = changing();
    let (place, meta) = file_at(root, asked)?;
    if meta.len() > max as u64 {
        return Err(Fault::Request(format!(
            "`{asked}` is {} bytes; this workspace edits files of at most {max} (`max_write_bytes`)",
            meta.len()
        )));
    }
    let bytes = fs::read(&place.real).map_err(|e| gone(asked, e))?;
    let text = text_of(bytes, false, asked)?;
    let found = text.matches(old).count();
    if found == 0 {
        return Err(Fault::Request(format!(
            "conflict: `old_text` is not in `{asked}`. It must match the file exactly, spaces and line ends included: read the file again and copy the text from it"
        )));
    }
    if found > 1 && !all {
        return Err(Fault::Request(format!(
            "conflict: `old_text` is in `{asked}` {found} times. Add the lines around it until it matches once, or set `replace_all`"
        )));
    }
    let updated = if all {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    };
    if updated.len() > max {
        return Err(Fault::Request(format!(
            "the edited file would be {} bytes; this workspace writes files of at most {max} (`max_write_bytes`)",
            updated.len()
        )));
    }
    replace(&place.real, updated.as_bytes(), Some(meta.permissions()))
        .map_err(|e| broken(asked, e))?;
    Ok(Edited {
        path: place.rel,
        replacements: if all { found } else { 1 },
        bytes: updated.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("regulus-code-fsops-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn put(home: &Path, path: &str, text: &str) {
        let file = home.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    fn bounds() -> Bounds {
        Bounds {
            matches: 100,
            line_chars: 100,
            files: 100,
            file_bytes: 1 << 20,
            time: Duration::from_secs(30),
            depth: 8,
            folder_entries: 100,
        }
    }

    fn needle() -> Query<'static> {
        Query {
            text: "needle",
            ..Query::default()
        }
    }

    fn found(hits: &Hits) -> Vec<&str> {
        hits.matches.iter().map(|m| m.path.as_str()).collect()
    }

    #[test]
    fn a_search_out_of_time_stops_in_a_tree_of_folders_too() {
        let home = temp("expired");
        fs::create_dir_all(home.join("a/b/c/d")).unwrap();
        fs::create_dir_all(home.join("e/f")).unwrap();
        let root = Root::open(home.to_str().unwrap(), Vec::new()).unwrap();
        // No file anywhere: the clock has to be read at the folders.
        let expired = Bounds {
            time: Duration::ZERO,
            ..bounds()
        };
        let hits = search(&root, &[], &needle(), &expired).unwrap();
        assert!(hits.truncated, "an expired search says it did not finish");
        assert!(hits.matches.is_empty());
        let whole = search(&root, &[], &needle(), &bounds()).unwrap();
        assert!(
            !whole.truncated,
            "with time, the same tree is searched whole"
        );
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn a_search_says_when_it_did_not_go_deeper() {
        let home = temp("depth");
        for file in ["top.txt", "a/one.txt", "a/b/two.txt", "a/b/c/three.txt"] {
            put(&home, file, "a needle\n");
        }
        let root = Root::open(home.to_str().unwrap(), Vec::new()).unwrap();
        let down = |depth: usize| {
            let bounds = Bounds { depth, ..bounds() };
            search(&root, &[], &needle(), &bounds).unwrap()
        };
        let none = down(0);
        assert_eq!(found(&none), ["top.txt"]);
        assert!(
            none.truncated,
            "a folder it did not enter is a search cut short"
        );
        let one = down(1);
        assert_eq!(found(&one), ["a/one.txt", "top.txt"]);
        assert!(one.truncated);
        let all = down(3);
        assert_eq!(
            found(&all),
            ["a/b/c/three.txt", "a/b/two.txt", "a/one.txt", "top.txt"]
        );
        assert!(!all.truncated);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn a_folder_is_looked_at_up_to_its_cap() {
        let home = temp("cap");
        for n in 0..10 {
            put(&home, &format!("f{n}.txt"), "a needle\n");
        }
        let root = Root::open(home.to_str().unwrap(), Vec::new()).unwrap();
        let capped = Bounds {
            folder_entries: 4,
            ..bounds()
        };
        let hits = search(&root, &[], &needle(), &capped).unwrap();
        assert_eq!((hits.matches.len(), hits.truncated), (4, true));
        let listed = list(&root, &[], "", 1, 100, 4).unwrap();
        assert_eq!((listed.entries.len(), listed.truncated), (4, true));
        let whole = list(&root, &[], "", 1, 100, 100).unwrap();
        assert_eq!((whole.entries.len(), whole.truncated), (10, false));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn a_line_is_read_within_what_is_kept() {
        let mut reader = BufReader::with_capacity(4, &b"abcdefgh\nij\n\nlast"[..]);
        let mut line = Vec::new();
        assert_eq!(bounded_line(&mut reader, 3, &mut line).unwrap(), Some(true));
        assert_eq!(line, b"abc", "the rest of the line is passed over");
        assert_eq!(
            bounded_line(&mut reader, 10, &mut line).unwrap(),
            Some(false)
        );
        assert_eq!(line, b"ij\n");
        assert_eq!(bounded_line(&mut reader, 0, &mut line).unwrap(), Some(true));
        assert!(line.is_empty(), "nothing kept, the line still counts");
        assert_eq!(
            bounded_line(&mut reader, 10, &mut line).unwrap(),
            Some(false)
        );
        assert_eq!(line, b"last", "a last line without its end");
        assert_eq!(bounded_line(&mut reader, 10, &mut line).unwrap(), None);
    }

    #[test]
    fn text_is_utf8_without_binary_content() {
        assert_eq!(text_of(b"hola".to_vec(), false, "f").unwrap(), "hola");
        assert!(text_of(vec![b'a', 0, b'b'], false, "f").is_err());
        assert!(text_of(vec![0xff, b'a'], false, "f").is_err());
        // `é` split by a cut: dropped when the bytes stop mid-file, refused otherwise.
        let split = vec![b'a', 0xc3];
        assert_eq!(text_of(split.clone(), true, "f").unwrap(), "a");
        assert!(text_of(split, false, "f").is_err());
    }

    #[test]
    fn a_match_shows_its_line_within_the_limit() {
        assert_eq!(shorten("short  \r", 10), "short");
        assert_eq!(shorten("abcdef", 3), "abc…");
        assert_eq!(shorten("añö", 2), "añ…");
    }
}
