//! The agent a profile is linked to: a Muse or a Hermes.
//!
//! The daemon asks the same things of either (send a message and follow its turn, read
//! history, the agent's name, a file, speech, a call) and each answers in its own way in
//! [`crate::muse`] or [`crate::hermes`]. What one of them does not have, it says so: a Hermes
//! has no live call, and keeps no history for the handheld to read back.

use std::sync::Arc;
use std::time::Duration;

use goblin_ipc::IpcError;
// What the daemon and either agent say to each other in: the handheld's description, what a
// turn can report, the command runner. These shapes were Muse's first and still live in its
// crate; the rest of the daemon takes them from here, and only what speaks to a Muse names
// `muse_link`.
pub(crate) use muse_link::{
    random_uuid, AgentStatus, CommandRunner, DeviceDescription, Reply, TaskFinished, TurnEvent,
};

use crate::avatar::AvatarInfo;
use crate::daemon::{HistoryPage, Payload};
use crate::{hermes, muse};

/// How much of a reply a Hermes is asked to say (the plugin's own limit is 4096).
const MAX_SPOKEN_CHARS: usize = 4000;

/// A live link to the profile's agent.
#[derive(Clone)]
pub(crate) enum Link {
    Muse(Box<muse::Link>),
    Hermes(hermes::Link),
}

/// The agent's work on one sent message.
pub(crate) enum Turn {
    Muse(Box<muse::Turn>),
    Hermes(hermes::Turn),
}

/// A message the agent has taken.
pub(crate) struct Sent {
    /// The agent's id for it ("" if it gave none).
    pub message_id: String,
    pub turn: Turn,
}

const NOT_ON_HERMES: &str = "Hermes does not offer this";

impl Link {
    /// Sends `payload` to `chat`. `voice_reply` asks a Muse for a spoken answer to a voice note.
    pub(crate) async fn send(
        &self,
        chat: &str,
        payload: &Payload,
        voice_reply: bool,
    ) -> Result<Sent, IpcError> {
        match self {
            Link::Muse(link) => {
                let sent = link.send(chat, payload, voice_reply).await?;
                Ok(Sent {
                    message_id: sent.message_id,
                    turn: Turn::Muse(Box::new(sent.turn)),
                })
            }
            Link::Hermes(link) => {
                let (message_id, turn) = link.send(chat, payload).await?;
                Ok(Sent {
                    message_id,
                    turn: Turn::Hermes(turn),
                })
            }
        }
    }

    /// A page of the chat's history as the agent keeps it. A Hermes keeps none for the
    /// handheld: its own copy of the chat is the record.
    pub(crate) async fn history(
        &self,
        chat: &str,
        limit: u32,
        before_seq: Option<i64>,
    ) -> Result<HistoryPage, IpcError> {
        match self {
            Link::Muse(link) => link.history(chat, limit, before_seq).await,
            Link::Hermes(_) => Ok(HistoryPage {
                messages: Vec::new(),
                floor: None,
                has_more: false,
            }),
        }
    }

    /// The agent's name and avatar, as far as it gives them.
    pub(crate) async fn identity(&self) -> Option<(Option<String>, Option<AvatarInfo>)> {
        match self {
            Link::Muse(link) => link.identity().await,
            Link::Hermes(link) => Some((link.agent(), None)),
        }
    }

    /// A file the agent named, at most `max` bytes.
    pub(crate) async fn file(&self, path: &str, max: usize) -> Result<Vec<u8>, String> {
        match self {
            Link::Muse(link) => link.file(path, max).await.map_err(|e| e.to_string()),
            Link::Hermes(_) => Err(NOT_ON_HERMES.into()),
        }
    }

    /// The agent's speech for one of its messages, as MP3; `None` once nobody waits for it.
    /// A Muse knows its messages by id; a Hermes is given the words to say (`text`).
    pub(crate) async fn speech(
        &self,
        message_id: &str,
        text: Option<&str>,
        wanted: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<Vec<u8>>, String> {
        match self {
            Link::Muse(link) => link
                .speech(message_id, wanted)
                .await
                .map_err(|e| e.to_string()),
            Link::Hermes(link) => {
                let text = text.ok_or("the reply is no longer on this handheld")?;
                // A long reply is spoken as far as Hermes takes at once.
                let end = text
                    .char_indices()
                    .nth(MAX_SPOKEN_CHARS)
                    .map_or(text.len(), |(at, _)| at);
                let mp3 = link.speech(&text[..end]).await?;
                Ok(wanted().then_some(mp3))
            }
        }
    }

    /// Runs one live call and says how it ended; `Err` when none could start.
    pub(crate) async fn call(
        &self,
        device: DeviceDescription,
        platform: Arc<goblin_platform::Platform>,
        thread: Option<String>,
        cmds: tokio::sync::mpsc::Receiver<crate::call::CallCmd>,
        observer: Arc<dyn crate::call::CallObserver>,
        greeting: &(dyn Fn() -> bool + Sync),
    ) -> Result<String, String> {
        match self {
            Link::Muse(link) => {
                link.call(device, platform, thread, cmds, observer, greeting)
                    .await
            }
            Link::Hermes(_) => Err("Calls are not available with Hermes".into()),
        }
    }

    /// Gives the owner's answer to a question the agent put to them as a card.
    pub(crate) fn answer(&self, prompt_id: &str, answer: &str) {
        if let Link::Hermes(link) = self {
            link.answer(prompt_id, answer);
        }
    }
}

impl Turn {
    /// Whether the message created its chat, so that history, not the live events, decides
    /// what the answer was.
    pub(crate) fn fresh(&self) -> bool {
        match self {
            Turn::Muse(turn) => turn.fresh(),
            Turn::Hermes(_) => false,
        }
    }

    /// What the agent's acknowledgement already said about the turn.
    pub(crate) fn take_held(&mut self) -> Vec<TurnEvent> {
        match self {
            Turn::Muse(turn) => turn.take_held(),
            Turn::Hermes(_) => Vec::new(),
        }
    }

    /// What the agent heard in the voice note the turn began with, once it says so.
    pub(crate) fn take_heard(&mut self) -> Option<String> {
        match self {
            Turn::Muse(_) => None,
            Turn::Hermes(turn) => turn.take_heard(),
        }
    }

    /// Whether the agent can be asked to stop working on it. Muse offers the handheld no
    /// such request.
    pub(crate) fn stoppable(&self) -> bool {
        matches!(self, Turn::Hermes(_))
    }

    /// Makes sure the turn's events can be followed.
    pub(crate) async fn open(&mut self) -> bool {
        match self {
            Turn::Muse(turn) => turn.open().await,
            Turn::Hermes(_) => true,
        }
    }

    /// Whether there is nothing more to wait for.
    pub(crate) fn finished(&self, replied: bool) -> bool {
        match self {
            Turn::Muse(turn) => turn.finished(replied),
            Turn::Hermes(turn) => turn.ended(),
        }
    }

    /// Waits up to `wait` for what the agent says next about the turn.
    pub(crate) async fn next(&mut self, wait: Duration) -> Result<Vec<TurnEvent>, String> {
        match self {
            Turn::Muse(turn) => turn.next(wait).await,
            Turn::Hermes(turn) => turn.next(wait).await,
        }
    }

    /// Whether the turn ended properly, judged once it is no longer followed.
    pub(crate) fn done(&self) -> bool {
        match self {
            Turn::Muse(turn) => turn.done(),
            Turn::Hermes(turn) => turn.ended(),
        }
    }

    /// Stops following. A Hermes turn that did not end is asked to stop working too.
    pub(crate) async fn close(self) {
        match self {
            Turn::Muse(turn) => turn.close().await,
            Turn::Hermes(turn) => {
                if !turn.ended() {
                    turn.cancel();
                }
            }
        }
    }
}
