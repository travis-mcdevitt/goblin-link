//! The device command log and the command consent toggles.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use goblin_ipc::types::{CommandEntry, CommandState, Consent, ConsentUpdate};
use serde::{Deserialize, Serialize};

use crate::fsutil::{quarantine, read_json, write_json_atomic};
use crate::StoreError;

/// Longest summary kept for a command, in characters.
pub const MAX_SUMMARY_CHARS: usize = 120;
/// Default number of log entries kept.
pub const DEFAULT_MAX_ENTRIES: usize = 100;

/// Turns arbitrary command text into a one-line summary of at most 120 characters: control
/// characters and line breaks become spaces, runs of whitespace collapse, and a long text is cut
/// with a trailing `…`.
pub fn summarize(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_SUMMARY_CHARS {
        collapsed
    } else {
        let mut cut: String = collapsed.chars().take(MAX_SUMMARY_CHARS - 1).collect();
        cut.push('…');
        cut
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    entries: Vec<CommandEntry>,
}

/// Bounded ring of what Muse ran on this device. Only name, a short summary, state and duration
/// are kept; arguments and output are never stored.
#[derive(Debug)]
pub struct CommandLog {
    path: Option<PathBuf>,
    max: usize,
    next_id: u64,
    entries: VecDeque<CommandEntry>,
    dirty: bool,
}

impl CommandLog {
    /// A log that is never persisted.
    pub fn in_memory(max: usize) -> Self {
        Self {
            path: None,
            max: max.max(1),
            next_id: 0,
            entries: VecDeque::new(),
            dirty: false,
        }
    }

    /// Loads the log at `path`. Entries still `running` from a previous run are marked `error`
    /// because the daemon that ran them is gone.
    pub fn open(path: &Path, max: usize) -> Result<Self, StoreError> {
        let saved = match read_json::<Saved>(path) {
            Ok(s) => s.unwrap_or_default(),
            Err(StoreError::Corrupt { .. }) => {
                quarantine(path);
                Saved::default()
            }
            Err(e) => return Err(e),
        };
        let mut log = Self::in_memory(max);
        log.path = Some(path.to_owned());
        log.next_id = saved.next_id;
        for mut e in saved.entries {
            if e.state == CommandState::Running {
                e.state = CommandState::Error;
                log.dirty = true;
            }
            log.entries.push_back(e);
        }
        while log.entries.len() > log.max {
            log.entries.pop_front();
        }
        Ok(log)
    }

    /// Records a command that is starting; `summary` is shortened with [`summarize`].
    pub fn start(&mut self, name: &str, summary: &str, at: u64) -> CommandEntry {
        let entry = CommandEntry {
            id: format!("c-{}", self.next_id),
            at,
            name: name.to_owned(),
            summary: summarize(summary),
            state: CommandState::Running,
            ms: None,
        };
        self.next_id += 1;
        self.entries.push_back(entry.clone());
        while self.entries.len() > self.max {
            self.entries.pop_front();
        }
        self.dirty = true;
        entry
    }

    /// Records a command that was refused by the consent toggles (no running phase).
    pub fn refused(&mut self, name: &str, summary: &str, at: u64) -> CommandEntry {
        let mut e = self.start(name, summary, at);
        if let Some(stored) = self.entries.back_mut() {
            stored.state = CommandState::Refused;
        }
        e.state = CommandState::Refused;
        e
    }

    /// Completes entry `id` with its final state and duration. `None` if it was already evicted.
    pub fn finish(&mut self, id: &str, state: CommandState, ms: u64) -> Option<CommandEntry> {
        let e = self.entries.iter_mut().find(|e| e.id == id)?;
        e.state = state;
        e.ms = Some(ms);
        self.dirty = true;
        Some(e.clone())
    }

    /// Entries newest first, at most `limit` (all when `None`).
    pub fn entries(&self, limit: Option<usize>) -> Vec<CommandEntry> {
        self.entries
            .iter()
            .rev()
            .take(limit.unwrap_or(usize::MAX))
            .cloned()
            .collect()
    }

    /// Number of commands currently running.
    pub fn running(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.state == CommandState::Running)
            .count()
    }

    /// Writes the log atomically if it is persistent and changed.
    pub fn save(&mut self) -> Result<(), StoreError> {
        let (true, Some(path)) = (self.dirty, self.path.as_ref()) else {
            return Ok(());
        };
        let saved = Saved {
            next_id: self.next_id,
            entries: self.entries.iter().cloned().collect(),
        };
        write_json_atomic(path, &saved, 0o600)?;
        self.dirty = false;
        Ok(())
    }
}

/// Persistent consent toggles (`shell`, `file_read`, `file_write`, `drop_ins`); default all on.
#[derive(Debug)]
pub struct ConsentStore {
    path: PathBuf,
    current: Consent,
}

impl ConsentStore {
    /// Loads the toggles. A corrupt file is moved aside and the defaults (all on) apply.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let current = match read_json::<Consent>(path) {
            Ok(c) => c.unwrap_or_default(),
            Err(StoreError::Corrupt { .. }) => {
                quarantine(path);
                Consent::default()
            }
            Err(e) => return Err(e),
        };
        Ok(Self {
            path: path.to_owned(),
            current,
        })
    }

    /// The current toggles.
    pub fn get(&self) -> Consent {
        self.current
    }

    /// Applies a partial update, persists it, and returns the new toggles. Nothing changes if
    /// the write fails.
    pub fn update(&mut self, update: &ConsentUpdate) -> Result<Consent, StoreError> {
        let mut next = self.current;
        next.apply(update);
        if next != self.current {
            write_json_atomic(&self.path, &next, 0o644)?;
            self.current = next;
        }
        Ok(self.current)
    }
}
