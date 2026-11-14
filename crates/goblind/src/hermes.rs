//! The Hermes side of a chat: the daemon's use of `hermes_link`.
//!
//! A Hermes turn is simpler than a Muse one. The plugin says in so many words when a turn
//! starts, what the reply is and when it is over, so nothing has to be routed or inferred.
//! What comes out is what [`crate::muse`] gives too: the sent message's id and
//! [`TurnEvent`]s.

use std::time::Duration;

use goblin_ipc::{ErrorCode, IpcError};
use hermes_link::{LinkError, Session};
use muse_link::{AgentStatus, Reply, TurnEvent};

use crate::daemon::Payload;

/// Pairs `profile` with the Hermes a pasted link names, and saves the pairing. A profile has
/// one agent: one already paired with a Muse is left as it is.
pub(crate) async fn pair(
    profile: &goblin_store::profiles::Profile,
    device: muse_link::DeviceDescription,
    link: &hermes_link::PairLink,
) -> Result<(), String> {
    if matches!(profile.pairing(), Ok(Some(_))) {
        return Err("This profile is paired with a Muse. Unpair it first.".into());
    }
    let device = hermes_link::Device {
        id: device.node_id,
        name: device.display_name,
        version: device.version,
        // Hermes hears of the commands at each hello, not while pairing.
        commands: serde_json::json!({}),
    };
    let paired = hermes_link::pair(link, &device)
        .await
        .map_err(|e| e.to_string())?;
    profile
        .save_hermes_pairing(&goblin_store::profiles::HermesPairing {
            hosts: paired.hosts,
            port: paired.port,
            fingerprint: paired.fingerprint,
            device_id: paired.device_id,
            token: goblin_store::profiles::Secret::new(paired.token),
        })
        .map_err(|_| "The handheld could not save the pairing.".to_owned())
}

/// A live session with a Hermes Agent's Goblin Link plugin.
#[derive(Clone)]
pub(crate) struct Link {
    session: Session,
}

/// Hermes's work on one sent message.
pub(crate) struct Turn {
    turn: hermes_link::Turn,
    /// Hermes said the turn is over.
    ended: bool,
}

impl Link {
    pub(crate) fn new(session: Session) -> Self {
        Self { session }
    }

    /// The agent's name, if it gave one.
    pub(crate) fn agent(&self) -> Option<String> {
        self.session.agent().map(str::to_owned)
    }

    /// Gives the owner's answer to a question Hermes put to them.
    pub(crate) fn answer(&self, prompt_id: &str, answer: &str) {
        self.session.answer(prompt_id, answer);
    }

    /// Sends `payload` to `chat`: the message's id, and Hermes's work on it.
    pub(crate) async fn send(
        &self,
        chat: &str,
        payload: &Payload,
    ) -> Result<(String, Turn), IpcError> {
        let payload = match payload {
            Payload::Text(t) => hermes_link::Payload::Text(t),
            Payload::Voice { wav, .. } => hermes_link::Payload::Voice(wav),
        };
        match self.session.send(chat, payload).await {
            Ok(turn) => Ok((turn.message_id.clone(), Turn { turn, ended: false })),
            Err(LinkError::Refused(why)) => Err(IpcError::new(ErrorCode::Rejected, why)),
            // Sent, and then no word of it: never resent on a guess.
            Err(LinkError::Timeout | LinkError::Closed) => Err(IpcError::new(
                ErrorCode::Uncertain,
                "the message may or may not have been delivered; it was not resent",
            )),
            Err(e) => Err(IpcError::new(ErrorCode::NotConnected, e.to_string())),
        }
    }
}

impl Turn {
    /// Whether Hermes said the turn is over.
    pub(crate) fn ended(&self) -> bool {
        self.ended
    }

    /// Waits up to `wait` for what Hermes says next. `Err` is why the turn can no longer be
    /// followed: it failed, or the connection is gone.
    pub(crate) async fn next(&mut self, wait: Duration) -> Result<Vec<TurnEvent>, String> {
        use hermes_link::TurnEvent as H;
        let event = match self.turn.next(wait).await {
            Ok(Some(event)) => event,
            Ok(None) => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        Ok(vec![match event {
            H::Status { text, working } => TurnEvent::Status(AgentStatus {
                activity_code: Some(if working { "working" } else { "idle" }.into()),
                activity_text: Some(text).filter(|t| !t.is_empty()),
            }),
            H::Message {
                message_id,
                text,
                done,
            } => TurnEvent::Reply(Reply {
                message_id,
                display_text: text.clone(),
                text,
                choices: Vec::new(),
                streamed: String::new(),
                done,
                bound: true,
            }),
            H::Done => {
                self.ended = true;
                TurnEvent::Finished(muse_link::TaskFinished)
            }
            H::Failed(why) => return Err(format!("Hermes could not finish: {why}")),
        }])
    }

    /// Asks Hermes to stop working on it.
    pub(crate) fn cancel(&self) {
        self.turn.cancel();
    }
}

#[cfg(test)]
mod tests {
    use goblin_store::profiles::{Pairing, Secret};
    use goblin_store::{Layout, ProfileStore};
    use hermes_link::testsupport::{FakeHermes, Script, Tls, CODE, FINGERPRINT, TOKEN};

    fn device() -> muse_link::DeviceDescription {
        muse_link::DeviceDescription {
            node_id: "dev-1".into(),
            display_name: "Brick".into(),
            version: "0".into(),
            commands: serde_json::json!({"file.read": {}}),
        }
    }

    #[tokio::test]
    async fn pairing_saves_a_record_the_profile_keeps_to_itself() {
        let fake = FakeHermes::start(Script::default(), Tls::On).await;
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(Layout::new(dir.path().join("data"))).unwrap();
        let profile = store.create("hermes").unwrap();

        // Another machine at that address: nothing is saved.
        let stranger = fake.link(&FINGERPRINT.replace('e', "f"), CODE);
        let refused = super::pair(&profile, device(), &stranger)
            .await
            .unwrap_err();
        assert!(refused.contains("not the Hermes"), "{refused}");
        assert!(profile.hermes_pairing().unwrap().is_none());

        super::pair(&profile, device(), &fake.link(FINGERPRINT, CODE))
            .await
            .unwrap();
        let saved = profile.hermes_pairing().unwrap().unwrap();
        assert_eq!(saved.token.expose(), TOKEN);
        assert_eq!((saved.device_id.as_str(), saved.port), ("dev-1", fake.port));
        assert_eq!(saved.fingerprint, FINGERPRINT);
        // The commands are not sent while pairing: Hermes learns them at hello.
        let sent = fake.received.lock().unwrap().last().unwrap().clone();
        assert_eq!(sent["type"], "pair");
        assert!(sent.get("commands").is_none());

        // The code is spent: a second use is refused in Hermes's words.
        let other = store.create("other").unwrap();
        let again = super::pair(&other, device(), &fake.link(FINGERPRINT, CODE)).await;
        assert!(again.unwrap_err().contains("spent"));
    }

    #[tokio::test]
    async fn a_profile_paired_with_a_muse_keeps_its_muse() {
        let fake = FakeHermes::start(Script::default(), Tls::On).await;
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(Layout::new(dir.path().join("data"))).unwrap();
        let profile = store.create("muse").unwrap();
        profile
            .save_pairing(&Pairing::new(Secret::new("a"), Secret::new("r"), 1))
            .unwrap();
        let refused = super::pair(&profile, device(), &fake.link(FINGERPRINT, CODE)).await;
        assert!(refused.unwrap_err().contains("Unpair it first"));
        assert!(profile.hermes_pairing().unwrap().is_none());
        assert!(
            fake.received.lock().unwrap().is_empty(),
            "the code was not spent"
        );
    }
}
