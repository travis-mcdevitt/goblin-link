//! The Muse side of a chat: sending a message, following its turn, and reading history.
//!
//! This is the daemon's only code that knows how Muse acknowledges a message, which of its
//! chat events belong to a turn, and what its history looks like. What comes out is what any
//! backend could give: the sent message's id, [`TurnEvent`]s and [`HistoryPage`]s.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use goblin_ipc::types::{Message, MessageState, Origin, Role};
use goblin_ipc::{ErrorCode, IpcError};
use muse_link::{
    ChatSubscription, CommandRunner, DeviceDescription, LinkError, LinkSession, MuseTurn,
    SessionConfig, TurnEvent,
};
use serde_json::{json, Map, Value};

use crate::avatar::AvatarInfo;

use crate::daemon::{now_millis, HistoryMessage, HistoryPage, Payload};

/// A live session with Muse: what the daemon asks of the agent it is linked to.
#[derive(Clone)]
pub(crate) struct Link {
    session: LinkSession,
    /// What the session was opened with (its secret token, memory only), so a call can open
    /// a voice session of its own to the same VM.
    config: SessionConfig,
    /// The daemon's clock: a turn's times are seconds since this.
    started: Instant,
}

/// The voice `app_id` the SDK's `voice-call` uses by default.
const VOICE_APP_ID: &str = "hatch-web";

/// The voice session advertises no commands and refuses any.
struct RefuseCommands;

impl CommandRunner for RefuseCommands {
    fn run(
        &self,
        _command: String,
        _params: Map<String, Value>,
        _timeout_ms: Option<i64>,
    ) -> Pin<Box<dyn Future<Output = Map<String, Value>> + Send>> {
        Box::pin(async {
            let mut m = Map::new();
            m.insert("ok".into(), json!(false));
            m.insert("error".into(), json!("unsupported command"));
            m
        })
    }
}

/// A message Muse has acknowledged.
pub(crate) struct Sent {
    /// Muse's id for it ("" if it gave none).
    pub message_id: String,
    /// Muse's work on it.
    pub turn: Turn,
}

/// Muse's work on one sent message, and the events that report it.
pub(crate) struct Turn {
    session: LinkSession,
    started: Instant,
    session_id: Option<String>,
    turn: MuseTurn,
    held: Vec<TurnEvent>,
    /// `None` while a side chat this message creates does not exist yet.
    sub: Option<ChatSubscription>,
    router: ReplyRouter,
    fresh: bool,
}

impl Link {
    pub(crate) fn new(session: LinkSession, config: SessionConfig, started: Instant) -> Self {
        Self {
            session,
            config,
            started,
        }
    }

    /// The agent's name and avatar, as far as it gives them.
    pub(crate) async fn identity(&self) -> Option<(Option<String>, Option<AvatarInfo>)> {
        match self.session.request("GET", "/identity", &[], &[]).await {
            Ok((status, body)) if (200..300).contains(&status) => {
                let identity = body.get("result").unwrap_or(&body);
                let name = identity
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|n| !n.is_empty() && n.chars().count() <= 64)
                    .map(str::to_owned);
                Some((name, crate::avatar::parse(identity)))
            }
            Ok((status, _)) => {
                log::info!("identity returned HTTP {status}");
                None
            }
            Err(e) => {
                log::info!("could not read identity: {e}");
                None
            }
        }
    }

    /// A file the agent named (media in a reply, an avatar asset), at most `max` bytes.
    pub(crate) async fn file(&self, path: &str, max: usize) -> Result<Vec<u8>, LinkError> {
        self.session.fetch_workspace_file(path, max).await
    }

    /// The agent's speech for one of its messages, as MP3; what had arrived when the stream
    /// broke off counts. `None` once `wanted` says nobody is waiting for it any more.
    pub(crate) async fn speech(
        &self,
        message_id: &str,
        wanted: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<Vec<u8>>, LinkError> {
        let mut stream = self.session.stream_tts(message_id).await?;
        let mut mp3 = Vec::new();
        while let Some(chunk) = stream.next_chunk().await {
            if !wanted() {
                return Ok(None);
            }
            match chunk {
                Ok(c) => mp3.extend_from_slice(&c),
                Err(e) => {
                    log::info!("reply speech stream ended early: {e}");
                    break;
                }
            }
        }
        Ok(Some(mp3))
    }

    /// Runs one live call until it ends and says how it ended; `Err` when no call could start.
    /// `thread` is the side chat it belongs to, `greeting` whether Muse speaks first.
    pub(crate) async fn call(
        &self,
        device: DeviceDescription,
        platform: Arc<goblin_platform::Platform>,
        thread: Option<String>,
        cmds: tokio::sync::mpsc::Receiver<crate::call::CallCmd>,
        observer: Arc<dyn crate::call::CallObserver>,
        greeting: &(dyn Fn() -> bool + Sync),
    ) -> Result<String, String> {
        let mut config = self.config.clone();
        config.app_id = Some(VOICE_APP_ID.into());
        config.register_device = false;
        config.invoke_slots = None;
        let device_id = device.node_id.clone();
        config.device = DeviceDescription {
            node_id: device.node_id,
            display_name: "Muse voice".into(),
            version: device.version,
            commands: json!({}),
        };
        let session = LinkSession::connect(config, Arc::new(RefuseCommands))
            .await
            .map_err(|e| format!("No voice connection ({e})"))?;
        let reason = crate::call::run(
            session.clone(),
            platform,
            device_id,
            thread,
            cmds,
            observer,
            greeting(),
        )
        .await;
        session.request_stop();
        Ok(reason)
    }

    /// Sends `payload` to `chat` ("main" or a side chat's id). `voice_reply` asks for a
    /// voice-modality answer to a voice note, which the speech stream can speak.
    pub(crate) async fn send(
        &self,
        chat: &str,
        payload: &Payload,
        voice_reply: bool,
    ) -> Result<Sent, IpcError> {
        let session_id = (chat != "main").then(|| chat.to_owned());
        // Open a subscription first, so the reply stream is in place. A side chat Muse does not
        // know yet answers 404: the message itself creates it, so send first and subscribe after
        // (as the reference's `send_and_wait` does).
        let sub = match self.session.subscribe_chat(session_id.as_deref()).await {
            Ok(sub) => Some(sub),
            Err(LinkError::HttpStatus { status: 404, .. }) if session_id.is_some() => None,
            Err(e) => return Err(map_link_err(e)),
        };
        let sent = match payload {
            Payload::Text(t) => {
                self.session
                    .send_chat(t, session_id.as_deref(), "text", None)
                    .await
            }
            // Speech-to-text by default: Muse transcribes the note and replies as usual.
            Payload::Voice { wav, .. } => {
                let modality = voice_reply.then_some("voice");
                self.session
                    .send_voice(wav, session_id.as_deref(), "", modality, None)
                    .await
            }
        };
        let ack = match sent {
            Ok(a) => a,
            Err(LinkError::SessionEnded) | Err(LinkError::NotConnected) => {
                // Delivery is ambiguous: never auto-resend.
                return Err(IpcError::new(
                    ErrorCode::Uncertain,
                    "the message may or may not have been delivered; it was not resent",
                ));
            }
            Err(e) => return Err(map_link_err(e)),
        };
        let now = self.started.elapsed().as_secs_f64();
        // Seed last_activity with the current time: `MuseTurn::new` uses 0.0, which makes a new
        // turn look quiet (and so complete) before Muse has said anything.
        let mut turn = MuseTurn::with_options(session_id.clone(), false, None, 0.0, now);
        let held = turn
            .acknowledge(&ack, false, now)
            .map_err(|e| IpcError::new(ErrorCode::Internal, e.to_string()))?;
        let message_id = ack
            .get("result")
            .and_then(|r| r.get("message_id"))
            .or_else(|| ack.get("message_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let fresh = sub.is_none();
        Ok(Sent {
            message_id,
            turn: Turn {
                session: self.session.clone(),
                started: self.started,
                session_id,
                turn,
                held,
                sub,
                router: ReplyRouter { after_note: fresh },
                fresh,
            },
        })
    }

    /// Up to `limit` history events of `chat`: the newest, or with `before_seq` those just
    /// older than that sequence number.
    pub(crate) async fn history(
        &self,
        chat: &str,
        limit: u32,
        before_seq: Option<i64>,
    ) -> Result<HistoryPage, IpcError> {
        let mut path = format!("/chat/history?limit={limit}");
        if chat != "main" {
            path.push_str(&format!("&session_id={chat}"));
        }
        if let Some(seq) = before_seq {
            path.push_str(&format!("&before_seq={seq}"));
        }
        let (status, body) = self
            .session
            .request("GET", &path, &[], &[])
            .await
            .map_err(map_link_err)?;
        if !(200..300).contains(&status) {
            return Err(IpcError::new(
                ErrorCode::Internal,
                format!("chat history returned HTTP {status}"),
            ));
        }
        Ok(history_page(chat, &body))
    }
}

impl Turn {
    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// Whether the message created its chat, so the events that report its turn could only be
    /// followed late: history, not the live stream, decides what the answer was.
    pub(crate) fn fresh(&self) -> bool {
        self.fresh
    }

    /// What the acknowledgement itself already said about the turn. Empty after the first call.
    pub(crate) fn take_held(&mut self) -> Vec<TurnEvent> {
        std::mem::take(&mut self.held)
    }

    /// Makes sure the turn's events can be followed. A fresh side chat exists once Muse has
    /// acknowledged the message, so this is where it is subscribed to.
    pub(crate) async fn open(&mut self) -> bool {
        for attempt in 0..5u64 {
            if self.sub.is_some() {
                break;
            }
            match self
                .session
                .subscribe_chat(self.session_id.as_deref())
                .await
            {
                Ok(sub) => self.sub = Some(sub),
                Err(LinkError::HttpStatus { status: 404, .. }) => {
                    tokio::time::sleep(Duration::from_millis(300 * (attempt + 1))).await;
                }
                Err(e) => {
                    log::warn!("could not follow the new side chat: {e}");
                    break;
                }
            }
        }
        self.sub.is_some()
    }

    /// Whether there is nothing more to wait for: the turn is settled, or, once reply text has
    /// arrived (`replied`), complete.
    pub(crate) fn finished(&self, replied: bool) -> bool {
        let now = self.now();
        self.turn.settled(now) || (replied && self.turn.complete(now))
    }

    /// Waits up to `wait` for the next event and says what it means for the turn (nothing, for
    /// an event that is not this turn's, or when none came). `Err` is why it can no longer be
    /// followed.
    pub(crate) async fn next(&mut self, wait: Duration) -> Result<Vec<TurnEvent>, String> {
        let Some(sub) = self.sub.as_mut() else {
            return Err("chat subscription ended".into());
        };
        match tokio::time::timeout(wait, sub.next_event()).await {
            Ok(Some(Ok(event))) => {
                if log::log_enabled!(log::Level::Debug) {
                    let payload = event.get("payload").unwrap_or(&event);
                    log::debug!(
                        "turn event: {} seq {}",
                        nonempty_str(event.get("event")).unwrap_or("?"),
                        payload
                            .get("seq")
                            .map(|v| v.to_string())
                            .unwrap_or_default()
                    );
                }
                if !self.router.admits(&self.turn, &event) {
                    return Ok(Vec::new());
                }
                let now = self.now();
                self.turn
                    .feed(&event, now)
                    .map_err(|e| format!("event rejected: {e}"))
            }
            Ok(Some(Err(e))) => Err(format!("chat subscription failed: {e}")),
            Ok(None) => Err("chat subscription ended".into()),
            Err(_) => Ok(Vec::new()), // nothing yet; the caller re-checks completion
        }
    }

    /// Whether the turn ended properly, judged once it is no longer followed.
    pub(crate) fn done(&self) -> bool {
        self.turn.task_finished || self.turn.complete(self.now() + 4.0)
    }

    /// Stops following.
    pub(crate) async fn close(mut self) {
        if let Some(sub) = self.sub.as_mut() {
            sub.close().await;
        }
    }
}

/// Port of the reference's `_ReplyReader` routing (service.py): on a shared chat, only the
/// events after this turn's own `message.user` (until another user message), or ones linked to
/// this turn's message or replies, reach the turn. Other people's messages on the same chat must
/// not be mistaken for the answer.
struct ReplyRouter {
    after_note: bool,
}

/// A non-empty string field (Muse sends absent ids as "").
fn nonempty_str(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty())
}

impl ReplyRouter {
    fn admits(&mut self, turn: &MuseTurn, event: &Value) -> bool {
        let Some(payload) = event.get("payload").filter(|p| p.is_object()) else {
            return false;
        };
        if event.get("type").and_then(Value::as_str) != Some("event") {
            return false;
        }
        let chat = nonempty_str(payload.get("session_id"))
            .or_else(|| nonempty_str(event.get("session_id")));
        if let (Some(chat), Some(mine)) = (chat, turn.session_id.as_deref()) {
            if chat != mine {
                return false;
            }
        }
        let message_id = nonempty_str(payload.get("message_id"))
            .or_else(|| nonempty_str(event.get("message_id")))
            .or_else(|| nonempty_str(payload.get("id")));
        if event.get("event").and_then(Value::as_str) == Some("message.user") {
            self.after_note = message_id.is_some_and(|m| turn.is_user_message(m));
            return false;
        }
        let parent = nonempty_str(payload.get("reply_to_message_id"))
            .or_else(|| nonempty_str(payload.get("parent_message_id")));
        self.after_note
            || parent.is_some_and(|p| turn.is_user_message(p) || turn.has_reply(p))
            || message_id.is_some_and(|m| turn.has_reply(m))
    }
}

/// Muse's display text with attachment markers (`[file:audio/wav …]`) removed. For a voice note
/// what remains is Muse's transcript of it.
fn clean_display_text(text: &str) -> String {
    text.lines()
        .filter(|l| {
            let t = l.trim();
            !(t.starts_with("[file:") && t.ends_with(']'))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// Parses `/chat/history`'s `chat_events` into messages. Only `message.user` and
/// `message.assistant` rows with display text count; everything else is ignored.
fn parse_history(chat: &str, body: &Value) -> Vec<HistoryMessage> {
    let events = body
        .get("result")
        .unwrap_or(body)
        .get("chat_events")
        .and_then(Value::as_array);
    let Some(events) = events else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ev in events {
        let role = match ev.get("event_name").and_then(Value::as_str) {
            Some("message.user") => Role::User,
            Some("message.assistant") => Role::Assistant,
            Some("delta.presentation") => {
                // Images or video Muse sent: a message of its own, holding the media.
                let refs = crate::media::refs_from_event(ev);
                let id = ev.get("message_id").and_then(Value::as_str).unwrap_or("");
                if refs.is_empty() || id.is_empty() || id.len() > 200 {
                    continue;
                }
                out.push(HistoryMessage {
                    message: Message {
                        id: id.to_owned(),
                        chat: chat.to_owned(),
                        role: Role::Assistant,
                        text: String::new(),
                        rich: None,
                        choices: Vec::new(),
                        attachments: None,
                        reaction: None,
                        at: ev
                            .get("occurred_at_ms")
                            .and_then(Value::as_u64)
                            .unwrap_or_else(now_millis),
                        state: MessageState::Done,
                        origin: Origin::Elsewhere,
                    },
                    seq: ev.get("seq").and_then(Value::as_i64).unwrap_or(0),
                    parent: None,
                    media: refs,
                });
                continue;
            }
            _ => continue,
        };
        if ev.get("display_text_ready").and_then(Value::as_bool) == Some(false) {
            continue; // still being written
        }
        let raw = clean_display_text(ev.get("display_text").and_then(Value::as_str).unwrap_or(""));
        let resolved = if role == Role::Assistant {
            muse_link::choices::decode(&raw, ev)
        } else {
            muse_link::choices::Choices {
                text: raw,
                replies: Vec::new(),
            }
        };
        let text = resolved.text.as_str();
        let id = ev.get("message_id").and_then(Value::as_str).unwrap_or("");
        if text.trim().is_empty() || id.is_empty() || id.len() > 200 {
            continue;
        }
        let at = ev
            .get("occurred_at_ms")
            .and_then(Value::as_u64)
            .unwrap_or_else(now_millis);
        out.push(HistoryMessage {
            message: Message {
                id: id.to_owned(),
                chat: chat.to_owned(),
                role,
                text: text.to_owned(),
                rich: None,
                choices: resolved.replies,
                attachments: None,
                reaction: ev
                    .get("reaction_emoji")
                    .and_then(Value::as_str)
                    .filter(|r| !r.is_empty() && r.chars().count() <= 8)
                    .map(str::to_owned),
                at,
                state: if role == Role::User {
                    MessageState::Sent
                } else {
                    MessageState::Done
                },
                origin: Origin::Elsewhere,
            },
            seq: ev.get("seq").and_then(Value::as_i64).unwrap_or(0),
            parent: ev
                .get("parent_message_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            media: Vec::new(),
        });
    }
    out
}

/// Reads a `/chat/history` response as a page.
fn history_page(chat: &str, body: &Value) -> HistoryPage {
    let result = body.get("result").unwrap_or(body);
    let floor = result
        .get("chat_events")
        .and_then(Value::as_array)
        .and_then(|events| {
            events
                .iter()
                .filter_map(|e| e.get("seq").and_then(Value::as_i64))
                .min()
        });
    HistoryPage {
        messages: parse_history(chat, body),
        floor,
        has_more: result.get("has_more").and_then(Value::as_bool) == Some(true),
    }
}

fn map_link_err(e: muse_link::LinkError) -> IpcError {
    use muse_link::LinkError as L;
    match e {
        L::NotConnected | L::SessionEnded => IpcError::new(ErrorCode::NotConnected, e.to_string()),
        L::Timeout => IpcError::new(ErrorCode::Timeout, e.to_string()),
        L::HttpStatus { status: 403, .. } => IpcError::new(ErrorCode::Rejected, e.to_string()),
        L::Invalid(_) => IpcError::new(ErrorCode::BadRequest, e.to_string()),
        _ => IpcError::new(ErrorCode::Internal, e.to_string()),
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use crate::daemon::{media_anchors, replies_to};
    use serde_json::json;

    fn ev(name: &str, id: &str, seq: i64, parent: Option<&str>, text: &str) -> Value {
        json!({"event_name": name, "message_id": id, "seq": seq, "parent_message_id": parent,
               "display_text": text, "occurred_at_ms": 1_000 + seq, "role": "x"})
    }

    fn body(events: Vec<Value>) -> Value {
        json!({"ok": true, "result": {"chat_events": events, "has_more": false}})
    }

    #[test]
    fn a_history_page_knows_its_floor_and_whether_more_exists() {
        // Events that are not messages still count toward the floor.
        let mut b = body(vec![
            ev("delta.tool", "t", 7, None, ""),
            ev("message.user", "u", 8, None, "hi"),
            ev("message.assistant", "a", 9, Some("u"), "hello"),
        ]);
        let page = history_page("main", &b);
        assert_eq!(page.floor, Some(7));
        assert_eq!(page.messages.len(), 2);
        assert!(!page.has_more);
        b["result"]["has_more"] = json!(true);
        assert!(history_page("main", &b).has_more);
        let empty = history_page("main", &body(vec![]));
        assert_eq!(empty.floor, None);
        assert!(empty.messages.is_empty());
    }

    #[test]
    fn media_and_reactions_from_history() {
        let media = |id: &str, seq: i64| {
            json!({"event_name": "delta.presentation", "message_id": id, "seq": seq,
                   "occurred_at_ms": 1_000 + seq,
                   "payload": {"kind": "image", "data": {"images": [
                       {"path": "sandbox://workspace/imagine_media/a.webp", "label": "a"}]}}})
        };
        let mut reacted = ev("message.user", "u1", 1, None, "draw it");
        reacted["reaction_emoji"] = json!("\u{1F44D}");
        // The next message comes seconds later, as in life (the fixture's are 1 ms apart).
        let mut later = ev("message.user", "u2", 5, None, "thanks");
        later["occurred_at_ms"] = json!(9_000);
        let b = body(vec![
            reacted,
            ev("message.assistant", "a1", 2, Some("u1"), "Here it is."),
            media("p1", 3),
            media("p2", 4),
            later,
        ]);
        let mut h = parse_history("c-1", &b);
        assert_eq!(h.len(), 5);
        assert_eq!(h[0].message.reaction.as_deref(), Some("\u{1F44D}"));
        assert_eq!(h[2].media.len(), 1);
        // Muse's order is text, then media; each media item is anchored to that text.
        let anchors = media_anchors(&mut h);
        assert_eq!(anchors["p1"], ("a1".to_owned(), 1));
        assert_eq!(anchors["p2"], ("a1".to_owned(), 2));
        assert_eq!(anchors.len(), 2);
    }

    #[test]
    fn parses_user_and_assistant_rows_only() {
        let b = body(vec![
            ev("message.user", "u1", 1, None, "hello"),
            ev("task.status", "t1", 2, None, "working"),
            ev("message.assistant", "a1", 3, Some("u1"), "hi"),
            ev("message.assistant", "a2", 4, Some("u1"), "   "),
            json!({"event_name": "message.assistant", "message_id": "a3", "seq": 5,
                   "display_text": "half", "display_text_ready": false}),
        ]);
        let h = parse_history("c-1", &b);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].message.role, Role::User);
        assert_eq!(h[1].message.text, "hi");
        assert_eq!(h[1].message.state, MessageState::Done);
        assert_eq!(h[1].parent.as_deref(), Some("u1"));
    }

    #[test]
    fn replies_by_parent_or_after_a_fresh_message() {
        let b = body(vec![
            ev("message.user", "u0", 1, None, "older"),
            ev("message.assistant", "a0", 2, Some("u0"), "older reply"),
            ev("message.user", "u1", 3, None, "mine"),
            ev("message.assistant", "a1", 4, None, "parentless reply"),
            ev("message.assistant", "a2", 5, Some("u1"), "named reply"),
        ]);
        let h = parse_history("c-1", &b);
        let ids = |v: Vec<Message>| v.into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(ids(replies_to(&h, "u1", false)), vec!["a2"]);
        assert_eq!(ids(replies_to(&h, "u1", true)), vec!["a1", "a2"]);
        assert!(replies_to(&h, "missing", true).is_empty());
    }

    #[test]
    fn tolerates_missing_or_odd_bodies() {
        assert!(parse_history("main", &json!(null)).is_empty());
        assert!(parse_history("main", &json!({"result": {"chat_events": "x"}})).is_empty());
        assert!(parse_history("main", &json!({"chat_events": [{"event_name": 3}]})).is_empty());
    }
}

#[cfg(test)]
mod router_tests {
    use super::*;
    use serde_json::json;

    fn ev(name: &str, payload: Value) -> Value {
        json!({"type": "event", "event": name, "payload": payload})
    }

    #[test]
    fn opens_on_own_message_and_closes_on_another() {
        let mut turn = MuseTurn::with_options(None, false, None, 0.0, 1.0);
        turn.acknowledge(&json!({"result": {"message_id": "u1"}}), false, 1.0)
            .unwrap();
        let mut r = ReplyRouter { after_note: false };
        // Before our message: a parentless reply on the shared chat is not ours.
        assert!(!r.admits(
            &turn,
            &ev(
                "delta.message_start",
                json!({"message_id": "x0", "reply_to_message_id": ""})
            )
        ));
        assert!(!r.admits(&turn, &ev("message.user", json!({"message_id": "u1"}))));
        assert!(r.admits(
            &turn,
            &ev(
                "delta.message_start",
                json!({"message_id": "a1", "reply_to_message_id": ""})
            )
        ));
        // Someone else speaks: later parentless events are theirs.
        assert!(!r.admits(&turn, &ev("message.user", json!({"message_id": "u2"}))));
        assert!(!r.admits(
            &turn,
            &ev("delta.message_start", json!({"message_id": "x2"}))
        ));
        // But anything naming our message still counts.
        assert!(r.admits(
            &turn,
            &ev(
                "delta.message_start",
                json!({"message_id": "a3", "parent_message_id": "u1"})
            )
        ));
    }

    #[test]
    fn other_chats_are_ignored() {
        let mut turn = MuseTurn::with_options(Some("s-1".into()), false, None, 0.0, 1.0);
        turn.acknowledge(&json!({"result": {"message_id": "u1"}}), false, 1.0)
            .unwrap();
        let mut r = ReplyRouter { after_note: true };
        assert!(!r.admits(
            &turn,
            &ev(
                "delta.message_start",
                json!({"message_id": "a1", "session_id": "s-2"})
            )
        ));
        assert!(r.admits(
            &turn,
            &ev(
                "delta.message_start",
                json!({"message_id": "a1", "session_id": "s-1"})
            )
        ));
    }
}

#[cfg(test)]
mod transcript_tests {
    use super::clean_display_text;

    #[test]
    fn file_markers_are_dropped() {
        assert_eq!(
            clean_display_text(
                "Voice test, okay?\n[file:audio/wav workspace/user/files/voice_note.wav]"
            ),
            "Voice test, okay?"
        );
        assert_eq!(clean_display_text("[file:audio/wav x.wav]"), "");
        assert_eq!(
            clean_display_text("plain [file: inline] text"),
            "plain [file: inline] text"
        );
    }
}

#[cfg(test)]
mod choice_tests {
    #[test]
    fn native_history_choices_are_bound_to_the_assistant_message() {
        let body = serde_json::json!({"chat_events":[{
            "event_name":"message.assistant", "message_id":"a1", "display_text":"Where? [[hatch_widget:w]]",
            "payload":{"widgets":[{"widget_id":"w", "kind":"option", "data":{"options":["On the Brick", "On the web"]}}]}
        }]});
        let history = super::parse_history("main", &body);
        assert_eq!(history[0].message.choices, ["On the Brick", "On the web"]);
        assert!(!history[0].message.text.contains("hatch_widget"));
    }
}
