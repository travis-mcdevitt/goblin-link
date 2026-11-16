//! Wires `goblind` (with a simulated VM and a temp socket) to a goblin-ipc client:
//! send a chat and observe events; have the VM invoke a command and see it in
//! the command log; and check a consent refusal. All host-side, no real account.

use std::future::pending;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use goblin_ipc::client::{Client, ClientOptions};
use goblin_ipc::paths;
use goblin_ipc::server::{EventBus, Server, ServerConfig};
use goblin_ipc::types::*;
use muse_link::session::SessionConfig;
use muse_link::testsupport::{Script, SimVm};
use muse_link::{random_uuid, DeviceDescription};

use goblind::daemon::{build_state, cache_limits, DaemonHandler, DaemonState, Stores};
use goblind::exec::{Executor, HealthFn, RunAs};
use goblind::service;

use goblin_store::chats::ChatCache;
use goblin_store::commands::{CommandLog, ConsentStore};
use goblin_store::inbox::Inbox;
use goblin_store::profiles::ProfileStore;
use goblin_store::settings::SettingsStore;
use goblin_store::Layout;

struct Harness {
    state: Arc<DaemonState>,
    socket: std::path::PathBuf,
    _tmp: tempfile::TempDir,
    _sim: SimVm,
}

async fn start(script: Script) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&data).unwrap();

    let layout = Layout::new(&data);
    let profiles = ProfileStore::open(layout.clone()).unwrap();
    profiles.create("test").unwrap();
    profiles.set_active("test").unwrap();
    let profile = profiles.profile("test").unwrap();

    let settings = SettingsStore::open(&layout.settings_file()).unwrap();
    // Start with legacy command permissions. Each test adjusts the grants it exercises;
    // writes outside home still require an explicit owner grant.
    std::fs::write(
        profile.consent_file(),
        serde_json::to_vec(&Consent::default()).unwrap(),
    )
    .unwrap();
    let consent = ConsentStore::open(&profile.consent_file()).unwrap();
    let stores = Stores {
        chats: ChatCache::open(&profile.chats_dir(), cache_limits()).unwrap(),
        inbox: Inbox::open(&profile.notifications_file(), 200).unwrap(),
        commands: CommandLog::open(&profile.commands_file(), 100).unwrap(),
    };
    let health: HealthFn = Arc::new(|| json!({"version": "test", "uptime_s": 1}));
    let homes = tmp.path().join("homes");
    std::fs::create_dir_all(homes.join("test")).unwrap();
    std::fs::create_dir_all(homes.join("other")).unwrap();
    let rules = goblind::exec::scope::FileRules {
        home: homes.join("test"),
        homes,
        storage: Some(tmp.path().join("card")),
        denied: vec![data.clone()],
    };
    let executor = Arc::new(Executor::new(RunAs::current(), Vec::new(), health).with_rules(rules));
    let identity = profile.identity().unwrap();
    let device = DeviceDescription {
        node_id: identity.node_id(),
        display_name: "test".into(),
        version: "0".into(),
        commands: executor.specs(),
    };
    let device_for_cfg = device.clone();

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
    state.set_built_dir(tmp.path().join("built"));

    let sim = SimVm::start(script).await;
    let mut cfg = SessionConfig::new(sim.host(), "vm-1", "tok", device_for_cfg);
    cfg.insecure_plaintext = true;

    let session_state = state.clone();
    tokio::spawn(async move {
        service::run_one_session(&session_state, cfg).await;
    });

    let socket = paths::socket_path(&runtime, "test").unwrap();
    let handler = Arc::new(DaemonHandler::new(state.clone()));
    let server = Server::bind(&socket, handler, bus.clone(), ServerConfig::default()).unwrap();
    tokio::spawn(async move {
        let _ = server.run(pending::<()>()).await;
    });

    // Wait for the device to register with the simulated VM.
    tokio::time::timeout(Duration::from_secs(5), sim.wait_registered())
        .await
        .expect("device registers");

    Harness {
        state,
        socket,
        _tmp: tmp,
        _sim: sim,
    }
}

fn script_with_reply() -> Script {
    Script {
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
    }
}

async fn connect(socket: &std::path::Path) -> Client {
    Client::connect(socket, ClientOptions::default())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_send_streams_events_to_an_ipc_client() {
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;
    client
        .subscribe(SubscribeParams {
            after_seq: None,
            topics: Some(vec![Topic::Chat]),
        })
        .await
        .unwrap();

    let result: ChatSendResult = client
        .request(ChatSendParams {
            chat: "main".into(),
            text: "hi".into(),
        })
        .await
        .unwrap();
    assert!(result.turn.starts_with("t-"));

    // Collect chat events until the assistant's final text arrives.
    let mut saw_user = false;
    let mut assistant_text = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while tokio::time::Instant::now() < deadline && assistant_text != "Hello there" {
        let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(8), client.next_event()).await
        else {
            break;
        };
        if ev.event == "chat.message" {
            let msg = &ev.data["message"];
            if msg["role"] == "user" {
                saw_user = true;
            } else if msg["role"] == "assistant" {
                assistant_text = msg["text"].as_str().unwrap_or("").to_owned();
            }
        }
    }
    assert!(saw_user, "the user message was echoed");
    assert_eq!(assistant_text, "Hello there");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_that_creates_its_chat_is_sent_first_and_reported_honestly() {
    // Muse does not know the chat: there is nothing to subscribe to, before or after.
    let h = start(Script {
        chat_ack: json!({"message_id": "u1", "session_id": "c-new"}),
        subscribe_events: Vec::new(),
        subscribe_known: false,
    })
    .await;
    let client = connect(&h.socket).await;
    client
        .subscribe(SubscribeParams {
            after_seq: None,
            topics: Some(vec![Topic::Chat]),
        })
        .await
        .unwrap();
    // The main chat always exists, so there a missing stream is an error and nothing is sent.
    let main = client
        .request(ChatSendParams {
            chat: "main".into(),
            text: "hi".into(),
        })
        .await;
    assert!(main.is_err());

    // A side chat is created by its first message: sent, and then not followable.
    let sent: ChatSendResult = tokio::time::timeout(
        Duration::from_secs(15),
        client.request(ChatSendParams {
            chat: "c-new".into(),
            text: "hi".into(),
        }),
    )
    .await
    .expect("the send returns")
    .unwrap();
    assert_eq!(sent.message_id.as_deref(), Some("u1"));
    let mut saw_user = false;
    let mut last_state = String::new();
    while let Ok(Some(ev)) =
        tokio::time::timeout(Duration::from_millis(500), client.next_event()).await
    {
        if ev.event == "chat.message" && ev.data["message"]["role"] == "user" {
            saw_user = ev.data["message"]["id"] == "u1";
        } else if ev.event == "chat.turn" && ev.data["turn"] == json!(sent.turn) {
            last_state = ev.data["state"].as_str().unwrap_or("").to_owned();
        }
    }
    assert!(saw_user, "the message is shown as sent");
    assert_eq!(last_state, "uncertain", "its answer cannot be followed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_invoke_appears_in_the_command_log() {
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;

    // The VM invokes device.health on the device.
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        h._sim.invoke("device.health", json!({})),
    )
    .await
    .expect("invoke completes");
    assert_eq!(result["ok"], json!(true));
    assert_eq!(result["payload"]["version"], json!("test"));

    // It shows up in the command log over IPC.
    let log: CommandsLogResult = client
        .request(CommandsLogParams { limit: None })
        .await
        .unwrap();
    assert!(log
        .entries
        .iter()
        .any(|e| e.name == "device.health" && matches!(e.state, CommandState::Ok)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reply_buttons_need_no_switch() {
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;
    // Every switch an agent could be held by is off, "Your commands" among them.
    client
        .request(ConsentUpdate {
            shell: Some(false),
            file_read: Some(false),
            file_write: Some(false),
            drop_ins: Some(false),
            builds: Some(BuildAccess::Off),
            ..Default::default()
        })
        .await
        .unwrap();
    let suggest = |params: serde_json::Value| {
        let sim = &h._sim;
        async move {
            tokio::time::timeout(
                Duration::from_secs(5),
                sim.invoke("handheld.suggest", params),
            )
            .await
            .expect("invoke completes")
        }
    };
    let r = suggest(json!({"replies": ["Yes, go ahead", "Not now"]})).await;
    assert_eq!(r["ok"], json!(true), "{r}");
    let history: ChatHistoryResult = client
        .request(ChatHistoryParams {
            chat: "main".into(),
            before: None,
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(history.suggestions, ["Yes, go ahead", "Not now"]);
    // It is still held to what a row of buttons can be, and does nothing else.
    for bad in [
        json!({"replies": "Yes"}),
        json!({"replies": [1, 2]}),
        json!({"replies": ["a", "b", "c", "d", "e", "f", "g"]}),
        json!({}),
    ] {
        let r = suggest(bad.clone()).await;
        assert_eq!(r["ok"], json!(false), "{bad}: {r}");
    }
    // The retired command file's name is no longer a command at all.
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        h._sim
            .invoke("chat.suggest_replies", json!({"replies": ["Yes"]})),
    )
    .await
    .unwrap();
    assert_eq!(r["ok"], json!(false), "{r}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consent_off_refuses_the_command() {
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;

    // Turn shell access off.
    client
        .request(ConsentUpdate {
            shell: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        h._sim.invoke("system.run", json!({"command": "echo hi"})),
    )
    .await
    .expect("invoke completes");
    assert_eq!(result["ok"], json!(false));
    assert!(result["error"].as_str().unwrap().contains("turned off"));

    let log: CommandsLogResult = client
        .request(CommandsLogParams { limit: None })
        .await
        .unwrap();
    assert!(log
        .entries
        .iter()
        .any(|e| e.name == "system.run" && matches!(e.state, CommandState::Refused)));
    let _ = h.state.active();
}

/// The file commands keep to what the profile's consent allows: its own folder at the
/// narrowest setting, and never the data directory or another profile's folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_commands_keep_to_the_profiles_reach() {
    use goblin_ipc::types::FileScope;
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;
    let root = h.socket.parent().unwrap().parent().unwrap().to_path_buf();
    let write = |path: std::path::PathBuf| {
        let sim = &h._sim;
        async move {
            tokio::time::timeout(
                Duration::from_secs(5),
                sim.invoke(
                    "file.write",
                    json!({"path": path, "data_b64": "aGk=", "offset": 0, "final": true,
                           "create_parents": true}),
                ),
            )
            .await
            .expect("invoke completes")
        }
    };

    client
        .request(ConsentUpdate {
            files: Some(FileScope::Home),
            file_write_outside_home: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    let inside = root.join("homes/test/notes/a.txt");
    assert_eq!(write(inside.clone()).await["ok"], json!(true));
    assert_eq!(std::fs::read_to_string(&inside).unwrap(), "hi");
    let outside = root.join("elsewhere.txt");
    let refused = write(outside.clone()).await;
    assert_eq!(refused["ok"], json!(false));
    assert!(refused["error"].as_str().unwrap().contains("outside"));
    assert!(!outside.exists());

    // Any path, yet not the credentials or another profile's folder.
    client
        .request(ConsentUpdate {
            files: Some(FileScope::Device),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(write(outside.clone()).await["ok"], json!(true));
    for path in ["data/profiles/test/sdk_token", "homes/other/a.txt"] {
        let refused = write(root.join(path)).await;
        assert_eq!(refused["ok"], json!(false), "{path}");
        assert!(refused["error"].as_str().unwrap().contains("off limits"));
    }
}

/// Muse installs what it built: refused while the switch is off, held for the owner when
/// they asked to be asked, and at once when installs are free. It removes only its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn muse_installs_and_removes_what_it_built() {
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;
    let root = h.socket.parent().unwrap().parent().unwrap().to_path_buf();
    let invoke = |command: &'static str, params: serde_json::Value| {
        let sim = &h._sim;
        async move {
            tokio::time::timeout(Duration::from_secs(5), sim.invoke(command, params))
                .await
                .expect("invoke completes")
        }
    };
    // What Muse would have written with file.write: a tool that answers with its input.
    let bundle = |home: &str| {
        let dir = root.join("homes").join(home).join("made/echo");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/echo.sh"), "#!/bin/sh\ncat\n").unwrap();
        std::fs::write(
            dir.join("goblin.json"),
            json!({"name": "test.echo", "kind": "tool", "description": "Says it back\n",
                   "run": ["bin/echo.sh"],
                   "required": {"word": {"type": "string", "description": "What to say"}}})
            .to_string(),
        )
        .unwrap();
        dir.to_string_lossy().into_owned()
    };
    let path = bundle("test");

    // Off, as this legacy-consent fixture starts.
    let r = invoke("build.install", json!({"path": path})).await;
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap().contains("turned off"), "{r}");

    // Ask: it waits, the owner is told, and nothing is callable yet.
    client
        .request(ConsentUpdate {
            builds: Some(BuildAccess::Ask),
            ..Default::default()
        })
        .await
        .unwrap();
    let r = invoke("build.install", json!({"path": path})).await;
    assert_eq!(r["payload"]["status"], json!("waiting_for_owner"), "{r}");
    let list: BuildsListResult = client.request(BuildsListParams {}).await.unwrap();
    assert_eq!(list.items.len(), 1);
    assert_eq!(list.items[0].state, BuildState::Pending);
    let notices: NotifyListResult = client
        .request(NotifyListParams { limit: None })
        .await
        .unwrap();
    assert!(notices
        .items
        .iter()
        .any(|n| n.title.starts_with("Install ")));
    let r = invoke("test.echo", json!({"word": "hi"})).await;
    assert_eq!(
        r["ok"],
        json!(false),
        "not installed, so not a command: {r}"
    );

    // Only out of its own folder.
    let r = invoke("build.install", json!({"path": bundle("other")})).await;
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("inside your own folder"),
        "{r}"
    );

    // Asking again under the same name replaces what the owner was shown. An answer to
    // the earlier request installs nothing, from either place the owner can answer.
    let shown = list.items[0]
        .request
        .clone()
        .expect("a waiting item names its request");
    let requests: RequestsListResult = client.request(RequestsListParams {}).await.unwrap();
    let shown_card = requests.items[0].id.clone();
    // The card's words about it are the description without the line break it ended
    // with: a card shows no space at the end of a line, and would not be whole with one.
    assert_eq!(requests.items[0].body, "Says it back");
    assert_eq!(requests.items[0].kind, RequestKind::Approval);
    let r = invoke("build.install", json!({"path": path})).await;
    assert_eq!(r["payload"]["status"], json!("waiting_for_owner"), "{r}");
    let stale = client
        .request(BuildsDecideParams {
            name: "test.echo".into(),
            approve: true,
            request: Some(shown.clone()),
        })
        .await;
    assert!(format!("{stale:?}").contains("changed"), "{stale:?}");
    let stale = client
        .request(RequestsDecideParams {
            id: shown_card,
            answer: "yes".into(),
        })
        .await;
    assert!(format!("{stale:?}").contains("changed"), "{stale:?}");
    let unnamed = client
        .request(BuildsDecideParams {
            name: "test.echo".into(),
            approve: true,
            request: None,
        })
        .await;
    assert!(unnamed.is_err(), "a yes must name what it answers");
    let list: BuildsListResult = client.request(BuildsListParams {}).await.unwrap();
    assert_eq!(list.items.len(), 1);
    assert_eq!(
        list.items[0].state,
        BuildState::Pending,
        "nothing installed"
    );
    let current = list.items[0].request.clone().unwrap();
    assert_ne!(current, shown);

    // The owner approves on the handheld what is now shown.
    let list: BuildsListResult = client
        .request(BuildsDecideParams {
            name: "test.echo".into(),
            approve: true,
            request: Some(current),
        })
        .await
        .unwrap();
    assert_eq!(list.items[0].state, BuildState::Installed);
    assert!(list.items[0].request.is_none());
    // The press is also the owner's answer in the chat. A tool restarts the session, so
    // the answer is kept for the next one.
    let owed = root.join("data/profiles/test/owed-reply.json");
    let note = std::fs::read_to_string(&owed).expect("the answer waits");
    assert!(
        note.contains("Yes, install") && note.contains("test.echo"),
        "{note}"
    );
    assert!(note.contains("\"chat\":\"main\""), "{note}");
    let r = invoke("test.echo", json!({"word": "hi"})).await;
    assert_eq!(r["payload"]["word"], json!("hi"), "{r}");

    // Turning the switch off stops the tool without removing it.
    client
        .request(ConsentUpdate {
            builds: Some(BuildAccess::Off),
            ..Default::default()
        })
        .await
        .unwrap();
    let r = invoke("test.echo", json!({"word": "hi"})).await;
    assert!(r["error"].as_str().unwrap().contains("turned off"), "{r}");

    // Free: installed at once; Muse lists and removes its own.
    client
        .request(ConsentUpdate {
            builds: Some(BuildAccess::Free),
            ..Default::default()
        })
        .await
        .unwrap();
    let r = invoke("build.install", json!({"path": path})).await;
    assert_eq!(r["payload"]["status"], json!("installed"), "{r}");
    let r = invoke("build.list", json!({})).await;
    assert_eq!(r["payload"]["items"][0]["name"], json!("test.echo"), "{r}");
    let r = invoke("build.remove", json!({"name": "test.echo"})).await;
    assert_eq!(r["ok"], json!(true), "{r}");
    let r = invoke("build.remove", json!({"name": "test.echo"})).await;
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("only what it installed"),
        "{r}"
    );
    let list: BuildsListResult = client.request(BuildsListParams {}).await.unwrap();
    assert!(list.items.is_empty());

    // Even a freely installed build keeps its request when the launcher is unavailable.
    std::fs::write(
        std::path::Path::new(&path).join("goblin.json"),
        json!({"name":"retry-game", "kind":"game", "title":"Retry game",
            "description":"test", "run":["bin/echo.sh"]})
        .to_string(),
    )
    .unwrap();
    let result = invoke("build.install", json!({"path":path})).await;
    assert_eq!(result["ok"], false);
    let list: BuildsListResult = client.request(BuildsListParams {}).await.unwrap();
    assert_eq!(list.items.len(), 1);
    assert_eq!(list.items[0].state, BuildState::Pending);
}

/// `handheld.about` is always allowed and says what the profile may do, with what each
/// switch means, following the switches as the owner changes them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn describe_reports_the_permissions_and_what_they_mean() {
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;
    client
        .request(ConsentUpdate {
            shell: Some(false),
            files: Some(FileScope::Home),
            builds: Some(BuildAccess::Ask),
            ..Default::default()
        })
        .await
        .unwrap();
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        h._sim.invoke("handheld.about", json!({})),
    )
    .await
    .expect("invoke completes");
    assert_eq!(r["ok"], json!(true), "{r}");
    let access = &r["payload"]["access"];
    assert_eq!(access["profile"], json!("test"));
    assert_eq!(access["shell"]["on"], json!(false));
    assert!(access["shell"]["means"]
        .as_str()
        .unwrap()
        .contains("system.run"));
    assert_eq!(access["file_reach"]["value"], json!("own_folder"));
    assert_eq!(access["install_builds"]["value"], json!("ask"));
    assert_eq!(access["read_files"]["on"], json!(true));
    assert!(r["payload"]["device"]["arch"].is_string());
    // Nothing that identifies the unit or names another profile.
    let text = r["payload"].to_string();
    assert!(!text.contains("homes/other"), "{text}");

    // Listing a folder reaches as far as reading does, and no further.
    let root = h.socket.parent().unwrap().parent().unwrap().to_path_buf();
    std::fs::write(root.join("homes/test/a.txt"), "hello").unwrap();
    std::fs::create_dir_all(root.join("homes/test/sub")).unwrap();
    let list = |path: std::path::PathBuf| {
        let sim = &h._sim;
        async move {
            tokio::time::timeout(
                Duration::from_secs(5),
                sim.invoke("file.list", json!({"path": path.to_string_lossy()})),
            )
            .await
            .expect("invoke completes")
        }
    };
    let r = list(root.join("homes/test")).await;
    let entries = r["payload"]["entries"].as_array().expect("entries").clone();
    assert!(
        entries.contains(&json!({"name": "a.txt", "type": "file", "size": 5})),
        "{r}"
    );
    assert!(
        entries.contains(&json!({"name": "sub", "type": "folder"})),
        "{r}"
    );
    let r = list(root.join("homes/other")).await;
    assert_eq!(r["ok"], json!(false), "another profile's folder: {r}");
    let r = list(root.join("homes")).await;
    assert_eq!(r["ok"], json!(false), "outside its own folder: {r}");
    client
        .request(ConsentUpdate {
            file_read: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
    let r = list(root.join("homes/test")).await;
    assert!(r["error"].as_str().unwrap().contains("turned off"), "{r}");
    assert!(
        text.contains("homes/test"),
        "its own folder is named: {text}"
    );
}

/// Muse sees the settings and asks for a change; the owner's answer decides it. A widened
/// permission can be given for an hour and then goes back; the widest cannot be asked for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn muse_asks_for_a_setting_and_the_owner_answers() {
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;
    let root = h.socket.parent().unwrap().parent().unwrap().to_path_buf();
    client
        .request(ConsentUpdate {
            shell: Some(false),
            files: Some(FileScope::Home),
            ..Default::default()
        })
        .await
        .unwrap();
    let invoke = |command: &'static str, params: serde_json::Value| {
        let sim = &h._sim;
        async move {
            tokio::time::timeout(Duration::from_secs(5), sim.invoke(command, params))
                .await
                .expect("invoke completes")
        }
    };
    let value_of = |report: &serde_json::Value, id: &str| {
        report["payload"]["settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .cloned()
            .unwrap()
    };

    let report = invoke("handheld.settings", json!({})).await;
    let reach = value_of(&report, "file_reach");
    assert_eq!(reach["value"], json!("own_folder"));
    assert_eq!(reach["you_may_ask_for"], json!(["own_folder", "storage"]));
    assert_eq!(
        value_of(&report, "shell")["you_may_ask_for"],
        json!(["off"])
    );

    // The widest cannot be asked for, and asking changes nothing.
    let r = invoke(
        "handheld.ask",
        json!({"setting": "shell", "value": "on", "reason": "I would like it"}),
    )
    .await;
    assert!(
        r["error"].as_str().unwrap().contains("owner's alone"),
        "{r}"
    );
    let ask = json!({"setting": "file_reach", "value": "storage", "reason": "To read your saves"});
    let r = invoke("handheld.ask", ask.clone()).await;
    assert_eq!(r["payload"]["status"], json!("waiting_for_owner"), "{r}");
    let consent: Consent = client.request(ConsentGetParams {}).await.unwrap();
    assert_eq!(consent.files, FileScope::Home, "asking changes nothing");

    // The owner sees it, with Muse's reason, and may give it for an hour.
    let list: RequestsListResult = client.request(RequestsListParams {}).await.unwrap();
    assert_eq!(list.items.len(), 1);
    assert_eq!(list.items[0].id, "setting:file_reach");
    assert_eq!(list.items[0].body, "To read your saves");
    let answers: Vec<&str> = list.items[0]
        .answers
        .iter()
        .map(|a| a.id.as_str())
        .collect();
    assert_eq!(answers, ["no", "yes", "hour"]);
    let list: RequestsListResult = client
        .request(RequestsDecideParams {
            id: "setting:file_reach".into(),
            answer: "hour".into(),
        })
        .await
        .unwrap();
    assert!(list.items.is_empty());
    let consent: Consent = client.request(ConsentGetParams {}).await.unwrap();
    assert_eq!(consent.files, FileScope::Storage);
    let report = invoke("handheld.settings", json!({})).await;
    let reach = value_of(&report, "file_reach");
    assert_eq!(reach["for_a_while"]["goes_back_to"], json!("own_folder"));

    // When the hour is up it goes back (the time is moved, not waited for).
    let timed_file = root.join("data/timed-settings.json");
    let mut timed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&timed_file).unwrap()).unwrap();
    timed[0]["until"] = json!(1);
    std::fs::write(&timed_file, timed.to_string()).unwrap();
    // Atomic writes need the parent directory to be writable.
    use std::os::unix::fs::PermissionsExt;
    let profile_dir = root.join("data/profiles/test");
    let permissions = std::fs::metadata(&profile_dir).unwrap().permissions();
    std::fs::set_permissions(&profile_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    h.state.revert_due();
    std::fs::set_permissions(&profile_dir, permissions).unwrap();
    let consent: Consent = client.request(ConsentGetParams {}).await.unwrap();
    assert_eq!(consent.files, FileScope::Storage);
    let kept: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&timed_file).unwrap()).unwrap();
    assert_eq!(kept, timed);
    h.state.revert_due();
    let consent: Consent = client.request(ConsentGetParams {}).await.unwrap();
    assert_eq!(consent.files, FileScope::Home);
    assert!(!timed_file.exists());

    // A preference is yes or no, never for an hour; and a no holds.
    let r = invoke(
        "handheld.ask",
        json!({"setting": "vibration", "value": "off", "reason": "You asked for quiet"}),
    )
    .await;
    assert_eq!(r["ok"], json!(true), "{r}");
    let list: RequestsListResult = client.request(RequestsListParams {}).await.unwrap();
    let answers: Vec<&str> = list.items[0]
        .answers
        .iter()
        .map(|a| a.id.as_str())
        .collect();
    assert_eq!(answers, ["no", "yes"]);
    let hour = client
        .request(RequestsDecideParams {
            id: "setting:vibration".into(),
            answer: "hour".into(),
        })
        .await;
    assert!(hour.is_err(), "not on offer");
    client
        .request(RequestsDecideParams {
            id: "setting:vibration".into(),
            answer: "no".into(),
        })
        .await
        .unwrap();
    let r = invoke(
        "handheld.ask",
        json!({"setting": "vibration", "value": "off", "reason": "Please"}),
    )
    .await;
    assert!(r["error"].as_str().unwrap().contains("said no"), "{r}");
    // The no is kept with the profile, where a restart still finds it.
    let kept: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("data/profiles/test/declined.json")).unwrap(),
    )
    .unwrap();
    assert!(kept["vibration"].as_u64().is_some(), "{kept}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_no_from_before_a_restart_still_holds() {
    let h = start(script_with_reply()).await;
    let root = h.socket.parent().unwrap().parent().unwrap().to_path_buf();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // What an earlier run of the daemon wrote: an hour ago, and two days ago.
    std::fs::write(
        root.join("data/profiles/test/declined.json"),
        json!({"vibration": now - 3600, "reduce_motion": now - 2 * 24 * 3600}).to_string(),
    )
    .unwrap();
    let ask = |setting: &'static str, value: &'static str| {
        let sim = &h._sim;
        async move {
            tokio::time::timeout(
                Duration::from_secs(5),
                sim.invoke(
                    "handheld.ask",
                    json!({"setting": setting, "value": value, "reason": "Please"}),
                ),
            )
            .await
            .expect("invoke completes")
        }
    };
    let r = ask("vibration", "off").await;
    assert!(r["error"].as_str().unwrap_or("").contains("said no"), "{r}");
    let r = ask("reduce_motion", "on").await;
    assert_eq!(r["ok"], json!(true), "a day has passed: {r}");
    // A no dated after now (the clock moved back) is dated now, so it holds a day only.
    let path = root.join("data/profiles/test/declined.json");
    std::fs::write(&path, json!({"lights": now + 365 * 24 * 3600}).to_string()).unwrap();
    let r = ask("lights", "off").await;
    assert!(r["error"].as_str().unwrap_or("").contains("said no"), "{r}");
    let kept: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(kept["lights"].as_u64().unwrap() <= now + 60, "{kept}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_hour_that_cannot_be_recorded_is_not_given() {
    use std::os::unix::fs::PermissionsExt;
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;
    let root = h.socket.parent().unwrap().parent().unwrap().to_path_buf();
    client
        .request(ConsentUpdate {
            shell: Some(false),
            files: Some(FileScope::Home),
            ..Default::default()
        })
        .await
        .unwrap();
    let ask = json!({"setting": "file_reach", "value": "storage", "reason": "To read your saves"});
    let r = tokio::time::timeout(Duration::from_secs(5), h._sim.invoke("handheld.ask", ask))
        .await
        .expect("invoke completes");
    assert_eq!(r["payload"]["status"], json!("waiting_for_owner"), "{r}");

    // The record lives beside the profiles; make it impossible to write.
    let data = root.join("data");
    let permissions = std::fs::metadata(&data).unwrap().permissions();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o500)).unwrap();
    let answer = client
        .request(RequestsDecideParams {
            id: "setting:file_reach".into(),
            answer: "hour".into(),
        })
        .await;
    std::fs::set_permissions(&data, permissions).unwrap();
    assert!(answer.is_err(), "an unrecorded hour is refused");
    let consent: Consent = client.request(ConsentGetParams {}).await.unwrap();
    assert_eq!(consent.files, FileScope::Home, "nothing was widened");
    let list: RequestsListResult = client.request(RequestsListParams {}).await.unwrap();
    assert_eq!(list.items.len(), 1, "the request still waits");

    // Once the record can be written the same answer works, and is recorded.
    client
        .request(RequestsDecideParams {
            id: "setting:file_reach".into(),
            answer: "hour".into(),
        })
        .await
        .unwrap();
    let consent: Consent = client.request(ConsentGetParams {}).await.unwrap();
    assert_eq!(consent.files, FileScope::Storage);
    let timed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("timed-settings.json")).unwrap()).unwrap();
    assert_eq!(timed[0]["revert_to"], json!("own_folder"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_damaged_hour_record_is_kept_aside() {
    let h = start(script_with_reply()).await;
    let root = h.socket.parent().unwrap().parent().unwrap().to_path_buf();
    let path = root.join("data/timed-settings.json");
    std::fs::write(&path, b"[{\"profile\":\"test\",\"sett").unwrap();
    // A record that cannot be read for now is left exactly where it is.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    h.state.revert_due();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(path.exists(), "an unreadable record is not moved");
    assert!(!root.join("data/timed-settings.json.corrupt").exists());
    // A damaged one is kept aside, once, for the owner.
    h.state.revert_due();
    assert!(!path.exists());
    assert_eq!(
        std::fs::read(root.join("data/timed-settings.json.corrupt")).unwrap(),
        b"[{\"profile\":\"test\",\"sett"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_hour_that_cannot_be_applied_leaves_no_record() {
    use std::os::unix::fs::PermissionsExt;
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;
    let root = h.socket.parent().unwrap().parent().unwrap().to_path_buf();
    client
        .request(ConsentUpdate {
            shell: Some(false),
            files: Some(FileScope::Home),
            ..Default::default()
        })
        .await
        .unwrap();
    let ask = json!({"setting": "file_reach", "value": "storage", "reason": "To read your saves"});
    let r = tokio::time::timeout(Duration::from_secs(5), h._sim.invoke("handheld.ask", ask))
        .await
        .expect("invoke completes");
    assert_eq!(r["payload"]["status"], json!("waiting_for_owner"), "{r}");
    // The consent itself cannot be written; the record beside it can.
    let profile_dir = root.join("data/profiles/test");
    let permissions = std::fs::metadata(&profile_dir).unwrap().permissions();
    std::fs::set_permissions(&profile_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let answer = client
        .request(RequestsDecideParams {
            id: "setting:file_reach".into(),
            answer: "hour".into(),
        })
        .await;
    std::fs::set_permissions(&profile_dir, permissions).unwrap();
    assert!(answer.is_err());
    let consent: Consent = client.request(ConsentGetParams {}).await.unwrap();
    assert_eq!(consent.files, FileScope::Home);
    assert!(!root.join("data/timed-settings.json").exists());
    let list: RequestsListResult = client.request(RequestsListParams {}).await.unwrap();
    assert_eq!(list.items.len(), 1, "the request still waits");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_rename_carries_timed_permissions() {
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;
    client
        .request(ProfileCreateParams {
            name: "other".into(),
        })
        .await
        .unwrap();
    let root = h.socket.parent().unwrap().parent().unwrap();
    let path = root.join("data/timed-settings.json");
    let timed = json!([{"profile":"other", "setting":"file_reach", "value":"storage",
        "revert_to":"own_folder", "until":1}]);
    std::fs::write(&path, timed.to_string()).unwrap();
    client
        .request(ProfileRenameParams {
            name: "other".into(),
            new_name: "renamed".into(),
        })
        .await
        .unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved[0]["profile"], "renamed");
    assert_eq!(saved[0]["until"], 1);
    assert_eq!(saved[0]["revert_to"], "own_folder");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_profile_delete_keeps_timed_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;
    client
        .request(ProfileCreateParams {
            name: "other".into(),
        })
        .await
        .unwrap();
    let root = h._tmp.path();
    let profile = root.join("data/profiles/other");
    let consent = Consent {
        files: FileScope::Storage,
        ..Consent::for_new_profile()
    };
    std::fs::write(
        profile.join("consent.json"),
        serde_json::to_vec(&consent).unwrap(),
    )
    .unwrap();
    let path = root.join("data/timed-settings.json");
    let timed = json!([{"profile":"other", "setting":"file_reach", "value":"storage",
        "revert_to":"own_folder", "until":1}]);
    std::fs::write(&path, timed.to_string()).unwrap();
    std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = client
        .request(ProfileDeleteParams {
            name: "other".into(),
        })
        .await;
    std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved, timed);
    let saved: Consent =
        serde_json::from_slice(&std::fs::read(profile.join("consent.json")).unwrap()).unwrap();
    // The inactive profile still has its grant, so keeping the expiry is essential.
    assert_eq!(saved.files, FileScope::Storage);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_files_and_builds_follow_rename_and_are_deleted() {
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;
    client
        .request(ProfileCreateParams {
            name: "other".into(),
        })
        .await
        .unwrap();
    let root = h.socket.parent().unwrap().parent().unwrap();
    std::fs::write(root.join("homes/other/keep.txt"), "mine").unwrap();
    let source = root.join("bundle");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(
        source.join("goblin.json"),
        json!({"name":"test.tool", "kind":"tool",
        "description":"test", "run":["run.sh"]})
        .to_string(),
    )
    .unwrap();
    let builds = goblind::builds::Builds::new(root.join("built/other"));
    let asked = builds.request(&source, 1).unwrap();
    let request = asked.request.unwrap();
    builds
        .approve("test.tool", &request, None, &|_, _| false, 2)
        .unwrap();
    builds.request(&source, 3).unwrap(); // deletion also clears a pending update
    client
        .request(ProfileRenameParams {
            name: "other".into(),
            new_name: "renamed".into(),
        })
        .await
        .unwrap();
    assert!(!root.join("built/other").exists());
    assert!(root
        .join("built/renamed/installed/test.tool/run.sh")
        .exists());
    assert!(root.join("homes/renamed/keep.txt").exists());
    client
        .request(ProfileDeleteParams {
            name: "renamed".into(),
        })
        .await
        .unwrap();
    assert!(!root.join("built/renamed").exists());
    assert!(!root.join("homes/renamed").exists());
    client
        .request(ProfileCreateParams {
            name: "renamed".into(),
        })
        .await
        .unwrap();
    assert!(goblind::builds::Builds::new(root.join("built/renamed"))
        .list()
        .is_empty());
}

/// Reading the card never grants writes to launcher scripts or other owner files.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outside_home_writes_require_the_owner_setting() {
    let h = start(script_with_reply()).await;
    let client = connect(&h.socket).await;
    let root = h._tmp.path();
    let card = root.join("card");
    std::fs::create_dir_all(&card).unwrap();
    let launcher = card.join("launch.sh");
    std::fs::write(&launcher, "original").unwrap();
    let home = root.join("homes/test/note.txt");
    let alias = root.join("homes/test/card");
    std::os::unix::fs::symlink(&card, &alias).unwrap();
    let invoke = |command: &'static str, params: serde_json::Value| {
        let sim = &h._sim;
        async move {
            tokio::time::timeout(Duration::from_secs(5), sim.invoke(command, params))
                .await
                .expect("invoke completes")
        }
    };
    client
        .request(ConsentUpdate {
            shell: Some(false),
            files: Some(FileScope::Home),
            ..Default::default()
        })
        .await
        .unwrap();
    // Even approving Muse's request for an hour grants reading, not outside writes.
    let r = invoke(
        "handheld.ask",
        json!({"setting": "file_reach", "value": "storage", "reason": "To read a save"}),
    )
    .await;
    assert_eq!(r["payload"]["status"], "waiting_for_owner");
    client
        .request(RequestsDecideParams {
            id: "setting:file_reach".into(),
            answer: "hour".into(),
        })
        .await
        .unwrap();
    for path in [&launcher, &alias.join("launch.sh")] {
        let r = invoke(
            "file.write",
            json!({"path": path, "data_b64": "Y2hhbmdlZA==", "final": true}),
        )
        .await;
        assert_eq!(
            r["ok"], false,
            "outside writes need a separate owner grant: {r}"
        );
    }
    assert_eq!(std::fs::read_to_string(&launcher).unwrap(), "original");
    assert_eq!(
        invoke("file.read", json!({"path": launcher})).await["ok"],
        true
    );
    assert_eq!(invoke("file.list", json!({"path": card})).await["ok"], true);
    assert_eq!(
        invoke(
            "file.write",
            json!({"path": home, "data_b64": "bWluZQ==", "final": true})
        )
        .await["ok"],
        true
    );

    let report = invoke("handheld.settings", json!({})).await;
    let setting = report["payload"]["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "write_outside_home")
        .expect("Muse can see the switch");
    assert_eq!(setting["value"], "off");
    assert_eq!(setting["you_may_ask_for"], json!([]));
    for value in ["on", "off"] {
        let r = invoke(
            "handheld.ask",
            json!({"setting": "write_outside_home", "value": value, "reason": "To edit a save"}),
        )
        .await;
        assert!(
            r["error"].as_str().unwrap().contains("owner's alone"),
            "{r}"
        );
    }
    let requests: RequestsListResult = client.request(RequestsListParams {}).await.unwrap();
    assert!(requests.items.is_empty());

    // The owner can enable it through local IPC, with the other limits still enforced.
    let update: ConsentUpdate =
        serde_json::from_value(json!({"file_write_outside_home": true})).unwrap();
    client.request(update).await.unwrap();
    assert_eq!(
        invoke(
            "file.write",
            json!({"path": launcher, "data_b64": "Y2hhbmdlZA==", "final": true})
        )
        .await["ok"],
        true
    );
    for path in [
        root.join("data/secret"),
        root.join("homes/other/note"),
        root.join("off-card"),
    ] {
        assert_eq!(
            invoke(
                "file.write",
                json!({"path": path, "data_b64": "bm8=", "final": true})
            )
            .await["ok"],
            false
        );
    }
    client
        .request(ConsentUpdate {
            file_write: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        invoke(
            "file.write",
            json!({"path": launcher, "data_b64": "bm8=", "final": true})
        )
        .await["ok"],
        false
    );
    let update: ConsentUpdate = serde_json::from_value(
        json!({"file_write": true, "file_write_outside_home": false, "files": "device"}),
    )
    .unwrap();
    client.request(update).await.unwrap();
    assert_eq!(
        invoke(
            "file.write",
            json!({"path": launcher, "data_b64": "bm8=", "final": true})
        )
        .await["ok"],
        false
    );
    assert_eq!(std::fs::read_to_string(&launcher).unwrap(), "changed");
}

#[test]
fn file_commands_need_rules_to_run_at_all() {
    let health: HealthFn = Arc::new(|| json!({}));
    let executor = Executor::new(RunAs::current(), Vec::new(), health);
    let mut params = serde_json::Map::new();
    params.insert("path".into(), json!("/etc/hostname"));
    let r = executor.run_scoped("file.read", &params, None, FileScope::Device, true);
    assert_eq!(r["ok"], json!(false), "{r:?}");
    assert!(executor.in_home("/etc").is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_sdk_token_does_not_reserve_the_pairing_window() {
    let h = start(Script::default()).await;
    let client = connect(&h.socket).await;
    let profile = h.state.profiles.profile("test").unwrap();
    std::fs::write(profile.dir().join("sdk_token"), "invalid").unwrap();
    for _ in 0..2 {
        let error = client
            .request::<_>(PairStartParams {
                profile: None,
                force: false,
                agent: None,
                timeout_s: Some(30),
            })
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("Already pairing"), "{error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_switch_is_rejected_until_the_first_restart() {
    let h = start(Script::default()).await;
    h.state.profiles.create("other").unwrap();
    let client = connect(&h.socket).await;
    let _: Empty = client
        .request(ProfileSwitchParams {
            name: "other".into(),
        })
        .await
        .unwrap();
    // Previously this returned success because test was still the serving profile,
    // although the stored target remained other.
    let error = client
        .request::<_>(ProfileSwitchParams {
            name: "test".into(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("switch in progress"), "{error}");
    assert_eq!(h.state.profiles.active().as_deref(), Some("other"));
}

/// The test runner has no network. Missing tokens fail before serving a page;
/// saved tokens bypass that page and fail the asynchronous connectivity check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_pairing_releases_its_window_and_saved_tokens_skip_the_page() {
    assert!(goblind::tokenpage::local_ip().is_none());
    let h = start(Script::default()).await;
    let client = connect(&h.socket).await;
    let params = || PairStartParams {
        profile: None,
        force: false,
        agent: None,
        timeout_s: Some(30),
    };
    for _ in 0..2 {
        let error = client.request(params()).await.unwrap_err();
        assert!(error.to_string().contains("Connect to Wi-Fi"), "{error}");
    }
    // The kind of agent is one of two, and a profile has one: asking for a Hermes on a
    // profile paired with a Muse is refused before any page is served, and the window is
    // free again afterwards.
    let with_muse = h.state.profiles.create("with-muse").unwrap();
    with_muse
        .save_pairing(
            &goblin_store::profiles::Pairing::from_value(&json!({
                "access_token": "access", "refresh_token": "refresh"
            }))
            .unwrap(),
        )
        .unwrap();
    for (profile, agent, why) in [
        (Some("with-muse"), "hermes", "paired with a Muse"),
        (None, "other", "muse or hermes"),
    ] {
        let error = client
            .request(PairStartParams {
                profile: profile.map(str::to_owned),
                agent: Some(agent.into()),
                force: true,
                ..params()
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains(why), "{agent}: {error}");
    }
    let error = client.request(params()).await.unwrap_err();
    assert!(error.to_string().contains("Connect to Wi-Fi"), "{error}");
    let profile = h.state.profiles.profile("test").unwrap();
    let token =
        goblin_store::profiles::SdkToken::parse(&format!("mgst_{}", "A".repeat(43))).unwrap();
    profile.save_sdk_token(&token).unwrap();
    client.request(params()).await.unwrap();
    // Completion is asynchronous; retry only Busy while the first attempt closes.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match client.request(params()).await {
                Ok(_) => break,
                Err(error) => {
                    assert!(error.to_string().contains("Already pairing"), "{error}");
                    tokio::task::yield_now().await;
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(profile.sdk_token().unwrap().unwrap(), token);
}
