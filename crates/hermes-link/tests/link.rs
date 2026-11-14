//! The link against a scripted plugin: pairing, the pinned certificate, turns, command calls.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use hermes_link::testsupport::{
    connect_plain, pair_plain, FakeHermes, Script, CODE, FINGERPRINT, TOKEN,
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
        " goblinlink://brick-host.tail1234.ts.net,192.168.1.20:47832/?fp={}&code=ABCD2345\n",
        FINGERPRINT.to_uppercase()
    ))
    .unwrap();
    assert_eq!(link.hosts, ["brick-host.tail1234.ts.net", "192.168.1.20"]);
    assert_eq!((link.port, link.code.as_str()), (47832, "ABCD2345"));
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
    let fake = FakeHermes::start(Script::default(), false).await;
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
    for code in [CODE, "WRONG999"] {
        let again = pair_plain(&fake.link(FINGERPRINT, code), &device()).await;
        assert!(matches!(again, Err(LinkError::Refused(r)) if r.contains("spent")));
    }
}

#[tokio::test]
async fn only_the_paired_certificate_is_trusted() {
    let fake = FakeHermes::start(Script::default(), true).await;
    // The right fingerprint: TLS, the fixture's certificate, and the pairing goes through.
    let paired = hermes_link::pair(&fake.link(FINGERPRINT, CODE), &device()).await;
    assert_eq!(paired.unwrap().token, TOKEN);
    // Any other fingerprint: nothing is sent to whoever answered.
    let before = fake.received.lock().unwrap().len();
    let other = FINGERPRINT.replace('e', "f");
    let refused = hermes_link::pair(&fake.link(&other, CODE), &device()).await;
    assert!(
        matches!(refused, Err(LinkError::Unreachable(_))),
        "{refused:?}"
    );
    assert_eq!(fake.received.lock().unwrap().len(), before);
    let (tx, _rx) = pushed();
    let mut pairing = fake.pairing();
    pairing.fingerprint = other;
    let session = Session::connect(&pairing, &device(), Arc::new(Echo), tx).await;
    assert!(matches!(session, Err(LinkError::Unreachable(_))));
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
        },
        false,
    )
    .await;
    let (tx, _rx) = pushed();
    let session = connect_plain(&fake.pairing(), &device(), Arc::new(Echo), tx)
        .await
        .unwrap();
    assert_eq!(session.agent(), Some("Hermes"));
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
        },
        false,
    )
    .await;
    let (tx, _rx) = pushed();
    let session = connect_plain(&fake.pairing(), &device(), Arc::new(Echo), tx.clone())
        .await
        .unwrap();
    let refused = session.send("main", Payload::Text("hi")).await;
    assert!(matches!(refused, Err(LinkError::Refused(r)) if r == "busy"));
    let mut pairing = fake.pairing();
    pairing.token = "someone-elses-token-000".into();
    let unknown = connect_plain(&pairing, &device(), Arc::new(Echo), tx).await;
    assert!(matches!(unknown, Err(LinkError::Refused(r)) if r.contains("not paired")));
}

#[tokio::test]
async fn hermes_calls_a_command_and_asks_the_owner() {
    let fake = FakeHermes::start(Script::default(), false).await;
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
            choices: vec![("once".into(), "Once".into()), ("deny".into(), "No".into())],
        }
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
