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
    // Start from everything allowed (as a profile from before per-profile access has), so
    // each test turns off what it is about.
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
        storage: None,
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
            json!({"name": "test.echo", "kind": "tool", "description": "Says it back",
                   "run": ["bin/echo.sh"],
                   "required": {"word": {"type": "string", "description": "What to say"}}})
            .to_string(),
        )
        .unwrap();
        dir.to_string_lossy().into_owned()
    };
    let path = bundle("test");

    // Off, as every profile starts.
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

    // The owner approves on the handheld.
    let list: BuildsListResult = client
        .request(BuildsDecideParams {
            name: "test.echo".into(),
            approve: true,
        })
        .await
        .unwrap();
    assert_eq!(list.items[0].state, BuildState::Installed);
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
