//! The box's own record of a conversation (docs/12-agents.md §5): what was asked, what
//! the agent answered and what happened in between, kept by Regulus whatever the engine
//! remembers. One conversation per key, in the module's store of the node's project, so
//! it outlives restarts; the box picks the key (`conversation` field). Without a store it
//! lives in memory.
//!
//! Layout under the conversation's key `K`: `K/meta`, the
//! counters and the native session id; `K/e/<seq>`, an entry; `K/a/<seq>`, an activity
//! event. Entries and activity share one sequence, so one cursor pages through both.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use regulus_contract_agent_engine::SessionEvent;
use regulus_core::ModuleStore;
use serde::{Deserialize, Serialize};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Who said an entry.
pub const USER: &str = "user";
pub const AGENT: &str = "agent";
pub const SYSTEM: &str = "system";

/// Something said in the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub at: u64,
    /// `user`, `agent` or `system` (what the box itself had to say: a failure, a warning).
    pub role: String,
    pub text: String,
    /// The box's turn, `t-<n>`, if it belongs to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    pub execution_id: String,
}

/// Something that happened in the engine's session during the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    pub seq: u64,
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    pub event: SessionEvent,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Meta {
    /// The last seq given.
    seq: u64,
    /// The last turn number given.
    turns: u64,
    /// The engine's own id of the last session, for a native resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    native_session_id: Option<String>,
}

pub struct Conversation {
    key: String,
    store: Option<ModuleStore>,
    /// How many entries, and how many activity events, are kept; the oldest go first.
    max: usize,
    meta: Meta,
    entries: VecDeque<Entry>,
    activity: VecDeque<Activity>,
}

/// One page of a conversation after a cursor.
pub struct Page {
    pub entries: Vec<Entry>,
    pub activity: Vec<Activity>,
    pub next: u64,
}

fn seq_key(seq: u64) -> String {
    format!("{seq:020}")
}

impl Conversation {
    /// The conversation at `key`, as the store has it, or a new one.
    pub fn load(key: String, store: Option<ModuleStore>, max: usize) -> Self {
        let mut conversation = Self {
            key,
            store,
            max: max.max(1),
            meta: Meta::default(),
            entries: VecDeque::new(),
            activity: VecDeque::new(),
        };
        if let Some(store) = &conversation.store {
            let meta = store.get_json(&format!("{}/meta", conversation.key));
            let entries = store.list(&format!("{}/e/", conversation.key));
            let activity = store.list(&format!("{}/a/", conversation.key));
            match (meta, entries, activity) {
                (Ok(meta), Ok(entries), Ok(activity)) => {
                    conversation.meta = meta.unwrap_or_default();
                    conversation.entries = entries
                        .iter()
                        .filter_map(|(_, bytes)| serde_json::from_slice(bytes).ok())
                        .collect();
                    conversation.activity = activity
                        .iter()
                        .filter_map(|(_, bytes)| serde_json::from_slice(bytes).ok())
                        .collect();
                }
                (meta, entries, activity) => {
                    let error = [meta.err(), entries.err(), activity.err()]
                        .into_iter()
                        .flatten()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join("; ");
                    eprintln!(
                        "regulus: agent: cannot read conversation `{}`: {error}",
                        conversation.key
                    );
                }
            }
        }
        conversation.trim();
        conversation
    }

    fn save<T: Serialize>(&self, key: &str, value: &T) {
        if let Some(store) = &self.store {
            if let Err(error) = store.set_json(key, value) {
                eprintln!("regulus: agent: cannot save `{key}`: {error}");
            }
        }
    }

    fn delete(&self, key: &str) {
        if let Some(store) = &self.store {
            if let Err(error) = store.delete(key) {
                eprintln!("regulus: agent: cannot delete `{key}`: {error}");
            }
        }
    }

    fn save_meta(&self) {
        self.save(&format!("{}/meta", self.key), &self.meta);
    }

    fn next_seq(&mut self) -> u64 {
        self.meta.seq += 1;
        self.meta.seq
    }

    fn trim(&mut self) {
        while self.entries.len() > self.max {
            if let Some(old) = self.entries.pop_front() {
                self.delete(&format!("{}/e/{}", self.key, seq_key(old.seq)));
            }
        }
        while self.activity.len() > self.max {
            if let Some(old) = self.activity.pop_front() {
                self.delete(&format!("{}/a/{}", self.key, seq_key(old.seq)));
            }
        }
    }

    /// A new turn id, `t-<n>`, unique in the conversation.
    pub fn next_turn(&mut self) -> String {
        self.meta.turns += 1;
        self.save_meta();
        format!("t-{}", self.meta.turns)
    }

    pub fn add_entry(&mut self, role: &str, text: &str, turn: Option<&str>, execution_id: &str) {
        let entry = Entry {
            seq: self.next_seq(),
            at: now_ms(),
            role: role.to_string(),
            text: text.to_string(),
            turn: turn.map(String::from),
            execution_id: execution_id.to_string(),
        };
        self.save(&format!("{}/e/{}", self.key, seq_key(entry.seq)), &entry);
        self.save_meta();
        self.entries.push_back(entry);
        self.trim();
    }

    pub fn add_activity(&mut self, turn: Option<&str>, event: SessionEvent) {
        let activity = Activity {
            seq: self.next_seq(),
            at: now_ms(),
            turn: turn.map(String::from),
            event,
        };
        self.save(
            &format!("{}/a/{}", self.key, seq_key(activity.seq)),
            &activity,
        );
        self.save_meta();
        self.activity.push_back(activity);
        self.trim();
    }

    pub fn native_session_id(&self) -> Option<String> {
        self.meta.native_session_id.clone()
    }

    pub fn set_native_session_id(&mut self, id: Option<String>) {
        if self.meta.native_session_id != id {
            self.meta.native_session_id = id;
            self.save_meta();
        }
    }

    /// Entries and activity after `since`, oldest first, at most `limit` of both together.
    pub fn page(&self, since: u64, limit: Option<usize>) -> Page {
        let mut seqs: Vec<u64> = self
            .entries
            .iter()
            .map(|e| e.seq)
            .chain(self.activity.iter().map(|a| a.seq))
            .filter(|seq| *seq > since)
            .collect();
        seqs.sort_unstable();
        if let Some(limit) = limit {
            seqs.truncate(limit);
        }
        let last = seqs.last().copied();
        let within = |seq: u64| seq > since && last.is_some_and(|last| seq <= last);
        Page {
            entries: self
                .entries
                .iter()
                .filter(|e| within(e.seq))
                .cloned()
                .collect(),
            activity: self
                .activity
                .iter()
                .filter(|a| within(a.seq))
                .cloned()
                .collect(),
            next: last.unwrap_or(since),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regulus_contract_agent_engine::SessionEventKind;

    fn event(seq: u64) -> SessionEvent {
        SessionEvent {
            seq,
            at: 0,
            turn: None,
            kind: SessionEventKind::TurnStarted,
        }
    }

    #[test]
    fn a_page_shares_one_cursor_and_the_oldest_go_first() {
        let mut conversation = Conversation::load("conv/w/a".into(), None, 2);
        let turn = conversation.next_turn();
        assert_eq!(turn, "t-1");
        conversation.add_entry(USER, "hi", Some(&turn), "exec-1");
        conversation.add_activity(Some(&turn), event(1));
        conversation.add_entry(AGENT, "hello", Some(&turn), "exec-1");
        conversation.add_entry(USER, "again", None, "exec-1");
        let page = conversation.page(0, None);
        let texts: Vec<&str> = page.entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["hello", "again"], "history_max drops the oldest");
        assert_eq!(page.activity.len(), 1);
        assert_eq!(page.next, 4);
        let first = conversation.page(0, Some(2));
        assert_eq!((first.entries.len(), first.activity.len()), (1, 1));
        assert_eq!(first.next, 3);
        let rest = conversation.page(first.next, None);
        assert_eq!(rest.entries[0].text, "again");
        assert_eq!(conversation.page(4, None).next, 4);
    }
}
