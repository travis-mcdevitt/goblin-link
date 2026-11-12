//! End-to-end tests of the client against the simulated VM (no real network).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use muse_link::session::SessionConfig;
use muse_link::testsupport::{Script, SimVm};
use muse_link::{CommandRunner, DeviceDescription, LinkSession, MuseTurn, Outcome, TurnEvent};
use serde_json::{json, Map, Value};

/// A command runner that records calls and returns a canned payload.
struct Recorder {
    calls: std::sync::Mutex<Vec<(String, Map<String, Value>)>>,
}

impl Recorder {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::Mutex::new(Vec::new()),
        })
    }
}

impl CommandRunner for Recorder {
    fn run(
        &self,
        command: String,
        params: Map<String, Value>,
        _timeout_ms: Option<i64>,
    ) -> Pin<Box<dyn Future<Output = Map<String, Value>> + Send>> {
        self.calls
            .lock()
            .unwrap()
            .push((command.clone(), params.clone()));
        Box::pin(async move {
            let mut out = Map::new();
            out.insert("ok".into(), json!(true));
            if command == "device.health" {
                out.insert("payload".into(), json!({"version": "test", "uptime_s": 1}));
            } else {
                out.insert("payload".into(), json!({"exit_code": 0, "echo": params}));
            }
            out
        })
    }
}

fn device() -> DeviceDescription {
    DeviceDescription {
        node_id: "homelink-abc123".into(),
        display_name: "Tester".into(),
        version: "0.1.0".into(),
        commands: json!({"device.health": {"description": "health"}}),
    }
}

fn config(sim: &SimVm) -> SessionConfig {
    let mut c = SessionConfig::new(sim.host(), "vm-1", "vm-token", device());
    c.insecure_plaintext = true;
    c
}

#[tokio::test]
async fn registers_with_the_vm() {
    let sim = SimVm::start(Script::default()).await;
    let session = LinkSession::connect(config(&sim), Recorder::new())
        .await
        .expect("connect");
    session.wait_registered().await.expect("registered");
    let node = sim.wait_registered().await;
    assert_eq!(node, "homelink-abc123");
    session.request_stop();
    assert_eq!(session.wait_closed().await, Outcome::Stopped);
}

#[tokio::test]
async fn invoke_round_trip_device_health() {
    let sim = SimVm::start(Script::default()).await;
    let runner = Recorder::new();
    let session = LinkSession::connect(config(&sim), runner.clone())
        .await
        .expect("connect");
    session.wait_registered().await.expect("registered");
    sim.wait_registered().await;

    let result = sim.invoke("device.health", json!({})).await;
    assert_eq!(result["ok"], json!(true));
    assert_eq!(result["payload"]["version"], json!("test"));
    assert_eq!(runner.calls.lock().unwrap()[0].0, "device.health");

    // And a system.run echoing params.
    let result = sim
        .invoke("system.run", json!({"command": "echo hi"}))
        .await;
    assert_eq!(result["payload"]["exit_code"], json!(0));
    assert_eq!(result["payload"]["echo"]["command"], json!("echo hi"));
}

#[tokio::test]
async fn chat_send_then_streamed_reply_then_done() {
    // The VM acknowledges with user message u1 on "main", then streams a reply
    // that is a child of u1, finishing with a terminal task status.
    let script = Script {
        chat_ack: json!({"message_id": "u1", "session_id": "main"}),
        subscribe_events: vec![
            json!({"type":"event","event":"delta.text_append",
                   "payload":{"session_id":"main","message_id":"a1","reply_to_message_id":"u1","text":"Hello"}}),
            json!({"type":"event","event":"delta.message_done",
                   "payload":{"session_id":"main","message_id":"a1","reply_to_message_id":"u1","display_text":"Hello there"}}),
            json!({"type":"event","event":"task.status",
                   "payload":{"session_id":"main","reply_to_message_id":"u1","status":"completed"}}),
        ],
        subscribe_known: true,
    };
    let sim = SimVm::start(script).await;
    let session = LinkSession::connect(config(&sim), Recorder::new())
        .await
        .expect("connect");
    session.wait_registered().await.expect("registered");
    sim.wait_registered().await;

    let mut sub = session
        .subscribe_chat(Some("main"))
        .await
        .expect("subscribe");
    let ack = session
        .send_chat("hi", Some("main"), "text", None)
        .await
        .expect("send");
    assert_eq!(ack["message_id"], json!("u1"));

    let mut turn = MuseTurn::new(Some("main".into()));
    let held = turn.acknowledge(&ack, false, 0.0).unwrap();
    let mut now = 0.0;
    let mut final_text = String::new();
    let mut finished = false;
    for ev in held {
        apply_event(&ev, &mut final_text, &mut finished);
    }
    while !finished {
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), sub.next_event())
            .await
            .expect("event in time")
            .expect("stream open")
            .expect("event ok");
        now += 1.0;
        for ev in turn.feed(&event, now).unwrap() {
            apply_event(&ev, &mut final_text, &mut finished);
        }
    }
    assert_eq!(final_text, "Hello there");
    assert!(turn.settled(now + 2.0));
    sub.close().await;
}

fn apply_event(ev: &TurnEvent, text: &mut String, finished: &mut bool) {
    match ev {
        TurnEvent::Reply(r) => *text = r.text.clone(),
        TurnEvent::Finished(_) => *finished = true,
        TurnEvent::Status(_) => {}
    }
}

#[tokio::test]
async fn unpaired_event_ends_the_session() {
    let sim = SimVm::start(Script::default()).await;
    let session = LinkSession::connect(config(&sim), Recorder::new())
        .await
        .expect("connect");
    session.wait_registered().await.expect("registered");
    sim.wait_registered().await;
    sim.unpair().await;
    assert_eq!(session.wait_closed().await, Outcome::Unpaired);
    assert!(session.is_closed());
}

#[tokio::test]
async fn open_side_chat_reports_absent_when_unknown() {
    let script = Script {
        subscribe_known: false,
        ..Script::default()
    };
    let sim = SimVm::start(script).await;
    let session = LinkSession::connect(config(&sim), Recorder::new())
        .await
        .expect("connect");
    session.wait_registered().await.expect("registered");
    let side = session.open_side_chat("newchat").await.expect("lookup");
    assert!(!side.exists);
}

#[tokio::test]
async fn tts_falls_back_to_legacy_path() {
    let sim = SimVm::start(Script::default()).await;
    let session = LinkSession::connect(config(&sim), Recorder::new())
        .await
        .expect("connect");
    session.wait_registered().await.expect("registered");
    let mut stream = session.stream_tts("m-1").await.expect("tts");
    assert_eq!(stream.status(), 200);
    let mut audio = Vec::new();
    while let Some(chunk) = stream.next_chunk().await {
        audio.extend_from_slice(&chunk.expect("chunk"));
    }
    assert_eq!(audio, b"ID3audio");
}
