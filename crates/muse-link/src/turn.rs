//! Following one request through Muse's chat event stream.
//!
//! A faithful port of `muse_turn.py`. Create a [`MuseTurn`] before sending the
//! request, [`MuseTurn::acknowledge`] it with the `send_chat`/`send_voice`
//! response, and feed it every event from [`crate::LinkSession::subscribe_chat`].
//! It returns typed [`TurnEvent`]s for this request only.
//!
//! # Time
//!
//! The reference reads `time.monotonic()` inside its event handlers. This port
//! takes the current monotonic time in **seconds** as a `now` argument on the
//! methods that record activity ([`MuseTurn::feed`], [`MuseTurn::acknowledge`])
//! and on [`MuseTurn::complete`]/[`MuseTurn::settled`]. This is an API shape
//! change only; the correlation and quiet-timeout semantics are identical, and
//! it makes the logic deterministically testable.

use std::collections::{BTreeSet, HashSet, VecDeque};

use serde_json::{Map, Value};

/// Silence (s) after which a turn without a terminal task status counts as answered.
pub const REPLY_QUIET_S: f64 = 3.0;
/// Silence (s) after which an unanswered follow-up stops holding the turn open.
pub const DETAIL_REPLY_QUIET_S: f64 = 30.0;
/// Wait (s) after a terminal task with blank replies, for reordered text.
pub const EMPTY_REPLY_GRACE_S: f64 = 1.0;

const ACTIVITY_CODES: &[&str] = &[
    "working",
    "responding",
    "idle",
    "online",
    "thinking",
    "researching",
    "planning",
    "listening",
    "speaking",
    "processing",
    "generating",
    "executing",
];
const TERMINAL_TASK_STATUSES: &[&str] =
    &["completed", "failed", "errored", "cancelled", "canceled"];
const STATUS_EVENTS: &[&str] = &["agent.status", "task.status"];
const MESSAGE_EVENTS: &[&str] = &[
    "delta.message_start",
    "delta.text_append",
    "delta.message_done",
    "message.assistant",
];

const MAX_ID_BYTES: usize = 512;
const MAX_PENDING_EVENTS: usize = 64;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_MESSAGES: usize = 32;
const MAX_OBSERVED_IDS: usize = 256;
const MAX_IGNORED_MESSAGES: usize = 256;

/// An error from Muse's events exceeding this turn's size bounds (the reference
/// raises `ValueError` for these; they end the subscription).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TurnError(pub &'static str);

/// Muse's public status for this request; either field is `None` when absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatus {
    /// Activity code (e.g. `working`), when present.
    pub activity_code: Option<String>,
    /// Human-readable activity text with control characters stripped.
    pub activity_text: Option<String>,
}

/// One assistant message answering the request, as it stands after the latest event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// Assistant message id.
    pub message_id: String,
    /// Best current text: streamed deltas, replaced by Muse's final text.
    pub text: String,
    /// Native text choices attached to this reply.
    pub choices: Vec<String>,
    /// Text suitable for display, with widget references resolved.
    pub display_text: String,
    /// The streamed deltas alone, in arrival order.
    pub streamed: String,
    /// Whether this message is complete.
    pub done: bool,
    /// Whether the message is proven to answer this request.
    pub bound: bool,
}

impl Reply {
    fn new(message_id: String) -> Self {
        Self {
            message_id,
            text: String::new(),
            choices: Vec::new(),
            display_text: String::new(),
            streamed: String::new(),
            done: false,
            bound: false,
        }
    }
}

/// Muse reported the request's task terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskFinished;

/// What one event means for this request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    /// Agent status update.
    Status(AgentStatus),
    /// An assistant reply changed.
    Reply(Reply),
    /// The request's task finished.
    Finished(TaskFinished),
}

/// Kind of a chat identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IdKind {
    /// A message id.
    Message,
    /// A task id.
    Task,
}

type Id = (IdKind, String);

fn strip_controls(text: &str) -> String {
    text.chars()
        .filter(|&c| {
            let n = c as u32;
            !(n <= 0x08 || (0x0b..=0x1f).contains(&n) || (0x7f..=0x9f).contains(&n))
        })
        .collect()
}

fn as_object(event: &Value) -> Option<&Map<String, Value>> {
    event.as_object()
}

fn get_str<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(Value::as_str)
}

/// An identifier field. Muse sends absent ids as `""` (observed live for
/// `reply_to_message_id`), and the reference's `a or b` treats them as missing;
/// so must we, or an empty parent reads as an unknown one and the reply is dropped.
fn get_id<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    get_str(obj, key).filter(|s| !s.is_empty())
}

fn text_byte_len(s: &str) -> usize {
    s.len()
}

/// Chat ids retired by earlier turns on one connection, bounded to `limit`.
///
/// Share one scope across the turns of a gadget that owns its chat: events
/// carrying a retired id are replays and never reach a later turn.
#[derive(Debug)]
pub struct ReplayScope {
    limit: usize,
    ids: HashSet<Id>,
    order: VecDeque<Id>,
}

impl Default for ReplayScope {
    fn default() -> Self {
        Self::new(2048)
    }
}

impl ReplayScope {
    /// A scope keeping at most `limit` retired ids.
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            ids: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    /// The ids an event carries. With `include_parents`, parent references too.
    pub fn event_ids(event: &Value, include_parents: bool) -> Result<HashSet<Id>, TurnError> {
        let mut result = HashSet::new();
        let Some(obj) = as_object(event) else {
            return Ok(result);
        };
        if let Some(payload) = obj.get("payload").and_then(Value::as_object) {
            let mut keys: Vec<(&str, IdKind)> = vec![
                ("message_id", IdKind::Message),
                ("id", IdKind::Message),
                ("task_id", IdKind::Task),
            ];
            if include_parents {
                keys.push(("reply_to_message_id", IdKind::Message));
                keys.push(("parent_message_id", IdKind::Message));
            }
            for (key, kind) in keys {
                if let Some(value) = get_str(payload, key) {
                    if !value.is_empty() {
                        if value.len() > MAX_ID_BYTES {
                            return Err(TurnError("Muse returned an oversized chat identifier"));
                        }
                        result.insert((kind, value.to_owned()));
                    }
                }
            }
        }
        if let Some(value) = get_id(obj, "message_id") {
            if !value.is_empty() {
                if value.len() > MAX_ID_BYTES {
                    return Err(TurnError("Muse returned an oversized chat identifier"));
                }
                result.insert((IdKind::Message, value.to_owned()));
            }
        }
        Ok(result)
    }

    /// Retires the given ids.
    pub fn retire(&mut self, ids: impl IntoIterator<Item = Id>) {
        for id in ids {
            if self.ids.insert(id.clone()) {
                self.order.push_back(id);
            }
        }
        while self.order.len() > self.limit {
            if let Some(old) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
    }

    /// Retires the user message of a request no turn will follow.
    pub fn retire_request(&mut self, response: &Value) -> Result<(), TurnError> {
        let ids = acknowledged_message_ids(response)?;
        self.retire(ids.into_iter().map(|v| (IdKind::Message, v)));
        Ok(())
    }

    /// Retires the ids of an event seen on `session_id` while no turn was open.
    pub fn observe_idle(&mut self, event: &Value, session_id: Option<&str>) {
        let Some(obj) = as_object(event) else {
            return;
        };
        let Some(payload) = obj.get("payload").and_then(Value::as_object) else {
            return;
        };
        let Some(session_id) = session_id else {
            return;
        };
        let event_session = get_id(payload, "session_id").or_else(|| get_id(obj, "session_id"));
        let name = get_str(obj, "event").unwrap_or("");
        let is_relevant = STATUS_EVENTS.contains(&name) || MESSAGE_EVENTS.contains(&name);
        if get_str(obj, "type") == Some("event") && event_session == Some(session_id) && is_relevant
        {
            if let Ok(ids) = Self::event_ids(event, true) {
                self.retire(ids);
            }
        }
    }
}

/// The acknowledged user message ids in a chat acknowledgement.
pub fn acknowledged_message_ids(response: &Value) -> Result<BTreeSet<String>, TurnError> {
    let result = response
        .get("result")
        .filter(|v| v.is_object())
        .unwrap_or(response);
    let obj = result
        .as_object()
        .ok_or(TurnError("Muse returned an invalid chat acknowledgement"))?;
    let mut ids = BTreeSet::new();
    for key in ["message_id", "reply_to_message_id"] {
        if let Some(v) = get_str(obj, key) {
            if !v.is_empty() {
                ids.insert(v.to_owned());
            }
        }
    }
    if ids.is_empty() {
        return Err(TurnError(
            "Muse chat acknowledgement omitted the user message ID",
        ));
    }
    Ok(ids)
}

/// Reads only assistant text content from Muse's structured transcript.
pub fn transcript_text(value: &Value, message_id: Option<&str>) -> String {
    if let Some(s) = value.as_str() {
        return s.to_owned();
    }
    let Some(obj) = value.as_object() else {
        return String::new();
    };
    let Some(messages) = obj.get("messages").and_then(Value::as_array) else {
        return String::new();
    };
    let mut assistants: Vec<&Value> = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .collect();
    if let Some(mid) = message_id {
        assistants.retain(|m| m.get("id").and_then(Value::as_str) == Some(mid));
    }
    let Some(last) = assistants.last() else {
        return String::new();
    };
    let Some(content) = last.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    let texts: Vec<&str> = content
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect();
    texts.join("\n")
}

fn update_reply(
    reply: &Reply,
    name: &str,
    payload: &Map<String, Value>,
) -> Result<Reply, TurnError> {
    let mut text = reply.text.clone();
    let mut streamed = reply.streamed.clone();
    let mut done = reply.done;
    if name == "delta.text_append" && !done {
        if let Some(delta) = get_str(payload, "text") {
            if !delta.is_empty() {
                let delta = strip_controls(delta);
                text.push_str(&delta);
                streamed.push_str(&delta);
            }
        }
    }
    if name == "delta.message_done" || name == "message.assistant" {
        let mut completed_text: Option<String> =
            get_str(payload, "display_text").map(str::to_owned);
        if completed_text.is_none() {
            let transcript = payload.get("transcript");
            match transcript {
                Some(Value::String(s)) => completed_text = Some(s.clone()),
                Some(v) => {
                    let t = transcript_text(v, Some(&reply.message_id));
                    if !t.is_empty() {
                        completed_text = Some(t);
                    } else {
                        // transcript present but not a string and yielded nothing.
                        completed_text = get_str(payload, "content").map(str::to_owned);
                    }
                }
                None => {
                    completed_text = get_str(payload, "content").map(str::to_owned);
                }
            }
        }
        if let Some(ct) = completed_text {
            text = strip_controls(&ct);
        }
        if name == "delta.message_done"
            || payload.get("display_text_ready") != Some(&Value::Bool(false))
        {
            done = true;
        }
    }
    if text_byte_len(&text) > MAX_TEXT_BYTES || text_byte_len(&streamed) > MAX_TEXT_BYTES {
        return Err(TurnError("Muse returned more than 64 KiB of reply text"));
    }
    let has_widgets = payload.contains_key("widgets")
        || payload
            .get("payload")
            .is_some_and(|p| p.get("widgets").is_some());
    let resolved = if !has_widgets && text == reply.text {
        crate::choices::Choices {
            text: reply.display_text.clone(),
            replies: reply.choices.clone(),
        }
    } else {
        crate::choices::decode(&text, &Value::Object(payload.clone()))
    };
    Ok(Reply {
        message_id: reply.message_id.clone(),
        text,
        display_text: resolved.text,
        choices: resolved.replies,
        streamed,
        done,
        bound: reply.bound,
    })
}

/// Muse's work on one request, bound to the request's acknowledged user message.
pub struct MuseTurn {
    /// The chat the request goes to; `None` learns it from the acknowledgement.
    pub session_id: Option<String>,
    /// Whether the gadget owns that chat.
    pub owns_chat: bool,
    /// The request's user message is known, so events bind as they arrive.
    pub acknowledged: bool,
    /// Muse reports work in progress on the request.
    pub busy: bool,
    /// Muse reported the request's task terminal and no other task of it runs.
    pub task_finished: bool,
    /// Muse's latest agent activity code, or `"other"` for an unknown code.
    pub activity_code: Option<String>,
    /// Monotonic time (s) of the last event that changed this turn.
    pub last_activity: f64,

    replay_scope: Option<ReplayScope>,
    ignore_parentless_until: f64,
    user_ids: HashSet<String>,
    replies: Vec<(String, Reply)>,
    pending: Vec<Value>,
    pending_text_bytes: usize,
    observed_ids: HashSet<Id>,
    running_task_ids: HashSet<String>,
    unanswered_follow_up_ids: HashSet<String>,
    follow_up_task_ids: HashSet<String>,
    ignored_messages: VecDeque<String>,
}

impl MuseTurn {
    /// A new turn targeting `session_id` (or `None` to learn it on acknowledge).
    pub fn new(session_id: Option<String>) -> Self {
        Self::with_options(session_id, false, None, 0.0, 0.0)
    }

    /// Full constructor.
    ///
    /// `owns_chat` means the gadget owns the chat, so every event on it belongs
    /// to the current turn and the acknowledgement must confirm the chat as a
    /// thread; `replay_scope` then filters replays of earlier turns. Assistant
    /// messages without a parent that first appear before `ignore_parentless_until`
    /// (a monotonic-seconds value) are ignored. `now` seeds `last_activity`.
    pub fn with_options(
        session_id: Option<String>,
        owns_chat: bool,
        replay_scope: Option<ReplayScope>,
        ignore_parentless_until: f64,
        now: f64,
    ) -> Self {
        Self {
            session_id,
            owns_chat,
            acknowledged: false,
            busy: false,
            task_finished: false,
            activity_code: None,
            last_activity: now,
            replay_scope,
            ignore_parentless_until,
            user_ids: HashSet::new(),
            replies: Vec::new(),
            pending: Vec::new(),
            pending_text_bytes: 0,
            observed_ids: HashSet::new(),
            running_task_ids: HashSet::new(),
            unanswered_follow_up_ids: HashSet::new(),
            follow_up_task_ids: HashSet::new(),
            ignored_messages: VecDeque::new(),
        }
    }

    /// Takes back ownership of the replay scope (e.g. to share it again).
    pub fn take_replay_scope(&mut self) -> Option<ReplayScope> {
        self.replay_scope.take()
    }

    /// Every assistant message bound to the request, by message id, in arrival order.
    pub fn replies(&self) -> impl Iterator<Item = &Reply> {
        self.replies.iter().map(|(_, r)| r)
    }

    /// Whether `id` is a user message this turn acknowledged.
    pub fn is_user_message(&self, id: &str) -> bool {
        self.user_ids.contains(id)
    }

    /// Whether `id` is a reply this turn is tracking.
    pub fn has_reply(&self, id: &str) -> bool {
        self.reply_get(id).is_some()
    }

    fn reply_get(&self, id: &str) -> Option<&Reply> {
        self.replies.iter().find(|(k, _)| k == id).map(|(_, r)| r)
    }

    fn reply_set(&mut self, id: String, reply: Reply) {
        if let Some(slot) = self.replies.iter_mut().find(|(k, _)| *k == id) {
            slot.1 = reply;
        } else {
            self.replies.push((id, reply));
        }
    }

    /// Binds the turn to the user message in Muse's acknowledgement; returns held events.
    pub fn acknowledge(
        &mut self,
        response: &Value,
        follow_up: bool,
        now: f64,
    ) -> Result<Vec<TurnEvent>, TurnError> {
        let user_ids = acknowledged_message_ids(response)?;
        if self.owns_chat && user_ids.iter().any(|v| v.len() > MAX_ID_BYTES) {
            return Err(TurnError("Muse returned an oversized chat identifier"));
        }
        let result = response
            .get("result")
            .filter(|v| v.is_object())
            .unwrap_or(response);
        if follow_up {
            for id in &user_ids {
                self.user_ids.insert(id.clone());
                self.unanswered_follow_up_ids.insert(id.clone());
            }
            self.task_finished = false;
            self.last_activity = now;
        } else {
            self.user_ids = user_ids.iter().cloned().collect();
        }
        let acknowledged_session = result.get("session_id").and_then(Value::as_str);
        if self.owns_chat
            && (self.session_id.is_none()
                || acknowledged_session != self.session_id.as_deref()
                || result.get("is_thread") != Some(&Value::Bool(true)))
        {
            return Err(TurnError(
                "Muse did not acknowledge the gadget's dedicated side chat",
            ));
        }
        if self.session_id.is_none() {
            if let Some(s) = acknowledged_session {
                self.session_id = Some(s.to_owned());
            }
        }
        self.acknowledged = true;
        let pending = std::mem::take(&mut self.pending);
        self.pending_text_bytes = 0;
        let mut held = Vec::new();
        for event in pending {
            held.extend(self.feed(&event, now)?);
        }
        Ok(held)
    }

    /// Holds events until [`MuseTurn::acknowledge`] binds one more user message.
    pub fn expect_follow_up(&mut self) {
        self.acknowledged = false;
    }

    /// Reads one raw chat event; returns what it means for this request.
    pub fn feed(&mut self, event: &Value, now: f64) -> Result<Vec<TurnEvent>, TurnError> {
        let Some(obj) = as_object(event) else {
            return Ok(vec![]);
        };
        if get_str(obj, "type") != Some("event") {
            return Ok(vec![]);
        }
        let Some(payload) = obj.get("payload").and_then(Value::as_object) else {
            return Ok(vec![]);
        };
        let event_session = get_id(payload, "session_id")
            .or_else(|| get_id(obj, "session_id"))
            .map(str::to_owned);

        if self.owns_chat {
            match (&self.session_id, &event_session) {
                (Some(s), Some(es)) if s == es => {}
                _ => return Ok(vec![]),
            }
            let identifiers = ReplayScope::event_ids(event, true)?;
            if let Some(scope) = &self.replay_scope {
                let own_ids = ReplayScope::event_ids(event, false)?;
                if own_ids.intersection(&scope.ids).next().is_some() {
                    return Ok(vec![]);
                }
                let parents: HashSet<Id> = identifiers.difference(&own_ids).cloned().collect();
                let authorized: HashSet<Id> = self
                    .user_ids
                    .iter()
                    .map(|v| (IdKind::Message, v.clone()))
                    .collect();
                let unauthorized: HashSet<Id> = parents.difference(&authorized).cloned().collect();
                if self.acknowledged && unauthorized.intersection(&scope.ids).next().is_some() {
                    return Ok(vec![]);
                }
            }
            for id in identifiers {
                self.observed_ids.insert(id);
            }
            if self.observed_ids.len() > MAX_OBSERVED_IDS {
                return Err(TurnError(
                    "Muse returned too many activity identifiers for one turn",
                ));
            }
        }

        if let (Some(es), Some(s)) = (&event_session, &self.session_id) {
            if es != s {
                return Ok(vec![]);
            }
        }

        if !self.acknowledged {
            if self.pending.len() >= MAX_PENDING_EVENTS {
                return Err(TurnError(
                    "too many chat events before Muse acknowledged the turn",
                ));
            }
            for key in ["text", "display_text", "content", "transcript"] {
                if let Some(s) = get_str(payload, key) {
                    self.pending_text_bytes += text_byte_len(s);
                }
            }
            if self.pending_text_bytes > MAX_TEXT_BYTES {
                return Err(TurnError(
                    "Muse returned more than 64 KiB of pending reply text",
                ));
            }
            self.pending.push(event.clone());
            return Ok(vec![]);
        }

        let scoped = self.owns_chat
            && self.session_id.is_some()
            && event_session.as_deref() == self.session_id.as_deref();
        let name = get_str(obj, "event").unwrap_or("").to_owned();
        if STATUS_EVENTS.contains(&name.as_str()) {
            return self.status(&name, payload, scoped, now);
        }
        if MESSAGE_EVENTS.contains(&name.as_str()) {
            let payload = payload.clone();
            return self.message(&name, obj, &payload, scoped, now);
        }
        Ok(vec![])
    }

    /// Stop waiting for one reply (e.g. one the gadget cannot read): counts as done.
    pub fn abandon_reply(&mut self, message_id: &str) {
        if let Some(slot) = self.replies.iter_mut().find(|(k, _)| k == message_id) {
            slot.1.done = true;
        }
    }

    /// Muse is done answering: every reply is done and Muse is idle, finished or quiet.
    pub fn complete(&self, now: f64) -> bool {
        self.acknowledged
            && !self.busy
            && (self.unanswered_follow_up_ids.is_empty()
                || now - self.last_activity >= DETAIL_REPLY_QUIET_S)
            && self.replies.iter().all(|(_, r)| r.done)
            && (self.task_finished || now - self.last_activity >= REPLY_QUIET_S)
    }

    /// Muse finished the task, every reply is done, and no late text arrived in the grace period.
    pub fn settled(&self, now: f64) -> bool {
        let grace = if self.replies.is_empty() {
            REPLY_QUIET_S
        } else {
            EMPTY_REPLY_GRACE_S
        };
        self.acknowledged
            && self.task_finished
            && !self.busy
            && self.unanswered_follow_up_ids.is_empty()
            && self.replies.iter().all(|(_, r)| r.done)
            && now - self.last_activity >= grace
    }

    /// Retires every id this turn saw into its replay scope, once the turn is over.
    pub fn retire(&mut self) {
        if !self.owns_chat {
            return;
        }
        let Some(mut scope) = self.replay_scope.take() else {
            return;
        };
        scope.retire(self.observed_ids.iter().cloned());
        scope.retire(self.user_ids.iter().map(|v| (IdKind::Message, v.clone())));
        for event in &self.pending {
            if let Ok(ids) = ReplayScope::event_ids(event, true) {
                scope.retire(ids);
            }
        }
        self.replay_scope = Some(scope);
    }

    fn status(
        &mut self,
        name: &str,
        payload: &Map<String, Value>,
        scoped: bool,
        now: f64,
    ) -> Result<Vec<TurnEvent>, TurnError> {
        let activity = get_str(payload, "activity_code");
        let status = get_str(payload, "status");
        let parent =
            get_id(payload, "reply_to_message_id").or_else(|| get_id(payload, "parent_message_id"));
        let message_id = get_id(payload, "message_id");
        let reply_bound = message_id.and_then(|m| self.reply_get(m)).map(|r| r.bound);
        let parent_reply_bound = parent.and_then(|p| self.reply_get(p)).map(|r| r.bound);
        let parent_linked =
            parent.is_some_and(|p| self.user_ids.contains(p)) || parent_reply_bound == Some(true);
        if !scoped && parent.is_some() && !parent_linked {
            return Ok(vec![]);
        }
        let explicitly_linked = parent_linked || reply_bound == Some(true);
        let terminal = status.is_some_and(|s| TERMINAL_TASK_STATUSES.contains(&s));
        let was_finished = self.task_finished;
        let task_id = get_id(payload, "task_id").map(str::to_owned);

        if self.owns_chat && name == "task.status" {
            if terminal
                && task_id
                    .as_deref()
                    .map(|t| !self.running_task_ids.contains(t))
                    .unwrap_or(true)
                && !explicitly_linked
            {
                return Ok(vec![]);
            }
            if status == Some("running") {
                if let Some(tid) = &task_id {
                    if !self.unanswered_follow_up_ids.is_empty()
                        && !self.running_task_ids.contains(tid)
                    {
                        self.follow_up_task_ids.insert(tid.clone());
                    }
                    self.running_task_ids.insert(tid.clone());
                }
            }
            if terminal {
                if let Some(tid) = &task_id {
                    self.running_task_ids.remove(tid);
                }
            }
        }

        if terminal {
            let parent_is_follow_up =
                parent.is_some_and(|p| self.unanswered_follow_up_ids.contains(p));
            let task_is_follow_up = task_id
                .as_deref()
                .is_some_and(|t| self.follow_up_task_ids.contains(t));
            if parent_is_follow_up || task_is_follow_up {
                if let Some(p) = parent {
                    self.unanswered_follow_up_ids.remove(p);
                }
                if task_is_follow_up {
                    self.unanswered_follow_up_ids.clear();
                }
            }
        }

        if name == "task.status" && status.is_some() {
            self.task_finished = terminal && !(self.owns_chat && !self.running_task_ids.is_empty());
        }

        if let Some(activity) = activity {
            self.busy = activity != "online"
                && activity != "idle"
                && !activity.is_empty()
                && !TERMINAL_TASK_STATUSES.contains(&activity);
            if name == "agent.status" {
                self.activity_code = Some(if ACTIVITY_CODES.contains(&activity) {
                    activity.to_owned()
                } else {
                    "other".to_owned()
                });
            }
        } else if let Some(status) = status {
            self.busy = !status.is_empty() && status != "idle" && !terminal;
        }

        if self.owns_chat && !self.running_task_ids.is_empty() {
            self.task_finished = false;
            self.busy = true;
        } else if name == "task.status" && self.task_finished {
            self.busy = false;
        }

        self.last_activity = now;

        if name == "agent.status" && (scoped || explicitly_linked) && !self.task_finished {
            let text = get_str(payload, "activity_text").map(strip_controls);
            return Ok(vec![TurnEvent::Status(AgentStatus {
                activity_code: activity.map(str::to_owned),
                activity_text: text,
            })]);
        }
        if self.task_finished && !was_finished {
            Ok(vec![TurnEvent::Finished(TaskFinished)])
        } else {
            Ok(vec![])
        }
    }

    fn message(
        &mut self,
        name: &str,
        event: &Map<String, Value>,
        payload: &Map<String, Value>,
        scoped: bool,
        now: f64,
    ) -> Result<Vec<TurnEvent>, TurnError> {
        let message_id = get_id(payload, "message_id")
            .or_else(|| get_id(event, "message_id"))
            .or_else(|| get_str(payload, "id"));
        let Some(message_id) = message_id else {
            return Ok(vec![]);
        };
        if message_id.is_empty() || get_str(payload, "role") == Some("user") {
            return Ok(vec![]);
        }
        let message_id = message_id.to_owned();
        let parent = get_id(payload, "reply_to_message_id")
            .or_else(|| get_id(payload, "parent_message_id"))
            .map(str::to_owned);
        let have_reply = self.reply_get(&message_id).is_some();

        if let Some(p) = &parent {
            let parent_unknown = !self.user_ids.contains(p)
                && self.reply_get(p).is_none()
                && !(scoped && *p == message_id);
            if parent_unknown {
                if !have_reply {
                    self.ignore(&message_id);
                }
                return Ok(vec![]);
            }
        }

        let mut reply = match self.reply_get(&message_id) {
            Some(r) => r.clone(),
            None => {
                let ignored = self.ignored_messages.contains(&message_id);
                if ignored || (parent.is_none() && now < self.ignore_parentless_until) {
                    self.ignore(&message_id);
                    return Ok(vec![]);
                }
                if self.replies.len() >= MAX_MESSAGES {
                    return Err(TurnError("Muse returned too many messages for one request"));
                }
                Reply::new(message_id.clone())
            }
        };

        let mut bound = reply.bound || scoped;
        if let Some(p) = &parent {
            let parent_bound = self.reply_get(p).map(|r| r.bound) == Some(true);
            bound = scoped || self.user_ids.contains(p) || parent_bound;
        }
        reply.bound = bound;
        self.last_activity = now;
        let updated = update_reply(&reply, name, payload)?;
        self.reply_set(message_id.clone(), updated.clone());

        if name == "delta.message_done"
            || (name == "message.assistant"
                && payload.get("display_text_ready") != Some(&Value::Bool(false)))
        {
            if let Some(p) = &parent {
                self.unanswered_follow_up_ids.remove(p);
            }
        }
        Ok(vec![TurnEvent::Reply(updated)])
    }

    fn ignore(&mut self, message_id: &str) {
        self.ignored_messages.retain(|m| m != message_id);
        self.ignored_messages.push_back(message_id.to_owned());
        if self.ignored_messages.len() > MAX_IGNORED_MESSAGES {
            self.ignored_messages.pop_front();
        }
    }
}

#[cfg(test)]
#[path = "turn_tests.rs"]
mod turn_tests;
