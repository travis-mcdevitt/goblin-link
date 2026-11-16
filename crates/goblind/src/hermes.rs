//! The Hermes side of a chat: the daemon's use of `hermes_link`.
//!
//! A Hermes turn is simpler than a Muse one. The plugin says in so many words when a turn
//! starts, what the reply is and when it is over, so nothing has to be routed or inferred.
//! What comes out is what [`crate::muse`] gives too: the sent message's id and
//! [`TurnEvent`]s.

use std::time::Duration;

use goblin_ipc::{ErrorCode, IpcError};
use hermes_link::{LinkError, Session};

use crate::backend::{AgentStatus, Reply, TurnEvent};
use crate::daemon::Payload;

/// Why a profile that has a Hermes pairing, or a record of one that cannot be read, may not be
/// paired with anything until it is unpaired: a profile has one agent, and a second pairing
/// beside the first would leave the old agent linked while the screen names the new one.
pub(crate) fn blocks_pairing(profile: &goblin_store::profiles::Profile) -> Option<&'static str> {
    match profile.hermes_pairing() {
        Ok(None) => None,
        Ok(Some(_)) => Some("This profile is paired with a Hermes. Unpair it first."),
        Err(_) => Some("This profile's Hermes pairing cannot be read. Unpair it first."),
    }
}

/// How pairing with a Hermes ended, when nothing went wrong.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Paired {
    /// The pairing is saved in the profile.
    Saved,
    /// The owner stopped it first: nothing is saved.
    Stopped,
}

/// Pairs `profile` with the Hermes a pasted link names, and saves the pairing. A profile has
/// one agent: one already paired with a Muse is left as it is. `name` and `version` are the
/// handheld's, the same for each of its profiles. `stop` is the owner stopping the pairing.
pub(crate) async fn pair(
    profile: &goblin_store::profiles::Profile,
    name: String,
    version: String,
    link: &hermes_link::PairLink,
    stop: &tokio::sync::Notify,
) -> Result<Paired, String> {
    if let Some(why) = blocks_pairing(profile) {
        return Err(why.into());
    }
    if !matches!(profile.pairing(), Ok(None)) {
        return Err("This profile is paired with a Muse. Unpair it first.".into());
    }
    // The profile's own id, whichever profile the daemon is serving. Hermes keeps one token
    // to an id: paired under another profile's, this one would take that profile's place
    // there, and the other would be told it is unpaired.
    let id = profile
        .identity()
        .map_err(|_| "The handheld could not read this profile's identity.".to_owned())?
        .node_id();
    let device = hermes_link::Device {
        id,
        name,
        version,
        // Hermes hears of the commands at each hello, not while pairing.
        commands: serde_json::json!({}),
    };
    // This can take a while: eight seconds for each address that does not answer, and
    // twenty more for Hermes's answer. The owner may stop it at any point in that. Nothing
    // is saved until it is over, so stopped here the handheld keeps nothing: not even a
    // token Hermes had already sent, which is dropped with the connection.
    let paired = tokio::select! {
        biased;
        () = stop.notified() => return Ok(Paired::Stopped),
        paired = hermes_link::pair(link, &device) => paired.map_err(|e| e.to_string())?,
    };
    profile
        .save_hermes_pairing(&goblin_store::profiles::HermesPairing {
            hosts: paired.hosts,
            port: paired.port,
            fingerprint: paired.fingerprint,
            device_id: paired.device_id,
            token: goblin_store::profiles::Secret::new(paired.token),
        })
        .map_err(|_| "The handheld could not save the pairing.".to_owned())?;
    Ok(Paired::Saved)
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
    /// What Hermes heard in the voice note, until the daemon takes it.
    heard: Option<String>,
}

impl Link {
    pub(crate) fn new(session: Session) -> Self {
        Self { session }
    }

    /// The agent's name, if it gave one.
    pub(crate) fn agent(&self) -> Option<String> {
        self.session.agent()
    }

    /// The picture the agent chose for itself, if any: a still image and no clips, named
    /// by its revision (which is also what [`Link::avatar`] is asked for).
    pub(crate) fn avatar_info(&self) -> Option<crate::avatar::AvatarInfo> {
        let revision = self.session.avatar_revision()?;
        Some(crate::avatar::AvatarInfo {
            image: Some(revision.clone()),
            revision,
            clips: Default::default(),
        })
    }

    /// Fetches the agent's picture of that revision.
    pub(crate) async fn avatar(&self, revision: &str) -> Result<Vec<u8>, String> {
        self.session
            .avatar(revision)
            .await
            .map_err(|e| e.to_string())
    }

    /// Has Hermes say `text` aloud, and returns the audio as MP3.
    pub(crate) async fn speech(&self, text: &str) -> Result<Vec<u8>, String> {
        self.session.speech(text).await.map_err(|e| e.to_string())
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
            Ok(turn) => Ok((
                turn.message_id.clone(),
                Turn {
                    turn,
                    ended: false,
                    heard: None,
                },
            )),
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
    /// What Hermes heard in the voice note, once: `None` after it has been taken.
    pub(crate) fn take_heard(&mut self) -> Option<String> {
        self.heard.take()
    }

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
            H::Heard(text) => {
                self.heard = Some(text);
                return Ok(Vec::new());
            }
            H::Done => {
                self.ended = true;
                TurnEvent::Finished(crate::backend::TaskFinished)
            }
            H::Failed(why) => return Err(format!("Hermes could not finish: {why}")),
        }])
    }

    /// Asks Hermes to stop working on it.
    pub(crate) fn cancel(&self) {
        self.turn.cancel();
    }
}

/// One connection to the Hermes a profile is paired with: connect, say hello, stay linked
/// until it ends, then wait as long as the way it ended calls for.
pub(crate) async fn serve(
    state: &std::sync::Arc<crate::daemon::DaemonState>,
    profile: &goblin_store::profiles::Profile,
    stored: goblin_store::profiles::HermesPairing,
    backoff: &mut crate::service::Backoff,
) {
    use crate::service::{
        sleep_or_stop, AUTH_BACKOFF_MIN_S, HEALTHY_RECONNECT_S, HEALTHY_SESSION_S, UNPAIRED_POLL_S,
    };
    use goblin_ipc::types::LinkState;
    use hermes_link::LinkError as E;
    state.set_link_state(LinkState::Connecting, None);
    let described = state.device();
    let device = hermes_link::Device {
        // The id it was paired under, as saved with the token: Hermes knows the token by it.
        id: stored.device_id.clone(),
        name: described.display_name,
        version: described.version,
        commands: described.commands,
    };
    let pairing = hermes_link::Pairing {
        hosts: stored.hosts,
        port: stored.port,
        fingerprint: stored.fingerprint,
        device_id: stored.device_id,
        token: stored.token.expose().to_owned(),
    };
    let (pushed, mut pushes) = tokio::sync::mpsc::channel(16);
    let runner = std::sync::Arc::new(crate::daemon::DaemonRunner::new(state.clone()));
    let session = match hermes_link::Session::connect(&pairing, &device, runner, pushed).await {
        Ok(session) => session,
        Err(E::Unpaired) => {
            log::warn!("Hermes no longer knows this handheld: unpaired");
            let deleted = profile.delete_hermes_pairing();
            state.set_link_state(LinkState::Unpaired, Some("unpaired by Hermes".into()));
            if deleted.is_err() {
                // The record is still there: without a wait this would ask Hermes again at once.
                sleep_or_stop(state, UNPAIRED_POLL_S).await;
            }
            return;
        }
        Err(e) => {
            // Another certificate at Hermes's address is said as what it is, and never
            // answered by pairing again without the owner.
            let (link, floor) = match e {
                E::WrongCertificate => (LinkState::AuthFailed, AUTH_BACKOFF_MIN_S),
                _ => (LinkState::Offline, 0.0),
            };
            state.set_link_state(link, Some(e.to_string()));
            backoff.set_floor(floor);
            let delay = backoff.next_delay();
            log::info!("Hermes: {e}; retrying in {delay:.0}s");
            sleep_or_stop(state, delay).await;
            return;
        }
    };
    let started = std::time::Instant::now();
    state.set_link(Some(crate::backend::Link::Hermes(Link::new(
        session.clone(),
    ))));
    state.set_link_state(LinkState::Online, None);
    state.send_owed_reply();
    let naming = state.clone();
    tokio::spawn(async move { naming.refresh_identity().await });
    let stopped = loop {
        tokio::select! {
            Some(push) = pushes.recv() => state.agent_pushed(push),
            _ = session.wait_closed() => break false,
            () = state.wait_shutdown() => {
                session.stop();
                break true;
            }
        }
    };
    state.set_link(None);
    if stopped {
        return;
    }
    state.set_link_state(LinkState::Offline, None);
    let delay = if started.elapsed().as_secs_f64() >= HEALTHY_SESSION_S {
        backoff.reset();
        HEALTHY_RECONNECT_S
    } else {
        backoff.next_delay()
    };
    log::info!("Hermes: disconnected; reconnecting in {delay:.0}s");
    sleep_or_stop(state, delay).await;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use goblin_store::profiles::{Pairing, Secret};
    use goblin_store::{Layout, ProfileStore};
    use hermes_link::testsupport::{FakeHermes, Script, Tls, CODE, FINGERPRINT, TOKEN};
    use tokio::sync::Notify;

    use super::Paired;

    /// [`super::pair`] as a handheld called Brick, at version 0, that nobody stops.
    async fn pair(
        profile: &goblin_store::profiles::Profile,
        link: &hermes_link::PairLink,
    ) -> Result<Paired, String> {
        pair_until(profile, link, &Notify::new()).await
    }

    async fn pair_until(
        profile: &goblin_store::profiles::Profile,
        link: &hermes_link::PairLink,
        stop: &Notify,
    ) -> Result<Paired, String> {
        super::pair(profile, "Brick".into(), "0".into(), link, stop).await
    }

    #[tokio::test]
    async fn pairing_saves_a_record_the_profile_keeps_to_itself() {
        let fake = FakeHermes::start(Script::default(), Tls::On).await;
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(Layout::new(dir.path().join("data"))).unwrap();
        let profile = store.create("hermes").unwrap();

        // Another machine at that address: nothing is saved.
        let stranger = fake.link(&FINGERPRINT.replace('e', "f"), CODE);
        let refused = pair(&profile, &stranger).await.unwrap_err();
        assert!(refused.contains("not the Hermes"), "{refused}");
        assert!(profile.hermes_pairing().unwrap().is_none());

        let paired = pair(&profile, &fake.link(FINGERPRINT, CODE)).await;
        assert_eq!(paired, Ok(Paired::Saved));
        // It is paired as itself: the id is the profile's own, and it is the one kept to
        // say hello with.
        let own = profile.identity().unwrap().node_id();
        let saved = profile.hermes_pairing().unwrap().unwrap();
        assert_eq!(saved.token.expose(), TOKEN);
        assert_eq!((saved.device_id.as_str(), saved.port), (&*own, fake.port));
        assert_eq!(saved.fingerprint, FINGERPRINT);
        let sent = fake.received.lock().unwrap().last().unwrap().clone();
        assert_eq!(sent["type"], "pair");
        assert_eq!(
            sent["device"],
            serde_json::json!({"id": own, "name": "Brick", "version": "0"})
        );
        // The commands are not sent while pairing: Hermes learns them at hello.
        assert!(sent.get("commands").is_none());

        // The code is spent: a second use is refused in Hermes's words.
        let other = store.create("other").unwrap();
        let again = pair(&other, &fake.link(FINGERPRINT, CODE)).await;
        assert!(again.unwrap_err().contains("spent"));
    }

    #[tokio::test]
    async fn a_pairing_the_owner_stops_keeps_nothing() {
        // Hermes takes the pairing, and its answer is on the way for as long as the test
        // holds it.
        let held = Arc::new(tokio::sync::Semaphore::new(0));
        let script = Script {
            pair_held: Some(held.clone()),
            ..Script::default()
        };
        let fake = FakeHermes::start(script, Tls::On).await;
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(Layout::new(dir.path().join("data"))).unwrap();
        let profile = store.create("hermes").unwrap();
        let own = profile.identity().unwrap().node_id();
        let stop = Arc::new(Notify::new());

        // Stopped before anything was sent: the code is not spent.
        stop.notify_one();
        let stopped = pair_until(&profile, &fake.link(FINGERPRINT, CODE), &stop).await;
        assert_eq!(stopped, Ok(Paired::Stopped));
        assert!(fake.received.lock().unwrap().is_empty());

        // Stopped once Hermes has made out a token for it: the handheld has none of it.
        let pairing = {
            let (profile, link, stop) =
                (profile.clone(), fake.link(FINGERPRINT, CODE), stop.clone());
            tokio::spawn(async move { pair_until(&profile, &link, &stop).await })
        };
        while !fake.devices.lock().unwrap().contains_key(&own) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        stop.notify_one();
        assert_eq!(pairing.await.unwrap(), Ok(Paired::Stopped));
        held.add_permits(1);
        assert!(profile.hermes_pairing().unwrap().is_none());
        assert_eq!(super::blocks_pairing(&profile), None);
    }

    #[test]
    fn a_hermes_profile_is_unpaired_before_it_is_paired_with_anything() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(Layout::new(dir.path().join("data"))).unwrap();
        let profile = store.create("hermes").unwrap();
        assert_eq!(super::blocks_pairing(&profile), None);
        profile
            .save_hermes_pairing(&goblin_store::profiles::HermesPairing {
                hosts: vec!["h".into()],
                port: 1,
                fingerprint: FINGERPRINT.into(),
                device_id: "dev-1".into(),
                token: Secret::new(TOKEN),
            })
            .unwrap();
        assert!(super::blocks_pairing(&profile)
            .unwrap()
            .contains("Unpair it first"));
        // A record that cannot be read is still a record: not a licence to pair again.
        std::fs::write(profile.dir().join("hermes.json"), "{broken").unwrap();
        assert!(super::blocks_pairing(&profile)
            .unwrap()
            .contains("cannot be read"));
        // Unpairing clears it either way.
        profile.delete_hermes_pairing().unwrap();
        assert_eq!(super::blocks_pairing(&profile), None);
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
        let refused = pair(&profile, &fake.link(FINGERPRINT, CODE)).await;
        assert!(refused.unwrap_err().contains("Unpair it first"));
        assert!(profile.hermes_pairing().unwrap().is_none());
        assert!(
            fake.received.lock().unwrap().is_empty(),
            "the code was not spent"
        );
    }
}
