//! Bounded per-chat transcript cache and the list of known side chats.
//!
//! Each chat is one JSON file `<dir>/<chat>.json` (transcript, replay cursor and flags); the
//! side-chat list is `<dir>/index.json`. Changes stay in memory until [`ChatCache::flush`], which
//! rewrites only dirty files atomically, so a crash leaves each file old or new. Transcripts are
//! loaded lazily. Size is bounded by a message count and a byte budget per chat and by a
//! maximum number of side chats.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use goblin_ipc::types::{Chat, Message};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fsutil::{create_private_dir, quarantine, read_json, remove_file, write_atomic};
use crate::StoreError;

const INDEX: &str = "index";

/// Size limits of the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLimits {
    /// Messages kept per chat; the oldest are evicted first.
    pub max_messages_per_chat: usize,
    /// Serialized bytes kept per chat file; the oldest messages are dropped to fit.
    pub max_bytes_per_chat: usize,
    /// Side chats kept; the least recently active ones are forgotten first.
    pub max_side_chats: usize,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            max_messages_per_chat: 500,
            max_bytes_per_chat: 512 * 1024,
            max_side_chats: 64,
        }
    }
}

/// Where to resume the server's event stream for a chat. Both values are opaque to the store.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplayCursor {
    /// `after_stream_seq` for the chat subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_seq: Option<Value>,
    /// `after_chat_event_seq` for the chat subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_event_seq: Option<Value>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Transcript {
    #[serde(default)]
    messages: Vec<Message>,
    #[serde(default)]
    cursor: ReplayCursor,
    #[serde(default)]
    history_complete: bool,
    /// Lowest Muse history sequence number already merged in; older pages start below it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history_floor: Option<i64>,
    #[serde(skip)]
    dirty: bool,
}

/// A side chat known on this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideChat {
    /// Chat UUID.
    pub id: String,
    /// Display title.
    pub title: String,
    /// Creation time, Unix milliseconds.
    pub created_at: u64,
    /// Last activity, Unix milliseconds.
    pub updated_at: u64,
    /// Unread message count.
    pub unread: u32,
    /// Preview of the last message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_text: Option<String>,
}

/// Result of [`ChatCache::upsert`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    /// The message was new and appended.
    Appended,
    /// A message with the same id was replaced in place.
    Replaced,
}

/// One page of cached history (see [`ChatCache::page`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// Messages in chronological order.
    pub messages: Vec<Message>,
    /// Pass as `before` to get the next older page, when older messages are cached.
    pub next: Option<String>,
    /// No older messages exist locally and the full history has been fetched.
    pub complete: bool,
}

/// Validates a chat id for use as a file name: `main` or a UUID-like string.
pub fn validate_chat_id(id: &str) -> Result<(), StoreError> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id != INDEX
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(StoreError::InvalidId(id.to_owned()))
    }
}

/// The transcript cache of one profile.
#[derive(Debug)]
pub struct ChatCache {
    dir: PathBuf,
    limits: CacheLimits,
    loaded: HashMap<String, Transcript>,
    side: Vec<SideChat>,
    side_dirty: bool,
    removed: HashSet<String>,
}

impl ChatCache {
    /// Opens (creating if needed) the cache directory and reads the side-chat list. A corrupt
    /// list is moved aside and starts empty.
    pub fn open(dir: &Path, limits: CacheLimits) -> Result<Self, StoreError> {
        create_private_dir(dir)?;
        let index = dir.join(format!("{INDEX}.json"));
        let side = match read_json::<Vec<SideChat>>(&index) {
            Ok(v) => v.unwrap_or_default(),
            Err(StoreError::Corrupt { .. }) => {
                quarantine(&index);
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        let side = side
            .into_iter()
            .filter(|c| validate_chat_id(&c.id).is_ok() && c.id != "main")
            .collect();
        Ok(Self {
            dir: dir.to_owned(),
            limits,
            loaded: HashMap::new(),
            side,
            side_dirty: false,
            removed: HashSet::new(),
        })
    }

    fn path(&self, chat: &str) -> PathBuf {
        self.dir.join(format!("{chat}.json"))
    }

    fn transcript(&mut self, chat: &str) -> Result<&mut Transcript, StoreError> {
        validate_chat_id(chat)?;
        if !self.loaded.contains_key(chat) {
            let path = self.path(chat);
            let t = match read_json::<Transcript>(&path) {
                Ok(t) => t.unwrap_or_default(),
                Err(StoreError::Corrupt { .. }) => {
                    quarantine(&path);
                    Transcript::default()
                }
                Err(e) => return Err(e),
            };
            self.loaded.insert(chat.to_owned(), t);
        }
        self.loaded
            .get_mut(chat)
            .ok_or_else(|| StoreError::NotFound(format!("chat {chat}")))
    }

    /// Appends `message`, or replaces the cached message with the same id in place. The oldest
    /// messages are evicted beyond the limit (which also marks the history incomplete).
    pub fn upsert(&mut self, message: Message) -> Result<Upsert, StoreError> {
        let max = self.limits.max_messages_per_chat.max(1);
        let chat = message.chat.clone();
        let t = self.transcript(&chat)?;
        t.dirty = true;
        if let Some(slot) = t.messages.iter_mut().find(|m| m.id == message.id) {
            *slot = message;
            return Ok(Upsert::Replaced);
        }
        t.messages.push(message);
        if t.messages.len() > max {
            let excess = t.messages.len() - max;
            t.messages.drain(..excess);
            t.history_complete = false;
            t.history_floor = None;
        }
        Ok(Upsert::Appended)
    }

    /// Like [`upsert`](Self::upsert), but a new message is placed by its time (after the last
    /// message that is not newer) instead of at the end. For messages learned late from
    /// Muse's history, such as media fetched after the fact.
    pub fn upsert_by_time(&mut self, message: Message) -> Result<Upsert, StoreError> {
        let (chat, id, at) = (message.chat.clone(), message.id.clone(), message.at);
        let result = self.upsert(message)?;
        // A message already stored elsewhere (by an earlier version, or under an older time)
        // moves to its place too.
        let t = self.transcript(&chat)?;
        if let Some(from) = t.messages.iter().position(|m| m.id == id) {
            let m = t.messages.remove(from);
            let to = t
                .messages
                .iter()
                .rposition(|x| x.at <= at)
                .map_or(0, |i| i + 1);
            t.messages.insert(to, m);
        }
        Ok(result)
    }

    /// Appends streamed text to a cached message. Returns `false` when the message is not cached
    /// (the caller should fetch it).
    pub fn apply_delta(
        &mut self,
        chat: &str,
        message_id: &str,
        text_append: &str,
    ) -> Result<bool, StoreError> {
        let t = self.transcript(chat)?;
        match t.messages.iter_mut().find(|m| m.id == message_id) {
            Some(m) => {
                m.text.push_str(text_append);
                t.dirty = true;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// All cached messages of a chat, oldest first.
    pub fn messages(&mut self, chat: &str) -> Result<&[Message], StoreError> {
        Ok(&self.transcript(chat)?.messages)
    }

    /// A page of up to `limit` messages immediately older than message `before` (or the newest
    /// ones when `before` is `None`).
    pub fn page(
        &mut self,
        chat: &str,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Page, StoreError> {
        let t = self.transcript(chat)?;
        let end = match before {
            None => t.messages.len(),
            Some(id) => t
                .messages
                .iter()
                .position(|m| m.id == id)
                .ok_or_else(|| StoreError::NotFound(format!("message {id}")))?,
        };
        let start = end.saturating_sub(limit.max(1));
        let messages = t.messages[start..end].to_vec();
        let next = (start > 0)
            .then(|| messages.first().map(|m| m.id.clone()))
            .flatten();
        Ok(Page {
            messages,
            next,
            complete: start == 0 && t.history_complete,
        })
    }

    /// The lowest Muse history sequence number merged into this chat so far, if any.
    pub fn history_floor(&mut self, chat: &str) -> Result<Option<i64>, StoreError> {
        Ok(self.transcript(chat)?.history_floor)
    }

    /// Whether the chat holds as many messages as the cache keeps.
    pub fn full(&mut self, chat: &str) -> Result<bool, StoreError> {
        let max = self.limits.max_messages_per_chat.max(1);
        Ok(self.transcript(chat)?.messages.len() >= max)
    }

    /// Merges one page of Muse's older history (`(sequence number, message)` pairs) in front
    /// of what is cached, and records `floor`, the lowest sequence number that page covered.
    /// Messages already cached, or not older than the oldest cached one, are left out. When
    /// the cache has room for only some, the newest of them are kept and the floor stops
    /// there. Returns how many were added.
    pub fn add_older(
        &mut self,
        chat: &str,
        mut older: Vec<(i64, Message)>,
        floor: i64,
    ) -> Result<usize, StoreError> {
        let max = self.limits.max_messages_per_chat.max(1);
        let t = self.transcript(chat)?;
        let oldest_at = t.messages.first().map(|m| m.at);
        older.retain(|(_, m)| {
            oldest_at.map_or(true, |at| m.at < at) && !t.messages.iter().any(|x| x.id == m.id)
        });
        older.sort_by_key(|(seq, m)| (m.at, *seq));
        older.dedup_by(|a, b| a.1.id == b.1.id);
        let room = max.saturating_sub(t.messages.len());
        let mut floor = floor;
        if older.len() > room {
            older.drain(..older.len() - room);
            if let Some(seq) = older.iter().map(|(seq, _)| *seq).min() {
                floor = seq;
            }
        }
        let added = older.len();
        t.messages.splice(0..0, older.into_iter().map(|(_, m)| m));
        if t.history_floor != Some(floor) || added > 0 {
            t.history_floor = Some(floor);
            t.dirty = true;
        }
        Ok(added)
    }

    /// Records that the server's full history for the chat is now cached.
    pub fn mark_history_complete(&mut self, chat: &str) -> Result<(), StoreError> {
        let t = self.transcript(chat)?;
        t.history_complete = true;
        t.dirty = true;
        Ok(())
    }

    /// The saved replay cursor of a chat.
    pub fn cursor(&mut self, chat: &str) -> Result<ReplayCursor, StoreError> {
        Ok(self.transcript(chat)?.cursor.clone())
    }

    /// Saves the replay cursor of a chat.
    pub fn set_cursor(&mut self, chat: &str, cursor: ReplayCursor) -> Result<(), StoreError> {
        let t = self.transcript(chat)?;
        if t.cursor != cursor {
            t.cursor = cursor;
            t.dirty = true;
        }
        Ok(())
    }

    /// Side chats known locally.
    pub fn side_chats(&self) -> &[SideChat] {
        &self.side
    }

    /// Registers a side chat. If `id` is known it is returned unchanged. Beyond the limit the
    /// least recently active side chat (and its transcript) is forgotten.
    pub fn add_side_chat(
        &mut self,
        id: &str,
        title: &str,
        now: u64,
    ) -> Result<&SideChat, StoreError> {
        validate_chat_id(id)?;
        if id == "main" {
            return Err(StoreError::InvalidId(id.to_owned()));
        }
        if !self.side.iter().any(|c| c.id == id) {
            self.side.push(SideChat {
                id: id.to_owned(),
                title: title.to_owned(),
                created_at: now,
                updated_at: now,
                unread: 0,
                last_text: None,
            });
            self.side_dirty = true;
            while self.side.len() > self.limits.max_side_chats.max(1) {
                let oldest = self
                    .side
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.id != id)
                    .min_by_key(|(_, c)| c.updated_at)
                    .map(|(i, _)| i);
                match oldest {
                    Some(i) => {
                        let gone = self.side.remove(i);
                        self.drop_transcript(&gone.id);
                    }
                    None => break,
                }
            }
        }
        self.side
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| StoreError::NotFound(format!("chat {id}")))
    }

    fn drop_transcript(&mut self, chat: &str) {
        self.loaded.remove(chat);
        self.removed.insert(chat.to_owned());
    }

    /// Renames a side chat.
    pub fn rename_side_chat(&mut self, id: &str, title: &str) -> Result<(), StoreError> {
        let c = self.side_mut(id)?;
        c.title = title.to_owned();
        self.side_dirty = true;
        Ok(())
    }

    fn side_mut(&mut self, id: &str) -> Result<&mut SideChat, StoreError> {
        self.side
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| StoreError::NotFound(format!("chat {id}")))
    }

    /// Removes a side chat and its cached transcript from the local list only.
    pub fn forget(&mut self, id: &str) -> Result<(), StoreError> {
        validate_chat_id(id)?;
        let before = self.side.len();
        self.side.retain(|c| c.id != id);
        if self.side.len() == before {
            return Err(StoreError::NotFound(format!("chat {id}")));
        }
        self.side_dirty = true;
        self.drop_transcript(id);
        Ok(())
    }

    /// Updates the preview, activity time and unread count of a side chat after a message.
    pub fn note_activity(
        &mut self,
        id: &str,
        last_text: &str,
        at: u64,
        unread_delta: u32,
    ) -> Result<(), StoreError> {
        let c = self.side_mut(id)?;
        c.last_text = Some(last_text.chars().take(120).collect());
        c.updated_at = c.updated_at.max(at);
        c.unread = c.unread.saturating_add(unread_delta);
        self.side_dirty = true;
        Ok(())
    }

    /// Sets the unread count of a side chat.
    pub fn set_unread(&mut self, id: &str, unread: u32) -> Result<(), StoreError> {
        let c = self.side_mut(id)?;
        if c.unread != unread {
            c.unread = unread;
            self.side_dirty = true;
        }
        Ok(())
    }

    /// The chat list for `chat.list`: `main` (derived from its transcript; `main_unread` is
    /// supplied by the caller) followed by side chats, most recently active first.
    pub fn chat_list(
        &mut self,
        main_unread: u32,
        pending_turns: impl Fn(&str) -> u32,
    ) -> Result<Vec<Chat>, StoreError> {
        let last = self.transcript("main")?.messages.last().cloned();
        let mut out = vec![Chat {
            id: "main".into(),
            title: "Muse".into(),
            last_text: last.as_ref().map(|m| m.text.chars().take(120).collect()),
            last_at: last.as_ref().map(|m| m.at),
            unread: main_unread,
            pending_turns: pending_turns("main"),
        }];
        let mut side: Vec<&SideChat> = self.side.iter().collect();
        side.sort_by_key(|c| std::cmp::Reverse(c.updated_at));
        out.extend(side.into_iter().map(|c| Chat {
            id: c.id.clone(),
            title: c.title.clone(),
            last_text: c.last_text.clone(),
            last_at: Some(c.updated_at),
            unread: c.unread,
            pending_turns: pending_turns(&c.id),
        }));
        Ok(out)
    }

    /// Whether unsaved changes exist.
    pub fn is_dirty(&self) -> bool {
        self.side_dirty || !self.removed.is_empty() || self.loaded.values().any(|t| t.dirty)
    }

    /// Writes all dirty files atomically and deletes files of forgotten chats. Transcripts over
    /// the byte budget lose their oldest messages first.
    pub fn flush(&mut self) -> Result<(), StoreError> {
        for chat in std::mem::take(&mut self.removed) {
            remove_file(&self.path(&chat))?;
        }
        let budget = self.limits.max_bytes_per_chat;
        let dir = self.dir.clone();
        for (chat, t) in self.loaded.iter_mut().filter(|(_, t)| t.dirty) {
            let mut bytes =
                serde_json::to_vec(&*t).map_err(|e| StoreError::corrupt(&dir, e.to_string()))?;
            while bytes.len() > budget && t.messages.len() > 1 {
                let drop = (t.messages.len() / 10).max(1);
                t.messages.drain(..drop);
                t.history_complete = false;
                bytes = serde_json::to_vec(&*t)
                    .map_err(|e| StoreError::corrupt(&dir, e.to_string()))?;
            }
            write_atomic(&dir.join(format!("{chat}.json")), &bytes, 0o600)?;
            t.dirty = false;
        }
        if self.side_dirty {
            let bytes = serde_json::to_vec(&self.side)
                .map_err(|e| StoreError::corrupt(&self.dir, e.to_string()))?;
            write_atomic(&self.dir.join(format!("{INDEX}.json")), &bytes, 0o600)?;
            self.side_dirty = false;
        }
        Ok(())
    }
}
