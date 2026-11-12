//! One example JSON document for every method and event, kept in sync with the types.
//!
//! [`generate`] builds the reference that is committed as `docs/ipc-examples.json`. The unit
//! tests check that every example parses into, and re-serializes identically from, the typed
//! structures, that every method and event has an example, and that the committed file is
//! current (set `UPDATE_IPC_EXAMPLES=1` while running the tests to rewrite it).

use serde_json::{json, Map, Value};

use crate::protocol::Event;

fn message() -> Value {
    json!({
        "id": "m-41", "chat": "main", "role": "assistant", "text": "Hello **there**",
        "rich": [
            {"t": "p", "spans": [{"text": "Hello "}, {"text": "there", "b": true}]},
            {"t": "ul", "items": [[{"text": "one"}], [{"text": "two", "link": "https://example.com"}]]},
            {"t": "code", "text": "ls -l"}
        ],
        "attachments": [{"kind": "image", "name": "cat.png", "path": "/spool/cat.png", "bytes": 2048, "mime": "image/png"}],
        "at": 1_760_000_000_000_u64, "state": "done", "origin": "elsewhere"
    })
}

fn notice() -> Value {
    json!({
        "id": "n-7", "at": 1_760_000_000_000_u64, "kind": "reply", "title": "New reply",
        "body": "Hello there", "chat": "main", "link": "chat/main#m-41", "seen": false
    })
}

fn entry() -> Value {
    json!({
        "id": "c-3", "at": 1_760_000_000_000_u64, "name": "system.run",
        "summary": "uname -a", "state": "ok", "ms": 12
    })
}

fn chat() -> Value {
    json!({
        "id": "main", "title": "Muse", "last_text": "Hello there",
        "last_at": 1_760_000_000_000_u64, "unread": 1, "pending_turns": 0
    })
}

fn account() -> Value {
    json!({"state": "ok", "since": 1_760_000_000_000_u64})
}

fn game() -> Value {
    json!({
        "slug": "snake", "title": "Snake", "runtime": "love2d", "version": "3",
        "versions": ["1", "2", "3"], "size_bytes": 40960, "chat": "6f1c0a52-8d0e-4a4e-9a35-1f2f3f6f9d11",
        "installed_at": 1_760_000_000_000_u64, "state": "installed"
    })
}

fn status() -> Value {
    json!({
        "profile": "dev", "paired": true, "link": "online",
        "agent": {"name": "Muse", "avatar_path": "/data/avatar.png", "activity": {"code": "thinking", "text": "Thinking"}},
        "vm": {"name": "vm-1"}, "token_age_s": 3600, "last_error": "none yet", "mode": "sdk",
        "account": {"state": "none"}, "battery": {"percent": 80, "charging": false},
        "uptime_s": 120, "version": "0.1.0"
    })
}

fn empty() -> Value {
    json!({})
}

type Example = (&'static str, Value, Value);

/// Example `(method, params, result)` triples, in documentation order.
pub fn method_examples() -> Vec<Example> {
    vec![
        (
            "hello",
            json!({"client": "ui", "version": "0.1.0"}),
            json!({
                "daemon_version": "0.1.0", "protocol": 1, "run_id": "r-1a2b3c", "profile": "dev",
                "capabilities": {"leds": false, "voice": true}
            }),
        ),
        (
            "subscribe",
            json!({"after_seq": 4400, "topics": ["chat", "link"]}),
            json!({"seq": 4411, "gap": false}),
        ),
        ("daemon.status", empty(), status()),
        ("daemon.shutdown", empty(), empty()),
        (
            "profile.list",
            empty(),
            json!({
                "active": "dev",
                "profiles": [{"name": "dev", "paired": true, "has_sdk_token": true, "has_account": false}]
            }),
        ),
        ("profile.create", json!({"name": "personal"}), empty()),
        (
            "profile.rename",
            json!({"name": "personal", "new_name": "home"}),
            empty(),
        ),
        ("profile.delete", json!({"name": "home"}), empty()),
        ("profile.switch", json!({"name": "dev"}), empty()),
        (
            "profile.set_sdk_token",
            json!({"name": "dev", "token_path": "/spool/token.txt"}),
            empty(),
        ),
        (
            "pair.start",
            json!({"timeout_s": 600, "force": true}),
            json!({"ble_name": "MuseGadgetA1B2C3"}),
        ),
        ("pair.cancel", empty(), empty()),
        ("pair.unpair", empty(), empty()),
        ("chat.list", empty(), json!({"chats": [chat()]})),
        (
            "chat.create",
            json!({"title": "Trip planning"}),
            json!({"chat": {
                "id": "6f1c0a52-8d0e-4a4e-9a35-1f2f3f6f9d11", "title": "Trip planning", "unread": 0, "pending_turns": 0
            }}),
        ),
        (
            "chat.forget",
            json!({"chat": "6f1c0a52-8d0e-4a4e-9a35-1f2f3f6f9d11"}),
            empty(),
        ),
        (
            "chat.rename",
            json!({"chat": "6f1c0a52-8d0e-4a4e-9a35-1f2f3f6f9d11", "title": "Lisbon in May"}),
            empty(),
        ),
        (
            "chat.history",
            json!({"chat": "main", "before": "m-40", "limit": 50}),
            json!({
                "messages": [message()], "next": "m-1", "complete": false
            }),
        ),
        (
            "chat.send",
            json!({"chat": "main", "text": "hello", "short_replies": true}),
            json!({"turn": "t-19", "message_id": "m-42"}),
        ),
        (
            "chat.send_image",
            json!({"chat": "main", "text": "look", "image_path": "/spool/cat.png"}),
            json!({"turn": "t-20"}),
        ),
        (
            "chat.send_voice",
            json!({"chat": "main", "note": "v-1"}),
            json!({"turn": "t-21"}),
        ),
        (
            "chat.mark_seen",
            json!({"chat": "main", "message_id": "m-41"}),
            empty(),
        ),
        ("chat.cancel", json!({"turn": "t-19"}), empty()),
        (
            "chat.suggest",
            json!({"replies": ["Yes, book it", "Not today", "Show options"]}),
            json!({"chat": "main"}),
        ),
        (
            "voice.ptt_start",
            json!({"chat": "main", "mode": "note"}),
            json!({"note": "v-1"}),
        ),
        (
            "voice.ptt_stop",
            json!({"note": "v-1", "cancel": true}),
            json!({"turn": "t-21"}),
        ),
        ("voice.speak", json!({"message_id": "m-41"}), empty()),
        ("voice.stop_speaking", empty(), empty()),
        (
            "voice.call_start",
            json!({"chat": "main", "full_duplex": true}),
            json!({"call": "call-1"}),
        ),
        ("voice.call_end", empty(), json!({"reason": "user"})),
        ("voice.call_mute", json!({"muted": true}), empty()),
        ("voice.barge_in", empty(), empty()),
        (
            "voice.devices",
            empty(),
            json!({
                "inputs": [{"id": "hw:0", "name": "Built-in mic"}], "outputs": [{"id": "hw:1", "name": "Speaker"}],
                "selected": {"input": "hw:0", "output": "hw:1"}
            }),
        ),
        (
            "notify.list",
            json!({"limit": 20}),
            json!({"items": [notice()], "unseen": 1}),
        ),
        ("notify.seen", json!({"ids": ["n-7"]}), empty()),
        ("notify.clear", empty(), empty()),
        (
            "commands.log",
            json!({"limit": 20}),
            json!({"entries": [entry()]}),
        ),
        (
            "commands.consent.get",
            empty(),
            json!({"shell": true, "file_read": true, "file_write": false, "drop_ins": true, "files": "home"}),
        ),
        (
            "commands.consent.set",
            json!({"file_write": false}),
            json!({"shell": true, "file_read": true, "file_write": false, "drop_ins": true, "files": "home"}),
        ),
        (
            "settings.get",
            json!({"keys": ["appearance.mode"]}),
            json!({"values": {"appearance.mode": "dark"}}),
        ),
        (
            "settings.set",
            json!({"values": {"appearance.mode": "dark", "audio.earcons": false}}),
            empty(),
        ),
        (
            "platform.info",
            empty(),
            json!({"adapter": "generic-sdl", "display": {"w": 1024, "h": 768}}),
        ),
        (
            "platform.volume",
            json!({"set": 20}),
            json!({"percent": 20}),
        ),
        (
            "platform.brightness",
            json!({"set": 60}),
            json!({"percent": 60}),
        ),
        ("platform.leds_test", json!({"zone": "f1"}), empty()),
        ("platform.rumble_test", json!({"ms": 200}), empty()),
        (
            "platform.autostart",
            json!({"set": true}),
            json!({"enabled": true}),
        ),
        (
            "platform.health",
            empty(),
            json!({"uptime_s": 120, "load": [0.1, 0.2, 0.3], "mem_free_kb": 512_000}),
        ),
        ("workshop.list", empty(), json!({"games": [game()]})),
        (
            "workshop.request",
            json!({"text": "make me a snake game"}),
            json!({"chat": "6f1c0a52-8d0e-4a4e-9a35-1f2f3f6f9d11", "turn": "t-30"}),
        ),
        (
            "workshop.modify",
            json!({"slug": "snake", "text": "faster"}),
            json!({"turn": "t-31"}),
        ),
        ("workshop.launch", json!({"slug": "snake"}), empty()),
        (
            "workshop.rollback",
            json!({"slug": "snake", "version": "2"}),
            empty(),
        ),
        ("workshop.remove", json!({"slug": "snake"}), empty()),
        (
            "workshop.log",
            json!({"slug": "snake"}),
            json!({"lines": ["started", "exit 0"]}),
        ),
        (
            "workshop.install",
            json!({"path": "/spool/snake.tar"}),
            empty(),
        ),
        ("account.status", empty(), account()),
        (
            "account.link_start",
            empty(),
            json!({
                "code": "482913", "expires_at": 1_760_000_600_000_u64,
                "listen": {"addr": "192.168.1.20", "port": 8765}, "drop_path": "/spool/account-drop"
            }),
        ),
        ("account.link_cancel", empty(), empty()),
        ("account.unlink", empty(), empty()),
        (
            "mirror.get",
            json!({"kind": "feed", "page": 1}),
            json!({
                "snapshot": {"posts": []}, "fetched_at": 1_760_000_000_000_u64, "live": false
            }),
        ),
        ("mirror.refresh", json!({"kind": "feed"}), empty()),
        (
            "mirror.act",
            json!({"action": "approve", "id": "a-1"}),
            json!({"outcome": "ok"}),
        ),
    ]
}

/// Example `(event, data)` pairs, in documentation order.
pub fn event_examples() -> Vec<(&'static str, Value)> {
    vec![
        ("daemon.profile_changed", json!({"profile": "dev"})),
        (
            "link.state",
            json!({"state": "reconnecting", "since": 1_760_000_000_000_u64, "error": "timeout"}),
        ),
        (
            "agent.activity",
            json!({"code": "thinking", "text": "Thinking"}),
        ),
        (
            "pair.state",
            json!({"state": "advertising", "detail": "MuseGadgetA1B2C3"}),
        ),
        ("chat.message", json!({"message": message()})),
        (
            "chat.delta",
            json!({"chat": "main", "message_id": "m-41", "text_append": " there"}),
        ),
        (
            "chat.turn",
            json!({"turn": "t-19", "chat": "main", "state": "working", "activity": {"code": "tool", "text": "Searching"}}),
        ),
        ("chat.unread", json!({"chat": "main", "unread": 2})),
        ("chat.list_changed", empty()),
        (
            "chat.suggestions",
            json!({"chat": "main", "replies": ["Yes, book it", "Not today"]}),
        ),
        ("voice.level", json!({"source": "mic", "rms": 0.25})),
        (
            "voice.note",
            json!({"note": "v-1", "state": "recording", "seconds": 1.5}),
        ),
        (
            "voice.speaking",
            json!({"message_id": "m-41", "state": "start"}),
        ),
        (
            "voice.call",
            json!({"state": "listening", "reason": "ok", "muted": false, "seconds": 12.0}),
        ),
        (
            "voice.caption",
            json!({"role": "assistant", "text": "Hi", "final": true}),
        ),
        ("notify.new", json!({"notice": notice()})),
        ("notify.count", json!({"unseen": 3})),
        (
            "commands.started",
            json!({"entry": {"id": "c-4", "at": 1_760_000_000_000_u64, "name": "file.read", "summary": "/etc/hostname", "state": "running"}}),
        ),
        ("commands.finished", json!({"entry": entry()})),
        (
            "settings.changed",
            json!({"values": {"audio.earcons": false}}),
        ),
        ("workshop.changed", json!({"slug": "snake"})),
        (
            "workshop.exited",
            json!({"slug": "snake", "code": 0, "tail": "bye"}),
        ),
        ("account.state", account()),
        ("mirror.changed", json!({"kind": "feed"})),
    ]
}

/// Builds the full reference document.
pub fn generate() -> Value {
    let mut methods = Map::new();
    for (name, params, result) in method_examples() {
        methods.insert(name.to_owned(), json!({"params": params, "result": result}));
    }
    let mut events = Map::new();
    for (name, data) in event_examples() {
        let topic = Event::from_parts(name, data.clone())
            .map(|e| e.topic().as_str())
            .unwrap_or("unknown");
        events.insert(name.to_owned(), json!({"topic": topic, "data": data}));
    }
    json!({
        "_comment": "Generated by goblin-ipc (cargo test -p goblin-ipc examples). Do not edit by hand.",
        "protocol": crate::PROTOCOL_VERSION,
        "methods": methods,
        "events": events,
    })
}

/// The reference document as committed: pretty-printed with a trailing newline.
pub fn render() -> String {
    let mut s = serde_json::to_string_pretty(&generate()).unwrap_or_default();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{roundtrip, Method};

    #[test]
    fn every_method_has_a_valid_example() {
        let ex = method_examples();
        let names: Vec<&str> = ex.iter().map(|e| e.0).collect();
        assert_eq!(
            names,
            Method::NAMES,
            "examples must follow Method::NAMES order"
        );
        for (name, params, result) in ex {
            let m =
                Method::from_parts(name, params.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(m.name(), name);
            assert_eq!(
                m.params_value().unwrap(),
                params,
                "{name} params round trip"
            );
            Method::check_result(name, &result).unwrap_or_else(|e| panic!("{name} result: {e}"));
        }
    }

    #[test]
    fn every_event_has_a_valid_example() {
        let ex = event_examples();
        let names: Vec<&str> = ex.iter().map(|e| e.0).collect();
        assert_eq!(names, Event::NAMES);
        for (name, data) in ex {
            Event::check_data(name, &data).unwrap_or_else(|e| panic!("{name}: {e}"));
            let ev = Event::from_parts(name, data).unwrap();
            assert_ne!(ev.topic(), crate::Topic::Unknown, "{name}");
        }
    }

    #[test]
    fn shared_fixtures_round_trip() {
        roundtrip::<crate::types::Message>(&message()).unwrap();
        roundtrip::<crate::types::Notice>(&notice()).unwrap();
        roundtrip::<crate::types::Game>(&game()).unwrap();
    }

    #[test]
    fn committed_reference_is_current() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/ipc-examples.json");
        let fresh = render();
        if std::env::var_os("UPDATE_IPC_EXAMPLES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "docs/ipc-examples.json is stale; run UPDATE_IPC_EXAMPLES=1 cargo test -p goblin-ipc examples"
        );
    }
}
