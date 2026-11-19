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
use hermes_link::testsupport::{FakeHermes, Script, Tls, CODE, DEVICE, FINGERPRINT, TOKEN};
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
    start_as(script, token, DEVICE).await
}

/// The same, for a profile whose own id is `node_id`. It was paired as [`DEVICE`] either way.
async fn start_as(script: Script, token: &str, node_id: &str) -> Harness {
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
            device_id: DEVICE.into(),
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
        node_id: node_id.into(),
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
    // The runner has no network: the pairing page is served where a test can reach it.
    state.serve_pairing_page_at(std::net::Ipv4Addr::LOCALHOST.into());
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

/// Whether the daemon answered that there is no such thing.
fn not_found(answer: Result<Empty, goblin_ipc::ClientError>) -> bool {
    matches!(answer, Err(goblin_ipc::ClientError::Daemon(e)) if e.code == ErrorCode::NotFound)
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
        ..Script::default()
    }
}

/// Hermes takes a message, starts working on it and says no more.
fn working_script() -> Script {
    Script {
        turn_events: vec![json!({"type": "status", "text": "Thinking", "working": true})],
        ..Script::default()
    }
}

/// Sends a message to `chat`, the `nth` turn of the test, and gives the turn's id on the
/// wire once Hermes has it. Hermes asks its questions within a turn.
async fn turn_under_way(h: &Harness, chat: &str, nth: usize) -> Value {
    let _: ChatSendResult = h
        .client
        .request(ChatSendParams {
            chat: chat.into(),
            text: "do it".into(),
        })
        .await
        .unwrap();
    received(h, "turn", nth).await[nth - 1]["id"].clone()
}

/// Hermes asks before it runs a command.
fn approval(id: &str, chat: &str) -> Value {
    json!({"type": "prompt", "id": id, "chat": chat, "kind": "approval",
           "text": "Run this command?\nrm -rf build",
           "choices": [{"id": "once", "label": "Run once"}, {"id": "deny", "label": "No"}]})
}

/// A card's id without the asking it names: `prompt:<Hermes's id>`.
fn shown(id: &str) -> &str {
    match id.rsplit_once(':') {
        Some((question, _)) if question.contains(':') => question,
        _ => id,
    }
}

/// The id of the card waiting for `question` (`prompt:<Hermes's id>`).
async fn card_id(h: &Harness, question: &str) -> String {
    let list: RequestsListResult = h.client.request(RequestsListParams {}).await.unwrap();
    list.items
        .into_iter()
        .map(|c| c.id)
        .find(|id| shown(id) == question)
        .unwrap_or_else(|| panic!("no card for {question}"))
}

/// The cards waiting, once they are exactly `wanted` (by id less the asking, oldest first).
async fn cards_are(h: &Harness, wanted: &[&str]) -> Vec<RequestView> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let list: RequestsListResult = h.client.request(RequestsListParams {}).await.unwrap();
        let ids: Vec<&str> = list.items.iter().map(|c| shown(&c.id)).collect();
        if ids == wanted {
            return list.items;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "cards are {ids:?}, wanted {wanted:?}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// The chats the inbox has a notice of a waiting question for, oldest first.
async fn asked_in(h: &Harness) -> Vec<String> {
    let list: NotifyListResult = h
        .client
        .request(NotifyListParams { limit: None })
        .await
        .unwrap();
    let mut chats: Vec<String> = list
        .items
        .into_iter()
        .filter(|n| n.kind == NoticeKind::Approval)
        .filter_map(|n| n.chat)
        .collect();
    chats.reverse();
    chats
}

/// A yes to `card` is refused, and Hermes hears nothing of it.
async fn yes_is_refused(h: &Harness, card: &str) {
    let refused = h
        .client
        .request(RequestsDecideParams {
            id: card.into(),
            answer: "yes".into(),
        })
        .await;
    assert!(refused.is_err(), "{card}");
    // Whatever was said would be there by now.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let id = shown(card).trim_start_matches("prompt:");
    let said = h.fake.received.lock().unwrap();
    assert!(
        !said
            .iter()
            .any(|f| f["type"] == "prompt.answer" && f["id"] == id),
        "{card}: {said:?}"
    );
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
    let h = start(working_script(), TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    turn_under_way(&h, "main", 1).await;
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
    assert_eq!(shown(&card.id), "prompt:p1");
    assert_eq!(
        (card.heading.as_str(), card.body.as_str()),
        ("Run this command?", "rm -rf build")
    );
    assert_eq!((card.kind, card.cut), (RequestKind::Approval, false));
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
            id: card.id.clone(),
            answer: "maybe".into(),
        })
        .await;
    assert!(wrong.is_err());
    let after: RequestsListResult = h
        .client
        .request(RequestsDecideParams {
            id: card.id.clone(),
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
    // The notice that announced the question went with it.
    assert!(asked_in(&h).await.is_empty());
    // Answered once: a second press finds nothing.
    let again = h
        .client
        .request(RequestsDecideParams {
            id: card.id.clone(),
            answer: "yes".into(),
        })
        .await;
    assert!(again.is_err());
    assert_eq!(received(&h, "prompt.answer", 1).await.len(), 1);
}

/// The cards waiting, once there are `wanted` of them.
async fn cards(h: &Harness, wanted: usize) -> Vec<RequestView> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let list: RequestsListResult = h.client.request(RequestsListParams {}).await.unwrap();
        if list.items.len() >= wanted {
            return list.items;
        }
        assert!(tokio::time::Instant::now() < deadline, "no card");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// What the handheld did not keep whole, nobody approves through it: an approval longer
/// than it keeps, or with characters it took out, has "no" for its only answer, and a yes
/// is refused by the daemon whichever client sends it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approval_the_handheld_cut_can_only_be_refused() {
    let h = start(working_script(), TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    turn_under_way(&h, "main", 1).await;
    let choices = json!([{"id": "once", "label": "Run once"}, {"id": "session", "label": "Session"},
                         {"id": "deny", "label": "No"}]);
    let long = format!("Run this command?\n{}", "echo padding; ".repeat(150));
    assert!(long.chars().count() > 2000);
    let prompts = [
        // Longer than the handheld keeps: the end of the command is not on the card.
        ("p1", "approval", long.as_str()),
        // A character that reorders what is read: taken out, so not what will run.
        ("p2", "approval", "Run this command?\necho \u{202e}dne"),
        // A question gives leave for nothing; it says it was cut and stays answerable.
        ("p3", "clarify", long.as_str()),
        // Kept whole: every answer.
        ("p4", "approval", "Run this command?\nls"),
    ];
    for (id, kind, text) in prompts {
        h.fake
            .push
            .send(
                json!({"type": "prompt", "id": id, "chat": "main", "kind": kind,
                         "text": text, "choices": choices}),
            )
            .unwrap();
        // One at a time, so they are listed in this order.
        let n = prompts.iter().position(|p| p.0 == id).unwrap() + 1;
        cards(&h, n).await;
    }
    let listed = cards(&h, 4).await;
    let seen: Vec<(&str, RequestKind, bool, Vec<&str>)> = listed
        .iter()
        .map(|c| {
            let answers = c.answers.iter().map(|a| a.id.as_str()).collect();
            (shown(&c.id), c.kind, c.cut, answers)
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("prompt:p1", RequestKind::Approval, true, vec!["no"]),
            ("prompt:p2", RequestKind::Approval, true, vec!["no"]),
            (
                "prompt:p3",
                RequestKind::Question,
                true,
                vec!["yes", "no", "hour"]
            ),
            (
                "prompt:p4",
                RequestKind::Approval,
                false,
                vec!["yes", "hour", "no"]
            ),
        ]
    );
    assert_eq!(
        listed[0].body.chars().count() + "Run this command?\n".len(),
        2000
    );
    // A yes to either cut approval answers nothing, and the card stays.
    for id in ["prompt:p1", "prompt:p2"] {
        for answer in ["yes", "hour"] {
            let refused = h
                .client
                .request(RequestsDecideParams {
                    id: card_id(&h, id).await,
                    answer: answer.into(),
                })
                .await;
            assert!(refused.is_err(), "{id} {answer}");
        }
    }
    assert_eq!(cards(&h, 4).await.len(), 4);
    // The no is given, and is all Hermes hears of them.
    for id in ["prompt:p1", "prompt:p2"] {
        let _: RequestsListResult = h
            .client
            .request(RequestsDecideParams {
                id: card_id(&h, id).await,
                answer: "no".into(),
            })
            .await
            .unwrap();
    }
    let answers = received(&h, "prompt.answer", 2).await;
    let said: Vec<(&str, &str)> = answers
        .iter()
        .map(|a| (a["id"].as_str().unwrap(), a["answer"].as_str().unwrap()))
        .collect();
    assert_eq!(said, [("p1", "deny"), ("p2", "deny")]);
}

/// A command can hold characters that are in it and not on any screen: a joiner, a tag, a
/// space that is not the plain one, a selector after a letter. It then does not read as it
/// will run. The daemon takes them out, which makes the approval one it cut: "no" is its
/// only answer, whichever client asks. What is ordinary stays whole, indentation and all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approval_with_a_character_no_screen_shows_can_only_be_refused() {
    let h = start(working_script(), TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    turn_under_way(&h, "main", 1).await;
    let choices = json!([{"id": "once", "label": "Run once"}, {"id": "session", "label": "Session"},
                         {"id": "deny", "label": "No"}]);
    let prompts = [
        // Reads as a test that is always true; with the joiner it is false, and the rm runs.
        (
            "p1",
            "Run this command?\n[ a = a\u{34f} ] || rm -rf ~/Documents",
        ),
        ("p2", "Run this command?\necho ok\u{e0041}\u{e0020}"),
        ("p3", "Run this command?\nrm -rf ~/old\u{a0}files"),
        ("p4", "Run this command?\nls a\u{fe0f} \u{2009}-la"),
        ("p5", "Run this\u{2060} command?\nls"),
        // The two ticks are drawn alike and only one has a selector after it: the test is
        // false, and the rm runs.
        (
            "p6",
            "Run this command?\n[ \u{2713} = \u{2713}\u{fe0f} ] || rm -rf ~/Documents",
        ),
        // A selector on the first line, where a command may be as well as anywhere.
        ("p7", "\u{26a0} sign\u{fe0f} Run this command?\nls"),
        // Hermes's own opening (its selector is not counted), an accent, a dash, and a
        // script with indentation.
        (
            "p8",
            "\u{26a0}\u{fe0f} Run this command?\n\n    if caf\u{e9}:\n        go() \u{2014} now\n",
        ),
    ];
    for (n, (id, text)) in prompts.iter().enumerate() {
        h.fake
            .push
            .send(
                json!({"type": "prompt", "id": id, "chat": "main", "kind": "approval",
                         "text": text, "choices": choices}),
            )
            .unwrap();
        cards(&h, n + 1).await;
    }
    let listed = cards(&h, prompts.len()).await;
    let seen: Vec<(&str, &str, bool, Vec<&str>)> = listed
        .iter()
        .map(|c| {
            let answers = c.answers.iter().map(|a| a.id.as_str()).collect();
            (c.heading.as_str(), c.body.as_str(), c.cut, answers)
        })
        .collect();
    let (asked, no, all) = ("Run this command?", vec!["no"], vec!["yes", "hour", "no"]);
    assert_eq!(
        seen,
        [
            (asked, "[ a = a ] || rm -rf ~/Documents", true, no.clone()),
            (asked, "echo ok", true, no.clone()),
            // A space that is not U+0020 is shown as one, and the card is still cut.
            (asked, "rm -rf ~/old files", true, no.clone()),
            (asked, "ls a  -la", true, no.clone()),
            (asked, "ls", true, no.clone()),
            (
                asked,
                "[ \u{2713} = \u{2713} ] || rm -rf ~/Documents",
                true,
                no.clone()
            ),
            ("\u{26a0} sign Run this command?", "ls", true, no.clone()),
            (
                "\u{26a0} Run this command?",
                "    if caf\u{e9}:\n        go() \u{2014} now",
                false,
                all
            ),
        ]
    );
    for card in &listed[..7] {
        for answer in ["yes", "hour"] {
            let refused = h
                .client
                .request(RequestsDecideParams {
                    id: card.id.clone(),
                    answer: answer.into(),
                })
                .await;
            assert!(refused.is_err(), "{} {answer}", card.id);
        }
    }
    assert_eq!(cards(&h, prompts.len()).await.len(), prompts.len());
    // Nothing was passed on for them; the whole one is answered as usual.
    let _: RequestsListResult = h
        .client
        .request(RequestsDecideParams {
            id: card_id(&h, "prompt:p8").await,
            answer: "yes".into(),
        })
        .await
        .unwrap();
    let answers = received(&h, "prompt.answer", 1).await;
    assert_eq!(answers.len(), 1);
    assert_eq!(
        (answers[0]["id"].as_str(), answers[0]["answer"].as_str()),
        (Some("p8"), Some("once"))
    );
}

/// A card is there only while its question can be answered. It goes with the turn it was
/// asked in, however that ends: Hermes finishes, Hermes fails, or the owner stops it. The
/// notice that announced it goes too, a yes to it is refused and not passed on, and a
/// question in another chat stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_card_goes_when_its_turn_ends() {
    for ending in ["done", "failed", "stopped"] {
        let h = start(working_script(), TOKEN).await;
        link_becomes(&h, LinkState::Online).await;
        let main = turn_under_way(&h, "main", 1).await;
        turn_under_way(&h, "side", 2).await;
        h.fake.push.send(approval("p1", "main")).unwrap();
        cards_are(&h, &["prompt:p1"]).await;
        h.fake.push.send(approval("p2", "side")).unwrap();
        let asked = cards_are(&h, &["prompt:p1", "prompt:p2"]).await;
        assert_eq!(asked_in(&h).await, ["main", "side"], "{ending}");
        match ending {
            "done" => h
                .fake
                .push
                .send(json!({"type": "turn.done", "turn": main}))
                .unwrap(),
            "failed" => h
                .fake
                .push
                .send(json!({"type": "turn.failed", "turn": main, "reason": "it broke"}))
                .unwrap(),
            _ => {
                let _: Empty = h
                    .client
                    .request(ChatStopParams {
                        chat: "main".into(),
                    })
                    .await
                    .unwrap();
            }
        }
        cards_are(&h, &["prompt:p2"]).await;
        assert_eq!(asked_in(&h).await, ["side"], "{ending}");
        yes_is_refused(&h, &asked[0].id).await;
        // The other chat's question is still answered.
        let _: RequestsListResult = h
            .client
            .request(RequestsDecideParams {
                id: asked[1].id.clone(),
                answer: "yes".into(),
            })
            .await
            .unwrap();
        let answer = received(&h, "prompt.answer", 1).await.remove(0);
        assert_eq!(
            (answer["id"].as_str(), answer["answer"].as_str()),
            (Some("p2"), Some("once")),
            "{ending}"
        );
    }
}

/// Hermes can take a question back: it was answered somewhere else, or Hermes stopped
/// waiting. The card and its notice go at once, though the turn goes on, and a yes to it
/// is refused and not passed on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_card_goes_when_hermes_closes_the_question() {
    let h = start(working_script(), TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    turn_under_way(&h, "main", 1).await;
    h.fake.push.send(approval("p1", "main")).unwrap();
    cards_are(&h, &["prompt:p1"]).await;
    let mut question = approval("p2", "main");
    question["kind"] = json!("clarify");
    h.fake.push.send(question).unwrap();
    let asked = cards_are(&h, &["prompt:p1", "prompt:p2"]).await;
    assert_eq!(asked_in(&h).await, ["main", "main"]);
    // One it never asked, and one that is not an id, close nothing.
    for closed in [json!("p9"), json!("../p2"), json!(7), json!("p1")] {
        h.fake
            .push
            .send(json!({"type": "prompt.closed", "id": closed}))
            .unwrap();
    }
    cards_are(&h, &["prompt:p2"]).await;
    assert_eq!(asked_in(&h).await, ["main"]);
    yes_is_refused(&h, &asked[0].id).await;
    h.fake
        .push
        .send(json!({"type": "prompt.closed", "id": "p2"}))
        .unwrap();
    cards_are(&h, &[]).await;
    assert!(asked_in(&h).await.is_empty());
    yes_is_refused(&h, &asked[1].id).await;
    // The turn itself is still under way, and can still be asked in.
    h.fake.push.send(approval("p3", "main")).unwrap();
    cards_are(&h, &["prompt:p3"]).await;
}

/// A question asked again under the same id is a new card. A press on the earlier card,
/// which showed something else, answers neither, and the newer question keeps waiting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_to_a_replaced_question_from_hermes_answers_nothing() {
    let h = start(working_script(), TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    turn_under_way(&h, "main", 1).await;
    h.fake.push.send(approval("p1", "main")).unwrap();
    let seen = cards_are(&h, &["prompt:p1"]).await.remove(0);
    let mut again = approval("p1", "main");
    again["text"] = json!("Run this command?\nrm -rf ~");
    h.fake.push.send(again).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let newer = loop {
        let card = cards_are(&h, &["prompt:p1"]).await.remove(0);
        if card.body == "rm -rf ~" {
            break card;
        }
        assert!(tokio::time::Instant::now() < deadline, "not replaced");
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    yes_is_refused(&h, &seen.id).await;
    assert_ne!(seen.id, newer.id, "each asking has its own card");
    // Nor does a no to the earlier card answer the newer question.
    let refused = h
        .client
        .request(RequestsDecideParams {
            id: seen.id.clone(),
            answer: "no".into(),
        })
        .await;
    assert!(refused.is_err());
    // An id that names no asking answers none.
    yes_is_refused(&h, "prompt:p1").await;
    assert_eq!(cards_are(&h, &["prompt:p1"]).await[0].id, newer.id);
    // The newer card is answered as itself.
    let _: RequestsListResult = h
        .client
        .request(RequestsDecideParams {
            id: newer.id.clone(),
            answer: "yes".into(),
        })
        .await
        .unwrap();
    let answer = received(&h, "prompt.answer", 1).await.remove(0);
    assert_eq!(
        (answer["id"].as_str(), answer["answer"].as_str()),
        (Some("p1"), Some("once"))
    );
}

/// A question belongs to a turn. One for a chat with no turn under way (none was sent
/// there, or it is over) gets no card and no notice; an approval among them is told no,
/// so Hermes does not wait for an answer nobody can give.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_question_with_no_turn_under_way_gets_no_card() {
    let h = start(working_script(), TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    let main = turn_under_way(&h, "main", 1).await;
    // Another chat's turn is not this chat's.
    h.fake.push.send(approval("p1", "side")).unwrap();
    let denied = received(&h, "prompt.answer", 1).await.remove(0);
    assert_eq!(
        (denied["id"].as_str(), denied["answer"].as_str()),
        (Some("p1"), Some("deny"))
    );
    // The turn ends, and a question for it comes after.
    h.fake
        .push
        .send(json!({"type": "turn.done", "turn": main}))
        .unwrap();
    let mut late = approval("p2", "main");
    late["kind"] = json!("clarify");
    h.fake.push.send(late).unwrap();
    h.fake.push.send(approval("p3", "main")).unwrap();
    let denied = received(&h, "prompt.answer", 2).await.remove(1);
    assert_eq!(
        (denied["id"].as_str(), denied["answer"].as_str()),
        (Some("p3"), Some("deny"))
    );
    cards_are(&h, &[]).await;
    assert!(asked_in(&h).await.is_empty());
    // A question that is not an approval has no "no" to be told: nothing is said for it.
    assert_eq!(received(&h, "prompt.answer", 2).await.len(), 2);
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

/// A second pairing code for the fake, and the token it gives.
const SECOND_CODE: &str = "PAIRTWO2PAIRTWO2PAIRTWO2";
const SECOND_TOKEN: &str = "second-token-0123456789abcdef0123456789";

/// The next `pair.state` the daemon publishes.
async fn pair_state(h: &Harness) -> Value {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(30), h.client.next_event())
            .await
            .expect("the daemon says how the pairing goes")
            .unwrap();
        if ev.event == "pair.state" {
            return ev.data;
        }
    }
}

/// What the owner does to pair `profile` with the fake, up to where the handheld takes
/// over: `pair.start` for a Hermes, then the link with `code` pasted on the page it serves.
async fn paste_a_link(h: &Harness, profile: &str, code: &str) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    h.client
        .subscribe(SubscribeParams {
            after_seq: None,
            topics: Some(vec![Topic::Daemon]),
        })
        .await
        .unwrap();
    let _: PairStartResult = h
        .client
        .request(PairStartParams {
            profile: Some(profile.into()),
            agent: Some("hermes".into()),
            ..PairStartParams::default()
        })
        .await
        .unwrap();
    let opened = pair_state(h).await;
    assert_eq!(opened["state"], "need_token", "{opened}");
    let page = opened["detail"].as_str().unwrap();
    let link = format!(
        "goblinlink://127.0.0.1:{}/?fp={FINGERPRINT}&code={code}",
        h.fake.port
    );
    // Every byte escaped: what a form may send, and nothing to get wrong here.
    let field: String = link.bytes().map(|b| format!("%{b:02X}")).collect();
    let body = format!("token={field}");
    let addr = page.trim_start_matches("http://").trim_end_matches('/');
    let mut phone = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    phone.write_all(request.as_bytes()).await.unwrap();
    let mut answer = String::new();
    phone.read_to_string(&mut answer).await.unwrap();
    assert!(answer.contains("Link received"), "{answer}");
}

/// How `pair.state` says a pairing ended.
async fn pairing_ends(h: &Harness) -> Value {
    loop {
        let said = pair_state(h).await;
        let state = said["state"].as_str().unwrap_or("");
        if matches!(state, "done" | "failed" | "cancelled" | "timeout") {
            return said;
        }
    }
}

/// Pairs `profile` with the fake as the owner does, and returns how it ended.
async fn pair_from_the_page(h: &Harness, profile: &str, code: &str) -> Value {
    paste_a_link(h, profile, code).await;
    pairing_ends(h).await
}

/// Says hello to the fake with a pairing a profile has saved, as the daemon does when it
/// serves that profile.
async fn says_hello(h: &Harness, saved: &HermesPairing) -> Result<(), hermes_link::LinkError> {
    let pairing = hermes_link::Pairing {
        hosts: saved.hosts.clone(),
        port: saved.port,
        fingerprint: saved.fingerprint.clone(),
        device_id: saved.device_id.clone(),
        token: saved.token.expose().to_owned(),
    };
    let device = hermes_link::Device {
        id: saved.device_id.clone(),
        name: "test".into(),
        version: "0".into(),
        commands: json!({}),
    };
    let (pushed, _pushes) = tokio::sync::mpsc::channel(1);
    let runner = Arc::new(goblind::daemon::DaemonRunner::new(h.state.clone()));
    let session = hermes_link::Session::connect(&pairing, &device, runner, pushed).await?;
    session.stop();
    Ok(())
}

/// One Hermes for two profiles of a handheld. The second is paired while the daemon serves
/// the first, and Hermes keeps one token to an id: under the first one's id, the second
/// would take its place there, and the first would be unpaired at its next hello.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_profile_is_paired_under_its_own_id_and_the_first_stays_paired() {
    let h = start(Script::default(), TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    let second = h.state.profiles.create("second").unwrap();
    let own = second.identity().unwrap().node_id();
    h.fake
        .codes
        .lock()
        .unwrap()
        .insert(SECOND_CODE.into(), SECOND_TOKEN.into());

    let ended = pair_from_the_page(&h, "second", SECOND_CODE).await;
    assert_eq!(
        (ended["state"].as_str(), ended["profile"].as_str()),
        (Some("done"), Some("second")),
        "{ended}"
    );
    // It was the second profile's id that went to Hermes, with the handheld's name and
    // version.
    let pair = received(&h, "pair", 1).await.remove(0);
    assert_eq!(
        pair["device"],
        json!({"id": own, "name": "test", "version": "0"})
    );
    // Hermes knows both, each by its own id and token.
    let known = h.fake.devices.lock().unwrap().clone();
    let expected = [(DEVICE, TOKEN), (own.as_str(), SECOND_TOKEN)];
    assert_eq!(
        known,
        expected
            .map(|(id, token)| (id.to_owned(), token.to_owned()))
            .into()
    );
    // The second kept its own id with its token, and Hermes takes a hello with them.
    let saved = second.hermes_pairing().unwrap().unwrap();
    assert_eq!(
        (saved.device_id.as_str(), saved.token.expose()),
        (own.as_str(), SECOND_TOKEN)
    );
    says_hello(&h, &saved).await.unwrap();

    // The first one's connection drops, as when Hermes starts again, and the daemon says
    // hello once more with what that profile has saved. It is taken: not answered
    // "unpaired", which would have cost the profile its pairing.
    h.fake.hang_up();
    link_becomes(&h, LinkState::Offline).await;
    let hello = received(&h, "hello", 3).await.remove(2);
    assert_eq!(hello["device"], DEVICE);
    let status = link_becomes(&h, LinkState::Online).await;
    assert!(status.paired);
    let first = h.state.profiles.profile("test").unwrap();
    let saved = first.hermes_pairing().unwrap().unwrap();
    assert_eq!(
        (saved.device_id.as_str(), saved.token.expose()),
        (DEVICE, TOKEN)
    );
}

/// The owner stops a pairing while the handheld is connecting to Hermes: it ends as
/// cancelled, whatever Hermes has done by then, and the handheld keeps nothing of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hermes_pairing_stopped_while_it_connects_is_cancelled_and_keeps_nothing() {
    // Hermes takes a pairing at once, and its answer is held back until the test lets it go.
    let held = Arc::new(tokio::sync::Semaphore::new(0));
    let script = Script {
        pair_held: Some(held.clone()),
        ..Script::default()
    };
    // The profile served is not paired: Hermes has forgotten the token it holds.
    let h = start(script, "a-token-hermes-has-forgotten-0000000000").await;
    link_becomes(&h, LinkState::Unpaired).await;
    let profile = h.state.profiles.profile("test").unwrap();
    let own = profile.identity().unwrap().node_id();
    let filed = |h: &Harness| h.fake.devices.lock().unwrap().get(&own).cloned();

    paste_a_link(&h, "test", CODE).await;
    // The page is closed. The apps are told the handheld is connecting, to show in place
    // of the page's code.
    let said = pair_state(&h).await;
    assert_eq!(said["state"], "connecting", "{said}");
    // Hermes has made out a token for it when the owner presses stop.
    while filed(&h).is_none() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _: Empty = h.client.request(PairCancelParams {}).await.unwrap();
    let ended = pair_state(&h).await;
    assert_eq!(
        (ended["state"].as_str(), ended["profile"].as_str()),
        (Some("cancelled"), Some("test")),
        "{ended}"
    );
    // Hermes's answer comes after that, to nobody. Wait longer than the daemon does before
    // it starts again after a pairing: it does not, and the profile has no pairing.
    held.add_permits(1);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(profile.hermes_pairing().unwrap().is_none());
    assert!(!h.state.restart_requested());
    let status: Status = h.client.request(DaemonStatusParams {}).await.unwrap();
    assert!(!status.paired);
    // What is left is on the Hermes: the entry it filed, with a token nobody holds, until
    // the owner unpairs it there or pairs the profile again.
    assert_eq!(filed(&h).as_deref(), Some(TOKEN));

    // The window is free again. A new link pairs the profile, in place of that entry, and
    // the daemon starts again to serve it as paired.
    h.fake
        .codes
        .lock()
        .unwrap()
        .insert(SECOND_CODE.into(), SECOND_TOKEN.into());
    held.add_permits(1);
    let ended = pair_from_the_page(&h, "test", SECOND_CODE).await;
    assert_eq!(ended["state"], "done", "{ended}");
    let saved = profile.hermes_pairing().unwrap().unwrap();
    assert_eq!(
        (saved.device_id.as_str(), saved.token.expose()),
        (own.as_str(), SECOND_TOKEN)
    );
    assert_eq!(filed(&h).as_deref(), Some(SECOND_TOKEN));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !h.state.restart_requested() {
        assert!(tokio::time::Instant::now() < deadline, "no restart");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// A profile says hello under the id it was paired under, which is the one saved with its
/// token, even where that is not its own (it was paired before each profile had its own).
/// Hermes knows the token by that id: any other would be answered "unpaired".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_profile_says_hello_under_the_id_it_was_paired_under() {
    let h = start_as(Script::default(), TOKEN, "homelink-0a0b0c").await;
    let status = link_becomes(&h, LinkState::Online).await;
    assert!(status.paired);
    let hello = received(&h, "hello", 1).await.remove(0);
    assert_eq!(hello["device"], DEVICE);
}

/// A square picture of one colour, as PNG in base64.
fn picture(shade: u8) -> String {
    use base64::Engine;
    let image = image::RgbaImage::from_pixel(96, 96, image::Rgba([shade, 40, 90, 255]));
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    base64::engine::general_purpose::STANDARD.encode(png)
}

/// The agent as `daemon.status` gives it, once `ready` holds.
async fn agent_becomes(h: &Harness, what: &str, ready: impl Fn(&AgentInfo) -> bool) -> AgentInfo {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let status: Status = h.client.request(DaemonStatusParams {}).await.unwrap();
        if ready(&status.agent) {
            return status.agent;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: agent is {:?}",
            status.agent
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A Hermes that chose a name and a picture for itself is shown with them: from the hello,
/// and again when it changes them while connected. A picture that cannot be had or cannot be
/// read changes nothing: the one before stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hermes_is_shown_with_the_name_and_picture_it_chose() {
    use base64::Engine;
    let broken = [b"\x89PNG\r\n\x1a\n".as_slice(), b"not a picture at all"].concat();
    let script = Script {
        identity: json!({"agent": "Pip", "avatar": {"revision": "r_1"}}),
        avatars: [
            ("r_1".to_owned(), picture(10)),
            ("r_2".to_owned(), picture(200)),
            (
                "r_broken".to_owned(),
                base64::engine::general_purpose::STANDARD.encode(broken),
            ),
        ]
        .into(),
        ..Script::default()
    };
    let h = start(script, TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    let ends_with = |agent: &AgentInfo, revision: &str| {
        agent
            .avatar_path
            .as_deref()
            .is_some_and(|p| p.ends_with(revision))
    };
    let first = agent_becomes(&h, "the hello's picture", |a| ends_with(a, "r_1")).await;
    assert_eq!(first.name, "Pip");
    let first_dir = std::path::PathBuf::from(first.avatar_path.unwrap());
    // Kept as the handheld shows it: a square PNG, marked ready, in the profile's own folder.
    assert!(first_dir.starts_with(h.root.join("data/profiles/test/avatar")));
    let kept = image::open(first_dir.join("avatar.png")).unwrap();
    assert_eq!(kept.width(), kept.height());
    assert!(first_dir.join("meta.json").exists());

    // It changes both while connected: no pairing again, no restart.
    let say = |agent: &str, revision: &str| {
        h.fake
            .push
            .send(json!({"type": "identity", "agent": agent, "avatar": {"revision": revision}}))
            .unwrap();
    };
    say("Pop", "r_2");
    let second = agent_becomes(&h, "the new picture", |a| ends_with(a, "r_2")).await;
    assert_eq!(second.name, "Pop");
    assert!(!first_dir.exists(), "the old picture is not kept");

    // A picture Hermes no longer has, and one that is no picture: the name follows, the
    // picture shown stays the last good one.
    for (name, revision) in [("Gone", "r_gone"), ("Broken", "r_broken")] {
        say(name, revision);
        let after = agent_becomes(&h, name, |a| a.name == name).await;
        let asked = received(&h, "avatar.get", 1).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !received(&h, "avatar.get", asked.len())
            .await
            .iter()
            .any(|f| f["revision"] == revision)
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{revision} never asked for"
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let status: Status = h.client.request(DaemonStatusParams {}).await.unwrap();
        assert!(
            ends_with(&status.agent, "r_2"),
            "{name}: {:?}",
            status.agent
        );
        assert!(ends_with(&after, "r_2"));
    }
    // Neither left a folder behind: only the picture shown is kept.
    let avatars = h.root.join("data/profiles/test/avatar");
    let kept: Vec<_> = std::fs::read_dir(&avatars)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(kept, ["r_2"]);
    // It gives its picture up: the stock one is shown again, and the files are gone.
    h.fake
        .push
        .send(json!({"type": "identity", "agent": "Plain"}))
        .unwrap();
    let plain = agent_becomes(&h, "no picture", |a| {
        a.name == "Plain" && a.avatar_path.is_none()
    })
    .await;
    assert_eq!(plain.avatar_path, None);
    assert_eq!(std::fs::read_dir(&avatars).unwrap().count(), 0);
    // And takes one again.
    say("Pip", "r_1");
    agent_becomes(&h, "a picture again", |a| ends_with(a, "r_1")).await;
}

/// A reply under way is stopped from the handheld: Hermes is told, the turn ends as stopped,
/// and what Hermes still says of it afterwards is not kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_under_way_is_stopped_and_nothing_of_it_is_kept_after() {
    // Hermes starts working and says no more.
    let h = start(
        Script {
            turn_events: vec![json!({"type": "status", "text": "Thinking", "working": true})],
            reject: None,
            silent: false,
            ..Script::default()
        },
        TOKEN,
    )
    .await;
    link_becomes(&h, LinkState::Online).await;
    // Nothing is under way yet.
    let none = h
        .client
        .request(ChatStopParams {
            chat: "main".into(),
        })
        .await;
    assert!(not_found(none), "none");
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
            text: "a long job".into(),
        })
        .await
        .unwrap();
    let turn_state = |wanted: &'static str| {
        let (client, turn) = (&h.client, sent.turn.clone());
        async move {
            loop {
                let ev = tokio::time::timeout(Duration::from_secs(8), client.next_event())
                    .await
                    .unwrap_or_else(|_| panic!("the turn never became {wanted}"))
                    .unwrap();
                if ev.event == "chat.turn" && ev.data["turn"] == json!(turn) {
                    let state = ev.data["state"].as_str().unwrap_or("").to_owned();
                    if state == wanted {
                        return;
                    }
                    assert_ne!(state, "done");
                }
            }
        }
    };
    turn_state("working").await;
    // Another chat's reply is not this one.
    let other = h
        .client
        .request(ChatStopParams {
            chat: "side".into(),
        })
        .await;
    assert!(not_found(other), "other");

    let _: Empty = h
        .client
        .request(ChatStopParams {
            chat: "main".into(),
        })
        .await
        .unwrap();
    turn_state("stopped").await;
    let wire_turn = received(&h, "turn", 1).await.remove(0)["id"].clone();
    let cancel = received(&h, "turn.cancel", 1).await;
    assert_eq!(
        (cancel.len(), &cancel[0]["id"]),
        (1, &wire_turn),
        "told once"
    );

    // Hermes had more to say; it comes too late.
    for late in [
        json!({"type": "message", "turn": wire_turn, "message_id": "a1", "text": "Too late", "done": true}),
        json!({"type": "turn.done", "turn": wire_turn}),
    ] {
        h.fake.push.send(late).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
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
    assert_eq!(texts, ["a long job"]);
    // It is over: there is nothing left to stop.
    let again = h
        .client
        .request(ChatStopParams {
            chat: "main".into(),
        })
        .await;
    assert!(not_found(again), "again");
}

/// A reply cut off while it streams is not written at each delta, but is on disk once the
/// daemon stops following it.
#[tokio::test]
async fn a_reply_cut_off_mid_stream_reaches_disk_when_it_is_let_go() {
    let h = start(
        Script {
            turn_events: vec![
                json!({"type": "message", "message_id": "a1", "text": "Half a", "done": false}),
            ],
            ..Script::default()
        },
        TOKEN,
    )
    .await;
    link_becomes(&h, LinkState::Online).await;
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
            text: "go on".into(),
        })
        .await
        .unwrap();
    let file = h.root.join("data/profiles/test/chats/main.json");
    let on_disk = || std::fs::read_to_string(&file).unwrap_or_default();
    let mut streamed = false;
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(8), h.client.next_event())
            .await
            .expect("the turn never ended")
            .unwrap();
        if ev.event == "chat.message" && ev.data["message"]["text"] == "Half a" {
            assert!(on_disk().contains("go on"), "the sent message is written");
            assert!(!on_disk().contains("Half a"), "a delta is not");
            streamed = true;
            let _: Empty = h
                .client
                .request(ChatStopParams {
                    chat: "main".into(),
                })
                .await
                .unwrap();
        }
        if ev.event == "chat.turn"
            && ev.data["turn"] == json!(sent.turn)
            && ev.data["state"] == "stopped"
        {
            break;
        }
    }
    assert!(streamed);
    assert!(on_disk().contains("Half a"), "what was left of it is");
}

/// The `chat.turn` event that ends `turn`, on a client subscribed to the chat topic.
async fn turn_end(h: &Harness, turn: &str) -> Value {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(8), h.client.next_event())
            .await
            .expect("the turn never ended")
            .unwrap();
        let state = ev.data["state"].as_str().unwrap_or("");
        let over = ["done", "failed", "uncertain", "stopped"].contains(&state);
        if ev.event == "chat.turn" && ev.data["turn"] == turn && over {
            return ev.data;
        }
    }
}

/// A turn Hermes says failed ends as failed, not as a send that may not have arrived, and
/// says why in a few words on one line, without what no screen shows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_hermes_says_failed_ends_as_failed_and_says_why() {
    let why = format!(
        "the voice note\ncould not\u{202e} be understood {}",
        "and so on ".repeat(20)
    );
    let script = Script {
        turn_events: vec![json!({"type": "turn.failed", "reason": why})],
        ..Script::default()
    };
    let h = start(script, TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
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
    let end = turn_end(&h, &sent.turn).await;
    assert_eq!(end["state"], "failed", "{end}");
    let reason = end["reason"].as_str().unwrap_or("");
    assert!(
        reason.starts_with("the voice note could not be understood and so on"),
        "{reason:?}"
    );
    assert!(
        reason.ends_with('\u{2026}') && reason.chars().count() <= 81,
        "{reason:?}"
    );
}

/// Hermes asks the owner, mid-turn, to change a setting, and the owner says no on the card.
async fn asked_and_told_no(h: &Harness) {
    h.fake
        .push
        .send(
            json!({"type": "tool.call", "id": "c1", "command": "handheld.ask",
                     "params": {"setting": "reduce_motion", "value": "on", "reason": "Calmer."}}),
        )
        .unwrap();
    received(h, "tool.result", 1).await;
    let card = cards(h, 1).await.remove(0);
    let _: RequestsListResult = h
        .client
        .request(RequestsDecideParams {
            id: card.id,
            answer: "no".into(),
        })
        .await
        .unwrap();
}

/// The owner's answer to a card reaches a Hermes still at work on the turn that asked: it
/// is held while the turn goes on (Hermes would refuse a new message in that chat) and sent
/// as the owner's message once the turn ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_given_while_hermes_works_is_sent_when_the_turn_ends() {
    let h = start(working_script(), TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    let asking = turn_under_way(&h, "main", 1).await;
    asked_and_told_no(&h).await;
    // Whatever was said would be there by now.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(received(&h, "turn", 1).await.len(), 1, "held");
    h.fake
        .push
        .send(json!({"type": "turn.done", "turn": asking}))
        .unwrap();
    let told = received(&h, "turn", 2).await.remove(1);
    assert_eq!(
        (told["chat"].as_str(), told["text"].as_str()),
        (Some("main"), Some("No to \u{201c}Reduce motion\u{201d}."))
    );
}

/// An answer held for a Hermes that goes away before the turn ends cannot reach it: it
/// stays in the chat as the owner's message, marked as not delivered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_that_cannot_reach_hermes_is_shown_as_not_delivered() {
    let h = start(working_script(), TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    turn_under_way(&h, "main", 1).await;
    asked_and_told_no(&h).await;
    // Held: whatever was said would be there by now.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(received(&h, "turn", 1).await.len(), 1, "held");
    h.fake.hang_up();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let history: ChatHistoryResult = h
            .client
            .request(ChatHistoryParams {
                chat: "main".into(),
                before: None,
                limit: None,
            })
            .await
            .unwrap();
        let told = history
            .messages
            .iter()
            .find(|m| m.text == "No to \u{201c}Reduce motion\u{201d}.");
        if let Some(told) = told {
            assert_eq!(told.role, Role::User);
            assert!(
                matches!(told.state, MessageState::Failed | MessageState::Uncertain),
                "{told:?}"
            );
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "not shown");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // It was never said to Hermes.
    assert_eq!(received(&h, "turn", 1).await.len(), 1);
}

/// Words typed in a chat where a question from Hermes waits are its answer, not a new turn:
/// a question with more choices than the card has buttons can be answered with any of them.
/// An approval is never answered in words; with only an approval waiting they are a new
/// message, and a turn that ends there leaves the turn that asked, and its card, as they were.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn words_typed_while_a_question_waits_answer_it_but_never_an_approval() {
    let h = start(working_script(), TOKEN).await;
    link_becomes(&h, LinkState::Online).await;
    let asking = turn_under_way(&h, "main", 1).await;
    h.fake.push.send(approval("p1", "main")).unwrap();
    cards_are(&h, &["prompt:p1"]).await;
    let choices: Vec<Value> = (1..=5)
        .map(|n| json!({"id": format!("c{n}"), "label": format!("Choice {n}")}))
        .collect();
    h.fake
        .push
        .send(
            json!({"type": "prompt", "id": "p2", "chat": "main", "kind": "clarify",
                     "text": "Which one?", "choices": choices}),
        )
        .unwrap();
    cards_are(&h, &["prompt:p1", "prompt:p2"]).await;
    let say = |text: &str| ChatSendParams {
        chat: "main".into(),
        text: text.into(),
    };
    let _: ChatSendResult = h.client.request(say("Choice 5")).await.unwrap();
    let answer = received(&h, "prompt.answer", 1).await.remove(0);
    assert_eq!(
        (answer["id"].as_str(), answer["answer"].as_str()),
        (Some("p2"), Some("Choice 5"))
    );
    cards_are(&h, &["prompt:p1"]).await;
    assert_eq!(received(&h, "turn", 1).await.len(), 1, "not a new turn");
    let history: ChatHistoryResult = h
        .client
        .request(ChatHistoryParams {
            chat: "main".into(),
            before: None,
            limit: None,
        })
        .await
        .unwrap();
    let said = history.messages.last().unwrap();
    assert_eq!((said.role, said.text.as_str()), (Role::User, "Choice 5"));

    // Only the approval waits: words do not answer it.
    let _: ChatSendResult = h.client.request(say("once")).await.unwrap();
    let other = received(&h, "turn", 2).await.remove(1)["id"].clone();
    assert_eq!(received(&h, "prompt.answer", 1).await.len(), 1);
    h.fake
        .push
        .send(json!({"type": "turn.done", "turn": other}))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    cards_are(&h, &["prompt:p1"]).await;
    h.fake
        .push
        .send(json!({"type": "turn.done", "turn": asking}))
        .unwrap();
    cards_are(&h, &[]).await;
}
