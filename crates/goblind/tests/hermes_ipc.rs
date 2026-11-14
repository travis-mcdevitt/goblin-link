//! A profile paired with a Hermes: the daemon's own connection loop against a scripted
//! plugin, over TLS with the pinned certificate, seen from a goblin-ipc client.

use std::future::pending;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use goblin_ipc::client::{Client, ClientOptions};
use goblin_ipc::paths;
use goblin_ipc::server::{EventBus, Server, ServerConfig};
use goblin_ipc::types::*;
use hermes_link::testsupport::{FakeHermes, Script, Tls, FINGERPRINT, TOKEN};
use muse_link::{random_uuid, DeviceDescription};

use goblind::daemon::{build_state, cache_limits, DaemonHandler, DaemonState, Stores};
use goblind::exec::{Executor, HealthFn, RunAs};
use goblind::service;

use goblin_store::chats::ChatCache;
use goblin_store::commands::{CommandLog, ConsentStore};
use goblin_store::inbox::Inbox;
use goblin_store::profiles::{HermesPairing, ProfileStore, Secret};
use goblin_store::settings::SettingsStore;
use goblin_store::Layout;

struct Harness {
    state: Arc<DaemonState>,
    client: Client,
    fake: FakeHermes,
    root: std::path::PathBuf,
    _tmp: tempfile::TempDir,
}

/// A daemon whose only profile is paired with `fake`, holding `token`.
async fn start(script: Script, token: &str) -> Harness {
    let fake = FakeHermes::start(script, Tls::On).await;
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&data).unwrap();
    let layout = Layout::new(&data);
    let profiles = ProfileStore::open(layout.clone()).unwrap();
    profiles.create("test").unwrap();
    profiles.set_active("test").unwrap();
    let profile = profiles.profile("test").unwrap();
    profile
        .save_hermes_pairing(&HermesPairing {
            hosts: vec!["127.0.0.1".into()],
            port: fake.port,
            fingerprint: FINGERPRINT.into(),
            device_id: "dev-1".into(),
            token: Secret::new(token),
        })
        .unwrap();
    let settings = SettingsStore::open(&layout.settings_file()).unwrap();
    // A new profile's switches: files in its own folder, no shell.
    let consent = ConsentStore::open(&profile.consent_file()).unwrap();
    let stores = Stores {
        chats: ChatCache::open(&profile.chats_dir(), cache_limits()).unwrap(),
        inbox: Inbox::open(&profile.notifications_file(), 200).unwrap(),
        commands: CommandLog::open(&profile.commands_file(), 100).unwrap(),
    };
    let health: HealthFn = Arc::new(|| json!({"version": "test", "uptime_s": 1}));
    let homes = tmp.path().join("homes");
    std::fs::create_dir_all(homes.join("test")).unwrap();
    let rules = goblind::exec::scope::FileRules {
        home: homes.join("test"),
        homes,
        storage: Some(tmp.path().join("card")),
        denied: vec![data.clone()],
    };
    let executor = Arc::new(Executor::new(RunAs::current(), Vec::new(), health).with_rules(rules));
    let device = DeviceDescription {
        node_id: "dev-1".into(),
        display_name: "test".into(),
        version: "0".into(),
        commands: executor.specs(),
    };
    let bus = Arc::new(EventBus::new(random_uuid()));
    let state = build_state(
        "0".into(),
        bus.clone(),
        profiles,
        None,
        settings,
        consent,
        "test".into(),
        stores,
        executor,
        device,
    );
    state.set_homes_dir(tmp.path().join("homes"));
    tokio::spawn(service::run(state.clone()));
    let socket = paths::socket_path(&runtime, "test").unwrap();
    let handler = Arc::new(DaemonHandler::new(state.clone()));
    let server = Server::bind(&socket, handler, bus, ServerConfig::default()).unwrap();
    tokio::spawn(async move {
        let _ = server.run(pending::<()>()).await;
    });
    let client = Client::connect(&socket, ClientOptions::default())
        .await
        .unwrap();
    Harness {
        state,
        client,
        fake,
        root: tmp.path().to_path_buf(),
        _tmp: tmp,
    }
}

async fn link_becomes(h: &Harness, wanted: LinkState) -> Status {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let status: Status = h.client.request(DaemonStatusParams {}).await.unwrap();
        if status.link == wanted {
            return status;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "link is {:?}, wanted {wanted:?}",
            status.link
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The plugin's log of what the handheld sent, once `wanted` of that type are there.
async fn received(h: &Harness, kind: &str, wanted: usize) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let found: Vec<Value> = h
            .fake
            .received
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["type"] == kind)
            .cloned()
            .collect();
        if found.len() >= wanted {
            return found;
        }
        assert!(tokio::time::Instant::now() < deadline, "no {kind} arrived");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

fn reply_script() -> Script {
    Script {
        turn_events: vec![
            json!({"type": "status", "text": "Thinking", "working": true}),
            json!({"type": "message", "message_id": "a1", "text": "Hello there", "done": true}),
            json!({"type": "turn.done"}),
        ],
        reject: None,
        silent: false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hermes_profile_links_chats_and_says_who_it_is() {
    let h = start(reply_script(), TOKEN).await;
    let status = link_becomes(&h, LinkState::Online).await;
    assert!(status.paired);
    // Hermes was told the handheld's commands as the daemon describes them.
    let hello = received(&h, "hello", 1).await.remove(0);
    assert_eq!(hello["device"], "dev-1");
    assert!(hello["commands"]["file.read"].is_object(), "{hello}");
    assert!(hello["commands"]["system.run"].is_object());

    h.client
        .subscribe(SubscribeParams {
            after_seq: None,
            topics: Some(vec![Topic::Chat]),
        })
        .await
        .unwrap();
    let sent: ChatSendResult = h
        .client
        .request(ChatSendParams {
            chat: "main".into(),
            text: "hi".into(),
        })
        .await
        .unwrap();
    assert_eq!(sent.message_id.as_deref(), Some("u1"));
    let (mut reply, mut last_turn) = (String::new(), String::new());
    while let Ok(Some(ev)) =
        tokio::time::timeout(Duration::from_secs(8), h.client.next_event()).await
    {
        if ev.event == "chat.message" && ev.data["message"]["role"] == "assistant" {
            reply = ev.data["message"]["text"].as_str().unwrap_or("").to_owned();
        } else if ev.event == "chat.turn" && ev.data["turn"] == json!(sent.turn) {
            last_turn = ev.data["state"].as_str().unwrap_or("").to_owned();
            if last_turn == "done" {
                break;
            }
        }
    }
    assert_eq!(
        (reply.as_str(), last_turn.as_str()),
        ("Hello there", "done")
    );
    let turn = received(&h, "turn", 1).await.remove(0);
    assert_eq!(
        (turn["chat"].as_str(), turn["text"].as_str()),
        (Some("main"), Some("hi"))
    );
    // A Hermes keeps no history for the handheld; its own copy of the chat is the record.
    let history: ChatHistoryResult = h
        .client
        .request(ChatHistoryParams {
            chat: "main".into(),
            before: None,
            limit: None,
        })
        .await
        .unwrap();
    let texts: Vec<&str> = history.messages.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(texts, ["hi", "Hello there"]);
    // And has no live call.
    let call = h.client.request(VoiceCallStartParams { chat: None }).await;
    assert!(call.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hermes_is_held_to_the_profiles_switches_like_any_agent() {
    let h = start(Script::default(), TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    let inside = h.root.join("homes/test/note.txt");
    std::fs::write(&inside, "mine").unwrap();
    let token_file = h.root.join("data/profiles/test/hermes.json");
    let calls = [
        ("c1", "file.read", json!({"path": inside})),
        // The shell is off for a new profile, whoever asks.
        ("c2", "system.run", json!({"command": "id"})),
        // The pairing with Hermes itself is in the daemon's data: never readable.
        ("c3", "file.read", json!({"path": token_file})),
        // Outside the profile's own folder.
        ("c4", "file.read", json!({"path": "/etc/hostname"})),
    ];
    for (id, command, params) in &calls {
        h.fake
            .push
            .send(json!({"type": "tool.call", "id": id, "command": command, "params": params}))
            .unwrap();
    }
    let results = received(&h, "tool.result", calls.len()).await;
    let ok = |id: &str| {
        results
            .iter()
            .find(|r| r["id"] == id)
            .map(|r| r["result"]["ok"].clone())
            .unwrap()
    };
    assert_eq!(ok("c1"), json!(true));
    for refused in ["c2", "c3", "c4"] {
        assert_eq!(ok(refused), json!(false), "{refused}: {results:?}");
    }
    assert!(
        !format!("{results:?}").contains(TOKEN),
        "the token never leaves"
    );
    // What Hermes ran is in the command log, as for a Muse.
    let log: CommandsLogResult = h
        .client
        .request(CommandsLogParams { limit: None })
        .await
        .unwrap();
    assert!(log.entries.iter().any(|e| e.name == "file.read"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_question_from_hermes_is_a_card_and_its_answer_goes_back() {
    let h = start(Script::default(), TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    h.fake
        .push
        .send(json!({"type": "prompt", "id": "p1", "chat": "main", "kind": "approval",
                     "text": "Run this command?\nrm -rf build",
                     "choices": [{"id": "once", "label": "Run once"}, {"id": "always", "label": "Always"},
                                 {"id": "deny", "label": "No"}]}))
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let card = loop {
        let list: RequestsListResult = h.client.request(RequestsListParams {}).await.unwrap();
        if let Some(card) = list.items.into_iter().next() {
            break card;
        }
        assert!(tokio::time::Instant::now() < deadline, "no card");
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    assert_eq!(card.id, "prompt:p1");
    assert_eq!(
        (card.heading.as_str(), card.body.as_str()),
        ("Run this command?", "rm -rf build")
    );
    let buttons: Vec<(&str, &str)> = card
        .answers
        .iter()
        .map(|a| (a.id.as_str(), a.label.as_str()))
        .collect();
    assert_eq!(
        buttons,
        [("yes", "Run once"), ("hour", "Always"), ("no", "No")]
    );
    // A button the card does not have answers nothing, and the card stays.
    let wrong = h
        .client
        .request(RequestsDecideParams {
            id: "prompt:p1".into(),
            answer: "maybe".into(),
        })
        .await;
    assert!(wrong.is_err());
    let after: RequestsListResult = h
        .client
        .request(RequestsDecideParams {
            id: "prompt:p1".into(),
            answer: "no".into(),
        })
        .await
        .unwrap();
    assert!(after.items.is_empty());
    let answer = received(&h, "prompt.answer", 1).await.remove(0);
    assert_eq!(
        (answer["id"].as_str(), answer["answer"].as_str()),
        (Some("p1"), Some("deny"))
    );
    // Answered once: a second press finds nothing.
    let again = h
        .client
        .request(RequestsDecideParams {
            id: "prompt:p1".into(),
            answer: "yes".into(),
        })
        .await;
    assert!(again.is_err());
    assert_eq!(received(&h, "prompt.answer", 1).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handheld_hermes_no_longer_knows_is_unpaired_not_retried() {
    let h = start(Script::default(), "a-token-hermes-has-forgotten-0000000000").await;
    let status = link_becomes(&h, LinkState::Unpaired).await;
    assert!(!status.paired);
    let profile = h.state.profiles.profile("test").unwrap();
    assert!(profile.hermes_pairing().unwrap().is_none());
    assert_eq!(
        received(&h, "hello", 1).await.len(),
        1,
        "it said hello once"
    );
}
