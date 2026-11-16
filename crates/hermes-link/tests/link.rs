//! The link against a scripted plugin: pairing, the pinned certificate, turns, command calls.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use hermes_link::testsupport::{
    connect_plain, pair_plain, FakeHermes, Script, Tls, CODE, FINGERPRINT, TOKEN,
};
use hermes_link::{
    Device, LinkError, Outcome, PairLink, Payload, Pushed, Runner, Session, TurnEvent,
};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

fn device() -> Device {
    Device {
        id: "dev-1".into(),
        name: "Brick".into(),
        version: "0".into(),
        commands: json!({"device.health": {"description": "How the handheld is"}}),
    }
}

/// Answers every command with its own name and params.
struct Echo;

impl Runner for Echo {
    fn run(
        &self,
        command: String,
        params: Map<String, Value>,
        _timeout_ms: Option<i64>,
    ) -> Pin<Box<dyn Future<Output = Map<String, Value>> + Send>> {
        Box::pin(async move {
            let mut m = Map::new();
            m.insert("ok".into(), json!(true));
            m.insert(
                "payload".into(),
                json!({"command": command, "params": params}),
            );
            m
        })
    }
}

fn pushed() -> (mpsc::Sender<Pushed>, mpsc::Receiver<Pushed>) {
    mpsc::channel(8)
}

#[test]
fn a_pasted_link_is_read_or_refused() {
    let link = PairLink::parse(&format!(
        " goblinlink://brick-host.tail1234.ts.net,192.168.1.20:47832/?fp={}&code=ABCD2345ABCD2345ABCD\n",
        FINGERPRINT.to_uppercase()
    ))
    .unwrap();
    assert_eq!(link.hosts, ["brick-host.tail1234.ts.net", "192.168.1.20"]);
    assert_eq!(
        (link.port, link.code.as_str()),
        (47832, "ABCD2345ABCD2345ABCD")
    );
    assert_eq!(link.fingerprint, FINGERPRINT);
    for bad in [
        "https://example.com/?fp=00&code=A",
        "goblinlink://host/?fp=00&code=ABCD",
        "goblinlink://host:0/?fp=00&code=ABCD",
        "goblinlink://host:99999/?fp=00&code=ABCD",
        "goblinlink://ho st:1/?fp=00&code=ABCD",
        "goblinlink://host:1/?fp=zz&code=ABCD",
        &format!("goblinlink://host:1/?fp={FINGERPRINT}"),
        &format!("goblinlink://host:1/?fp={FINGERPRINT}&code=a b"),
        &format!("goblinlink://user@host:1/?fp={FINGERPRINT}&code=ABCD"),
        "",
    ] {
        assert!(
            matches!(PairLink::parse(bad), Err(LinkError::BadLink(_))),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn pairing_spends_the_code_and_keeps_the_token() {
    let fake = FakeHermes::start(Script::default(), Tls::Off).await;
    let pairing = pair_plain(&fake.link(FINGERPRINT, CODE), &device())
        .await
        .unwrap();
    assert_eq!(pairing.token, TOKEN);
    assert_eq!(pairing.device_id, "dev-1");
    assert!(
        !format!("{pairing:?}").contains(TOKEN),
        "the token is never printed"
    );
    // The same code a second time, and a wrong one, are refused in the plugin's words.
    for code in [CODE, "WRONG999WRONG999WRONG999"] {
        let again = pair_plain(&fake.link(FINGERPRINT, code), &device()).await;
        assert!(matches!(again, Err(LinkError::Refused(r)) if r.contains("spent")));
    }
}

#[tokio::test]
async fn only_the_paired_certificate_is_trusted() {
    let fake = FakeHermes::start(Script::default(), Tls::On).await;
    // The right fingerprint: TLS, the fixture's certificate, and the pairing goes through.
    let paired = hermes_link::pair(&fake.link(FINGERPRINT, CODE), &device()).await;
    assert_eq!(paired.unwrap().token, TOKEN);
    // Any other fingerprint: nothing is sent to whoever answered.
    let before = fake.received.lock().unwrap().len();
    let other = FINGERPRINT.replace('e', "f");
    let refused = hermes_link::pair(&fake.link(&other, CODE), &device()).await;
    assert!(
        matches!(refused, Err(LinkError::WrongCertificate)),
        "{refused:?}"
    );
    assert_eq!(fake.received.lock().unwrap().len(), before);
    let (tx, _rx) = pushed();
    let mut pairing = fake.pairing();
    pairing.fingerprint = other;
    let session = Session::connect(&pairing, &device(), Arc::new(Echo), tx).await;
    assert!(matches!(session, Err(LinkError::WrongCertificate)));
    assert_eq!(
        fake.received.lock().unwrap().len(),
        before,
        "no token left the handheld"
    );
}

#[tokio::test]
async fn a_turn_streams_and_ends() {
    let fake = FakeHermes::start(
        Script {
            turn_events: vec![
                json!({"type": "status", "text": "Thinking", "working": true}),
                json!({"type": "message", "message_id": "a1", "text": "Hel", "done": false}),
                json!({"type": "message", "message_id": "a1", "text": "Hello", "done": true}),
                json!({"type": "turn.done"}),
            ],
            reject: None,
            silent: false,
            ..Script::default()
        },
        Tls::Off,
    )
    .await;
    let (tx, _rx) = pushed();
    let session = connect_plain(&fake.pairing(), &device(), Arc::new(Echo), tx)
        .await
        .unwrap();
    assert_eq!(session.agent().as_deref(), Some("Hermes"));
    let hello = fake.received.lock().unwrap()[0].clone();
    assert_eq!(
        hello["commands"]["device.health"]["description"],
        "How the handheld is"
    );

    let mut turn = session.send("main", Payload::Text("hi")).await.unwrap();
    assert_eq!(turn.message_id, "u1");
    let mut seen = Vec::new();
    while let Some(event) = turn.next(Duration::from_secs(5)).await.unwrap() {
        let last = event == TurnEvent::Done;
        seen.push(event);
        if last {
            break;
        }
    }
    assert_eq!(
        seen,
        [
            TurnEvent::Status {
                text: "Thinking".into(),
                working: true
            },
            TurnEvent::Message {
                message_id: "a1".into(),
                text: "Hel".into(),
                done: false
            },
            TurnEvent::Message {
                message_id: "a1".into(),
                text: "Hello".into(),
                done: true
            },
            TurnEvent::Done,
        ]
    );
    // A voice note travels as its WAV.
    let _voice = session.send("c-1", Payload::Voice(b"RIFF")).await.unwrap();
    let sent = fake.received.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        (sent["chat"].as_str(), sent["voice_wav_b64"].as_str()),
        (Some("c-1"), Some("UklGRg=="))
    );
}

#[tokio::test]
async fn a_refused_turn_and_a_wrong_token_say_why() {
    let fake = FakeHermes::start(
        Script {
            turn_events: Vec::new(),
            reject: Some("busy".into()),
            silent: false,
            ..Script::default()
        },
        Tls::Off,
    )
    .await;
    let (tx, _rx) = pushed();
    let session = connect_plain(&fake.pairing(), &device(), Arc::new(Echo), tx.clone())
        .await
        .unwrap();
    let refused = session.send("main", Payload::Text("hi")).await;
    assert!(matches!(refused, Err(LinkError::Refused(r)) if r == "busy"));
    let mut pairing = fake.pairing();
    pairing.token = "someone-elses-token-0000000000000000000".into();
    let unknown = connect_plain(&pairing, &device(), Arc::new(Echo), tx).await;
    assert!(
        matches!(unknown, Err(LinkError::Unpaired)),
        "the daemon can tell it must pair again"
    );
}

#[tokio::test]
async fn hermes_calls_a_command_and_asks_the_owner() {
    let fake = FakeHermes::start(Script::default(), Tls::Off).await;
    let (tx, mut rx) = pushed();
    let session = connect_plain(&fake.pairing(), &device(), Arc::new(Echo), tx)
        .await
        .unwrap();
    fake.push
        .send(
            json!({"type": "tool.call", "id": "c1", "command": "file.read",
                     "params": {"path": "/x"}, "timeout_ms": 1000}),
        )
        .unwrap();
    fake.push
        .send(
            json!({"type": "prompt", "id": "p1", "chat": "main", "kind": "approval",
                     "text": "Run rm -rf build?\u{7}",
                     "choices": [{"id": "once", "label": "Once"}, {"id": "deny", "label": "No"}]}),
        )
        .unwrap();
    // A question that arrives as it was sent, and one too long to keep whole.
    fake.push
        .send(
            json!({"type": "prompt", "id": "p2", "chat": "main", "kind": "approval",
                     "text": "Run this?\nls \u{2014} \u{26a0}\u{fe0f}", "choices": []}),
        )
        .unwrap();
    fake.push
        .send(
            json!({"type": "prompt", "id": "p3", "chat": "main", "kind": "clarify",
                     "text": "x".repeat(2001), "choices": []}),
        )
        .unwrap();
    // Text taken out because it hides or reorders what is read is lost too.
    fake.push
        .send(
            json!({"type": "prompt", "id": "p4", "chat": "main", "kind": "approval",
                     "text": "echo \u{202e}dne", "choices": []}),
        )
        .unwrap();
    fake.push
        .send(json!({"type": "prompt", "id": "p5", "chat": "main", "kind": "approval", "text": 7}))
        .unwrap();
    // Odd messages are dropped, not fatal.
    for odd in [
        json!({"type": "tool.call"}),
        json!({"type": 7}),
        json!([1, 2]),
        json!({"type": "message", "turn": "t9"}),
    ] {
        fake.push.send(odd).unwrap();
    }
    let prompt = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        prompt,
        Pushed::Prompt {
            id: "p1".into(),
            chat: "main".into(),
            kind: "approval".into(),
            text: "Run rm -rf build?".into(),
            // A character was taken out of it (the bell), so it is not what was sent.
            cut: true,
            choices: vec![("once".into(), "Once".into()), ("deny".into(), "No".into())],
        }
    );
    let mut more = Vec::new();
    for _ in 0..4 {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(Pushed::Prompt { id, text, cut, .. })) => {
                more.push((id, text.chars().count(), cut))
            }
            other => panic!("expected a question, got {other:?}"),
        }
    }
    assert_eq!(
        more,
        [
            ("p2".to_owned(), 17, false),
            ("p3".to_owned(), 2000, true),
            ("p4".to_owned(), 8, true),
            ("p5".to_owned(), 0, true),
        ]
    );
    session.answer("p1", "deny");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let (mut result, mut answer) = (None, None);
    while (result.is_none() || answer.is_none()) && tokio::time::Instant::now() < deadline {
        for frame in fake.received.lock().unwrap().iter() {
            match frame["type"].as_str() {
                Some("tool.result") => result = Some(frame.clone()),
                Some("prompt.answer") => answer = Some(frame.clone()),
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let result = result.expect("the command's result came back");
    assert_eq!(result["id"], "c1");
    assert_eq!(result["result"]["payload"]["command"], "file.read");
    assert_eq!(result["result"]["payload"]["params"]["path"], "/x");
    assert_eq!(answer.unwrap()["answer"], "deny");

    session.stop();
    assert_eq!(session.wait_closed().await, Outcome::Stopped);
    assert!(matches!(
        session.send("main", Payload::Text("late")).await,
        Err(LinkError::Closed)
    ));
}

/// A question belongs to a turn, so the session says which chats have one under way: from
/// the moment a message is sent until Hermes ends the turn or the turn is let go of. And
/// Hermes can say a question is over.
#[tokio::test]
async fn a_session_knows_where_a_turn_is_under_way_and_hears_a_question_closed() {
    let script = Script {
        turn_events: vec![json!({"type": "status", "text": "Thinking", "working": true})],
        ..Script::default()
    };
    let fake = FakeHermes::start(script, Tls::Off).await;
    let (tx, mut rx) = pushed();
    let session = connect_plain(&fake.pairing(), &device(), Arc::new(Echo), tx)
        .await
        .unwrap();
    assert!(!session.turn_in("main"));
    let mut turn = session.send("main", Payload::Text("hi")).await.unwrap();
    assert!(session.turn_in("main"));
    assert!(
        !session.turn_in("side"),
        "another chat's turn is not this one's"
    );

    // A closed question is passed on by its id; one without a usable id is dropped.
    for closed in [
        json!("p1"),
        json!("../p2"),
        json!(7),
        json!(null),
        json!("p3"),
    ] {
        fake.push
            .send(json!({"type": "prompt.closed", "id": closed}))
            .unwrap();
    }
    for wanted in ["p1", "p3"] {
        let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap();
        assert_eq!(got, Some(Pushed::PromptClosed { id: wanted.into() }));
    }

    // Hermes ends the turn: it is over as soon as the link has heard so.
    let wire_turn = {
        let received = fake.received.lock().unwrap();
        let sent = received.iter().find(|f| f["type"] == "turn");
        sent.expect("the turn was sent")["id"].clone()
    };
    fake.push
        .send(json!({"type": "turn.done", "turn": wire_turn}))
        .unwrap();
    loop {
        match turn.next(Duration::from_secs(5)).await {
            Ok(Some(TurnEvent::Done)) => break,
            Ok(Some(_)) => {}
            other => panic!("the turn did not end: {other:?}"),
        }
    }
    assert!(!session.turn_in("main"));

    // A turn that is let go of before it ends is no longer under way either.
    let other = session.send("side", Payload::Text("hi")).await.unwrap();
    assert!(session.turn_in("side"));
    drop(other);
    assert!(!session.turn_in("side"));
    session.stop();
}

#[tokio::test]
async fn the_certificate_alone_is_not_enough_without_its_key() {
    // Someone who has seen Hermes's certificate serves it with a key of their own. The
    // certificate matches, so it is the handshake's signature that gives them away.
    for tls in [Tls::WrongKey, Tls::WrongKeyTls12] {
        let fake = FakeHermes::start(Script::default(), tls).await;
        let refused = hermes_link::pair(&fake.link(FINGERPRINT, CODE), &device()).await;
        assert!(
            matches!(&refused, Err(LinkError::Unreachable(why)) if why.to_lowercase().contains("signature")),
            "{tls:?}: {refused:?}"
        );
        assert!(
            fake.received.lock().unwrap().is_empty(),
            "{tls:?}: the code never left the handheld"
        );
    }
    // An older TLS is still checked the same way, and works with the right key.
    let old = FakeHermes::start(Script::default(), Tls::Tls12).await;
    let paired = hermes_link::pair(&old.link(FINGERPRINT, CODE), &device()).await;
    assert_eq!(paired.unwrap().token, TOKEN);
}

#[tokio::test]
async fn a_dead_address_does_not_hide_hermes_at_the_next() {
    let hermes = FakeHermes::start(Script::default(), Tls::On).await;
    // Nothing listens on this port at 127.0.0.2; Hermes is at the second address.
    let mut link = hermes.link(FINGERPRINT, CODE);
    link.hosts = vec!["127.0.0.2".into(), "127.0.0.1".into()];
    let paired = hermes_link::pair(&link, &device()).await;
    assert_eq!(
        paired.unwrap().token,
        TOKEN,
        "the second address is still tried"
    );
    // When an address answers with another certificate and none is Hermes, that is said,
    // whichever address it was.
    let other = FINGERPRINT.replace('e', "f");
    let mut link = hermes.link(&other, CODE);
    link.hosts = vec!["127.0.0.1".into(), "127.0.0.2".into()];
    let refused = hermes_link::pair(&link, &device()).await;
    assert!(
        matches!(refused, Err(LinkError::WrongCertificate)),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_turn_nobody_reads_keeps_its_last_reply_and_its_end() {
    // More snapshots than the backlog holds, then the end, all before anything is read.
    let mut turn_events: Vec<Value> = (0..400)
        .map(|n| json!({"type": "message", "message_id": "a1", "text": format!("v{n}"), "done": false}))
        .collect();
    turn_events.push(json!({"type": "message", "message_id": "a1", "text": "final", "done": true}));
    turn_events.push(json!({"type": "turn.done"}));
    let fake = FakeHermes::start(
        Script {
            turn_events,
            reject: None,
            silent: false,
            ..Script::default()
        },
        Tls::Off,
    )
    .await;
    let (tx, _rx) = pushed();
    let session = connect_plain(&fake.pairing(), &device(), Arc::new(Echo), tx)
        .await
        .unwrap();
    let mut turn = session.send("main", Payload::Text("hi")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut seen = Vec::new();
    while let Ok(Some(event)) = turn.next(Duration::from_secs(2)).await {
        seen.push(event);
    }
    let n = seen.len();
    assert!(n < 400, "some snapshots were dropped ({n} kept)");
    assert_eq!(
        seen[n - 2..],
        [
            TurnEvent::Message {
                message_id: "a1".into(),
                text: "final".into(),
                done: true
            },
            TurnEvent::Done
        ]
    );
    assert!(matches!(
        turn.next(Duration::from_millis(50)).await,
        Err(LinkError::Closed)
    ));
}

#[tokio::test]
async fn stopping_does_not_wait_for_a_host_that_went_silent() {
    let fake = FakeHermes::start(
        Script {
            turn_events: Vec::new(),
            reject: None,
            silent: true,
            ..Script::default()
        },
        Tls::Off,
    )
    .await;
    let (tx, _rx) = pushed();
    let session = connect_plain(&fake.pairing(), &device(), Arc::new(Echo), tx)
        .await
        .unwrap();
    session.stop();
    let closed = tokio::time::timeout(Duration::from_secs(2), session.wait_closed()).await;
    assert_eq!(closed.unwrap(), Outcome::Stopped);
    // Dropping the last handle closes a session too.
    let (tx, _rx) = pushed();
    let other = connect_plain(&fake.pairing(), &device(), Arc::new(Echo), tx).await;
    drop(other);
}

/// A command that never finishes.
struct Hang;

impl Runner for Hang {
    fn run(
        &self,
        _command: String,
        _params: Map<String, Value>,
        _timeout_ms: Option<i64>,
    ) -> Pin<Box<dyn Future<Output = Map<String, Value>> + Send>> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn only_so_many_commands_run_at_once() {
    let fake = FakeHermes::start(Script::default(), Tls::Off).await;
    let (tx, _rx) = pushed();
    let _session = connect_plain(&fake.pairing(), &device(), Arc::new(Hang), tx)
        .await
        .unwrap();
    for n in 0..12 {
        fake.push
            .send(
                json!({"type": "tool.call", "id": format!("c{n}"), "command": "system.run",
                         "timeout_ms": -5}),
            )
            .unwrap();
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let busy = loop {
        let busy: Vec<Value> = fake
            .received
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["type"] == "tool.result")
            .cloned()
            .collect();
        if busy.len() >= 4 || tokio::time::Instant::now() >= deadline {
            break busy;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(busy.len(), 4, "eight run, the rest are refused at once");
    assert!(busy.iter().all(|f| f["result"]["ok"] == false));
}

#[tokio::test]
async fn a_slow_reader_still_gets_the_final_reply() {
    let mut turn_events: Vec<Value> = (0..300)
        .map(|n| json!({"type": "message", "message_id": "a1", "text": format!("v{n}"), "done": false}))
        .collect();
    turn_events.push(json!({"type": "message", "message_id": "a1", "text": "final", "done": true}));
    let fake = FakeHermes::start(
        Script {
            turn_events,
            reject: None,
            silent: false,
            ..Script::default()
        },
        Tls::Off,
    )
    .await;
    let (tx, _rx) = pushed();
    let session = connect_plain(&fake.pairing(), &device(), Arc::new(Echo), tx)
        .await
        .unwrap();
    let mut turn = session.send("main", Payload::Text("hi")).await.unwrap();
    // The backlog is full and the final reply is kept aside. The reader takes a few, and
    // only then do a status and the end arrive: they must not push the final reply out.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(turn.next(Duration::from_secs(2)).await.unwrap().unwrap());
    }
    let id = fake
        .received
        .lock()
        .unwrap()
        .iter()
        .find(|f| f["type"] == "turn")
        .unwrap()["id"]
        .clone();
    fake.push
        .send(json!({"type": "status", "turn": id, "text": "Done", "working": false}))
        .unwrap();
    fake.push
        .send(json!({"type": "turn.done", "turn": id}))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    while let Ok(Some(event)) = turn.next(Duration::from_secs(2)).await {
        seen.push(event);
    }
    let n = seen.len();
    assert_eq!(
        seen[n - 2..],
        [
            TurnEvent::Message {
                message_id: "a1".into(),
                text: "final".into(),
                done: true
            },
            TurnEvent::Done
        ],
        "{n} events"
    );
}

#[tokio::test]
async fn stopping_lets_go_of_a_host_that_has_stopped_reading() {
    // The host is there but reads nothing more. Enough is queued for the write to block.
    let fake = FakeHermes::start(
        Script {
            turn_events: Vec::new(),
            reject: None,
            silent: true,
            ..Script::default()
        },
        Tls::Off,
    )
    .await;
    let (tx, _rx) = pushed();
    let runner: Arc<dyn Runner> = Arc::new(Echo);
    let session = connect_plain(&fake.pairing(), &device(), runner.clone(), tx)
        .await
        .unwrap();
    let big = "x".repeat(512 * 1024);
    for n in 0..32 {
        session.answer(&format!("p{n}"), &big);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        Arc::strong_count(&runner) > 1,
        "the connection task is alive"
    );
    session.stop();
    // The task that owns the connection holds the only other runner: when it is gone, so
    // is the task, blocked write or not.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while Arc::strong_count(&runner) > 1 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(Arc::strong_count(&runner), 1, "the connection task ended");
}

#[tokio::test]
async fn a_voice_note_comes_back_as_what_was_heard_and_a_reply_as_speech() {
    let fake = FakeHermes::start(
        Script {
            turn_events: vec![
                json!({"type": "status", "text": "What time is it?", "working": true, "heard": true}),
                json!({"type": "turn.done"}),
            ],
            reject: None,
            silent: false,
            ..Script::default()
        },
        Tls::Off,
    )
    .await;
    let (tx, _rx) = pushed();
    let session = connect_plain(&fake.pairing(), &device(), Arc::new(Echo), tx)
        .await
        .unwrap();
    let mut turn = session.send("main", Payload::Voice(b"RIFF")).await.unwrap();
    let first = turn.next(Duration::from_secs(5)).await.unwrap();
    assert_eq!(first, Some(TurnEvent::Heard("What time is it?".into())));
    // Hermes speaks a reply with its own voice; the handheld gets the audio.
    assert_eq!(session.speech("It is noon.").await.unwrap(), b"ID3\0\0\0");
    let refused = session.speech("").await;
    assert!(matches!(refused, Err(LinkError::Refused(r)) if r == "nothing to say"));
    // Audio that is not MP3 does not reach the handheld's decoder.
    let wav = session.speech("as wav").await;
    assert!(
        matches!(wav, Err(LinkError::Refused(r)) if r.contains("not MP3")),
        "wav"
    );
    assert!(hermes_link::is_mp3(b"ID3\x04") && hermes_link::is_mp3(&[0xFF, 0xFB, 0x90]));
    assert!(
        !hermes_link::is_mp3(b"RIFF") && !hermes_link::is_mp3(&[0xFF]) && !hermes_link::is_mp3(b"")
    );
}

/// The agent's own name and picture: said at hello, changed while connected, and the picture
/// fetched by its revision. What does not look like a picture, or is too large, is refused.
#[tokio::test]
async fn the_agent_says_who_it_is_and_gives_its_picture() {
    use base64::Engine;
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), b"picture"].concat();
    let mut big = png.clone();
    big.resize(1024 * 1024 + 1, 0);
    let fake = FakeHermes::start(
        Script {
            identity: json!({"agent": "Pip", "avatar": {"revision": "r_1"}}),
            avatars: [
                ("r_1".to_owned(), b64(&png)),
                ("r_gif".to_owned(), b64(b"GIF89a, not a PNG")),
                ("r_big".to_owned(), b64(&big)),
            ]
            .into(),
            ..Script::default()
        },
        Tls::Off,
    )
    .await;
    let (tx, mut rx) = pushed();
    let session = connect_plain(&fake.pairing(), &device(), Arc::new(Echo), tx)
        .await
        .unwrap();
    assert_eq!(session.agent().as_deref(), Some("Pip"));
    assert_eq!(session.avatar_revision().as_deref(), Some("r_1"));
    assert_eq!(session.avatar("r_1").await.unwrap(), png);
    for refused in ["r_gone", "r_gif", "r_big"] {
        let answer = session.avatar(refused).await;
        assert!(
            matches!(answer, Err(LinkError::Refused(_))),
            "{refused}: {answer:?}"
        );
    }

    // A change while connected is heard of once; the same again is not news.
    let change = json!({"type": "identity", "agent": "Pop", "avatar": {"revision": "r_2"}});
    fake.push.send(change.clone()).unwrap();
    fake.push.send(change).unwrap();
    let heard = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
    assert_eq!(heard.unwrap(), Some(Pushed::Identity));
    assert_eq!(session.agent().as_deref(), Some("Pop"));
    assert_eq!(session.avatar_revision().as_deref(), Some("r_2"));
    // A revision names a folder on the handheld: one that could leave it is no revision.
    fake.push
        .send(json!({"type": "identity", "agent": "Pop", "avatar": {"revision": "../r_3"}}))
        .unwrap();
    let heard = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
    assert_eq!(heard.unwrap(), Some(Pushed::Identity));
    assert_eq!(session.avatar_revision(), None);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err(),
        "the repeated change was not news"
    );
}
