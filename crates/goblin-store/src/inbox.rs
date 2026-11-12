//! The notifications inbox: a bounded list with seen flags.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use goblin_ipc::types::{ChatId, Notice, NoticeKind, Route};
use serde::{Deserialize, Serialize};

use crate::fsutil::{quarantine, read_json, write_json_atomic};
use crate::StoreError;

/// Default maximum number of notices kept.
pub const DEFAULT_MAX_NOTICES: usize = 200;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    items: Vec<Notice>,
}

/// Fields of a new notice; the inbox assigns the id and the unseen flag.
#[derive(Debug, Clone)]
pub struct NewNotice {
    /// Creation time, Unix milliseconds.
    pub at: u64,
    /// Kind of notice.
    pub kind: NoticeKind,
    /// Short title.
    pub title: String,
    /// Optional body.
    pub body: Option<String>,
    /// Related chat.
    pub chat: Option<ChatId>,
    /// Deep link.
    pub link: Option<Route>,
}

/// Bounded notification inbox, persisted on [`save`](Inbox::save).
#[derive(Debug)]
pub struct Inbox {
    path: PathBuf,
    max: usize,
    next_id: u64,
    items: VecDeque<Notice>,
    dirty: bool,
}

impl Inbox {
    /// Loads the inbox at `path` keeping at most `max` notices. A corrupt file is moved aside.
    pub fn open(path: &Path, max: usize) -> Result<Self, StoreError> {
        let saved = match read_json::<Saved>(path) {
            Ok(s) => s.unwrap_or_default(),
            Err(StoreError::Corrupt { .. }) => {
                quarantine(path);
                Saved::default()
            }
            Err(e) => return Err(e),
        };
        let max = max.max(1);
        let mut items: VecDeque<Notice> = saved.items.into();
        while items.len() > max {
            items.pop_front();
        }
        let floor = items
            .iter()
            .filter_map(|n| n.id.strip_prefix("n-")?.parse::<u64>().ok())
            .max()
            .map_or(0, |m| m + 1);
        Ok(Self {
            path: path.to_owned(),
            max,
            next_id: saved.next_id.max(floor),
            items,
            dirty: false,
        })
    }

    /// Adds an unseen notice, evicting the oldest beyond the limit, and returns it.
    pub fn push(&mut self, n: NewNotice) -> Notice {
        let notice = Notice {
            id: format!("n-{}", self.next_id),
            at: n.at,
            kind: n.kind,
            title: n.title,
            body: n.body,
            chat: n.chat,
            link: n.link,
            seen: false,
        };
        self.next_id += 1;
        self.items.push_back(notice.clone());
        while self.items.len() > self.max {
            self.items.pop_front();
        }
        self.dirty = true;
        notice
    }

    /// Notices newest first, at most `limit` (all when `None`).
    pub fn list(&self, limit: Option<usize>) -> Vec<Notice> {
        self.items
            .iter()
            .rev()
            .take(limit.unwrap_or(usize::MAX))
            .cloned()
            .collect()
    }

    /// Number of unseen notices.
    pub fn unseen(&self) -> u32 {
        u32::try_from(self.items.iter().filter(|n| !n.seen).count()).unwrap_or(u32::MAX)
    }

    /// Marks the given notices (all when `None`) as seen; returns how many changed. Unknown ids
    /// are ignored.
    pub fn mark_seen(&mut self, ids: Option<&[String]>) -> usize {
        let mut changed = 0;
        for n in self.items.iter_mut().filter(|n| !n.seen) {
            if ids.map_or(true, |ids| ids.contains(&n.id)) {
                n.seen = true;
                changed += 1;
            }
        }
        self.dirty |= changed > 0;
        changed
    }

    /// Removes all notices.
    pub fn clear(&mut self) {
        self.dirty |= !self.items.is_empty();
        self.items.clear();
    }

    /// Writes the inbox atomically if it changed.
    pub fn save(&mut self) -> Result<(), StoreError> {
        if !self.dirty {
            return Ok(());
        }
        let saved = Saved {
            next_id: self.next_id,
            items: self.items.iter().cloned().collect(),
        };
        write_json_atomic(&self.path, &saved, 0o600)?;
        self.dirty = false;
        Ok(())
    }
}
