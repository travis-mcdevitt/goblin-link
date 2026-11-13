//! Correlation and quiet-timeout tests for [`MuseTurn`].
//!
//! The upstream SDK's own `muse_turn` tests are not vendored in this repo (only
//! the BLE regression test is), so these are written against the documented
//! semantics of `muse_turn.py` rather than copied. They exercise the same
//! cases the module's docstrings call out: holding events before the
//! acknowledgement, binding by parent chain, dropping other chats' events,
//! ignoring parentless pre-request answers, replay-scope filtering on an owned
//! chat, the terminal-task and quiet-timeout completion rules, and the size
//! bounds.

use super::*;
use serde_json::json;

fn event(name: &str, payload: Value) -> Value {
    json!({"type": "event", "event": name, "payload": payload})
}

fn ack(message_id: &str, session_id: Option<&str>, is_thread: bool) -> Value {
    let mut result = serde_json::Map::new();
    result.insert("message_id".into(), json!(message_id));
    if let Some(s) = session_id {
        result.insert("session_id".into(), json!(s));
    }
    if is_thread {
        result.insert("is_thread".into(), json!(true));
    }
    json!({ "result": Value::Object(result) })
}

#[test]
fn acknowledge_requires_user_message_id() {
    let mut turn = MuseTurn::new(None);
    assert!(turn
        .acknowledge(&json!({"result": {}}), false, 0.0)
        .is_err());
    let held = turn
        .acknowledge(&ack("u1", Some("s1"), false), false, 0.0)
        .unwrap();
    assert!(held.is_empty());
    assert!(turn.acknowledged);
    assert_eq!(turn.session_id.as_deref(), Some("s1"));
}

#[test]
fn streamed_reply_then_done_then_complete() {
    let mut turn = MuseTurn::new(Some("s1".into()));
    turn.acknowledge(&ack("u1", Some("s1"), false), false, 0.0)
        .unwrap();
    // A child message of the user message.
    let ev = event(
        "delta.text_append",
        json!({"session_id":"s1","message_id":"a1","reply_to_message_id":"u1","text":"Hel"}),
    );
    let out = turn.feed(&ev, 1.0).unwrap();
    assert_eq!(out.len(), 1);
    match &out[0] {
        TurnEvent::Reply(r) => {
            assert_eq!(r.text, "Hel");
            assert!(r.bound);
            assert!(!r.done);
        }
        _ => panic!("expected reply"),
    }
    let ev = event(
        "delta.text_append",
        json!({"session_id":"s1","message_id":"a1","reply_to_message_id":"u1","text":"lo"}),
    );
    turn.feed(&ev, 2.0).unwrap();
    let ev = event(
        "delta.message_done",
        json!({"session_id":"s1","message_id":"a1","reply_to_message_id":"u1"}),
    );
    turn.feed(&ev, 3.0).unwrap();
    assert_eq!(turn.replies().next().unwrap().text, "Hello");
    assert!(turn.replies().next().unwrap().done);
    // Not complete immediately (no terminal task, within quiet window)...
    assert!(!turn.complete(3.0));
    // ...but complete after REPLY_QUIET_S of silence.
    assert!(turn.complete(3.0 + REPLY_QUIET_S));
}

#[test]
fn events_before_acknowledge_are_held_and_replayed() {
    let mut turn = MuseTurn::new(Some("s1".into()));
    let ev = event(
        "delta.text_append",
        json!({"session_id":"s1","message_id":"a1","reply_to_message_id":"u1","text":"hi"}),
    );
    // Held: not acknowledged yet.
    assert!(turn.feed(&ev, 1.0).unwrap().is_empty());
    let held = turn
        .acknowledge(&ack("u1", Some("s1"), false), false, 2.0)
        .unwrap();
    assert_eq!(held.len(), 1);
    match &held[0] {
        TurnEvent::Reply(r) => assert_eq!(r.text, "hi"),
        _ => panic!("expected held reply"),
    }
}

#[test]
fn agent_status_sets_busy_and_emits_status() {
    let mut turn = MuseTurn::new(Some("s1".into()));
    turn.acknowledge(&ack("u1", Some("s1"), false), false, 0.0)
        .unwrap();
    let ev = event(
        "agent.status",
        json!({"session_id":"s1","reply_to_message_id":"u1","activity_code":"thinking","activity_text":"Thinking\u{0007}"}),
    );
    let out = turn.feed(&ev, 1.0).unwrap();
    assert!(turn.busy);
    assert_eq!(turn.activity_code.as_deref(), Some("thinking"));
    match &out[0] {
        TurnEvent::Status(s) => {
            assert_eq!(s.activity_code.as_deref(), Some("thinking"));
            assert_eq!(s.activity_text.as_deref(), Some("Thinking")); // control char stripped
        }
        _ => panic!("expected status"),
    }
}

#[test]
fn task_completed_finishes_and_settles() {
    let mut turn = MuseTurn::new(Some("s1".into()));
    turn.acknowledge(&ack("u1", Some("s1"), false), false, 0.0)
        .unwrap();
    let ev = event(
        "task.status",
        json!({"session_id":"s1","reply_to_message_id":"u1","status":"completed"}),
    );
    let out = turn.feed(&ev, 1.0).unwrap();
    assert!(turn.task_finished);
    assert!(!turn.busy);
    assert!(matches!(out[0], TurnEvent::Finished(_)));
    // Empty-reply turn settles only after REPLY_QUIET_S.
    assert!(!turn.settled(1.0));
    assert!(turn.settled(1.0 + REPLY_QUIET_S));
}

#[test]
fn task_completed_with_reply_settles_after_grace() {
    let mut turn = MuseTurn::new(Some("s1".into()));
    turn.acknowledge(&ack("u1", Some("s1"), false), false, 0.0)
        .unwrap();
    turn.feed(
        &event(
            "delta.message_done",
            json!({"session_id":"s1","message_id":"a1","reply_to_message_id":"u1","display_text":"done"}),
        ),
        1.0,
    )
    .unwrap();
    turn.feed(
        &event(
            "task.status",
            json!({"session_id":"s1","reply_to_message_id":"u1","status":"completed"}),
        ),
        1.0,
    )
    .unwrap();
    assert!(!turn.settled(1.0));
    assert!(turn.settled(1.0 + EMPTY_REPLY_GRACE_S));
}

#[test]
fn events_from_another_chat_are_dropped() {
    let mut turn = MuseTurn::new(Some("s1".into()));
    turn.acknowledge(&ack("u1", Some("s1"), false), false, 0.0)
        .unwrap();
    let ev = event(
        "delta.text_append",
        json!({"session_id":"other","message_id":"a1","reply_to_message_id":"u1","text":"x"}),
    );
    assert!(turn.feed(&ev, 1.0).unwrap().is_empty());
}

#[test]
fn parentless_message_before_request_is_ignored() {
    // ignore_parentless_until in the future: a parentless assistant message is
    // an answer to something else (e.g. a stop request) and is dropped.
    let mut turn = MuseTurn::with_options(Some("s1".into()), false, None, 100.0, 0.0);
    turn.acknowledge(&ack("u1", Some("s1"), false), false, 0.0)
        .unwrap();
    let ev = event(
        "delta.text_append",
        json!({"session_id":"s1","message_id":"old","text":"ignore me"}),
    );
    assert!(turn.feed(&ev, 1.0).unwrap().is_empty());
    assert!(turn.replies().next().is_none());
    // Even once the window passes, the id stays ignored.
    let ev2 = event(
        "delta.text_append",
        json!({"session_id":"s1","message_id":"old","text":"still ignored"}),
    );
    assert!(turn.feed(&ev2, 200.0).unwrap().is_empty());
}

#[test]
fn foreign_parent_status_rejected_on_shared_chat() {
    let mut turn = MuseTurn::new(Some("s1".into()));
    turn.acknowledge(&ack("u1", Some("s1"), false), false, 0.0)
        .unwrap();
    // A status whose parent is some other request's message: ignored, no busy.
    let ev = event(
        "agent.status",
        json!({"session_id":"s1","reply_to_message_id":"other","activity_code":"working"}),
    );
    assert!(turn.feed(&ev, 1.0).unwrap().is_empty());
    assert!(!turn.busy);
}

#[test]
fn owns_chat_requires_thread_acknowledgement() {
    let mut turn = MuseTurn::with_options(
        Some("s1".into()),
        true,
        Some(ReplayScope::default()),
        0.0,
        0.0,
    );
    // is_thread missing -> rejected.
    assert!(turn
        .acknowledge(&ack("u1", Some("s1"), false), false, 0.0)
        .is_err());
    let mut turn = MuseTurn::with_options(
        Some("s1".into()),
        true,
        Some(ReplayScope::default()),
        0.0,
        0.0,
    );
    assert!(turn
        .acknowledge(&ack("u1", Some("s1"), true), false, 0.0)
        .is_ok());
}

#[test]
fn owns_chat_filters_replayed_ids() {
    let mut scope = ReplayScope::default();
    scope.retire([(IdKind::Message, "replayed".to_string())]);
    let mut turn = MuseTurn::with_options(Some("s1".into()), true, Some(scope), 0.0, 0.0);
    turn.acknowledge(&ack("u1", Some("s1"), true), false, 0.0)
        .unwrap();
    // An event whose own id was retired by an earlier turn is a replay.
    let ev = event(
        "delta.text_append",
        json!({"session_id":"s1","message_id":"replayed","reply_to_message_id":"u1","text":"x"}),
    );
    assert!(turn.feed(&ev, 1.0).unwrap().is_empty());
    // A fresh id on the owned chat still binds (scoped).
    let ev2 = event(
        "delta.text_append",
        json!({"session_id":"s1","message_id":"fresh","text":"y"}),
    );
    let out = turn.feed(&ev2, 2.0).unwrap();
    assert_eq!(out.len(), 1);
}

#[test]
fn retire_feeds_ids_back_into_scope() {
    let mut turn = MuseTurn::with_options(
        Some("s1".into()),
        true,
        Some(ReplayScope::default()),
        0.0,
        0.0,
    );
    turn.acknowledge(&ack("u1", Some("s1"), true), false, 0.0)
        .unwrap();
    turn.feed(
        &event(
            "delta.text_append",
            json!({"session_id":"s1","message_id":"a1","text":"hi"}),
        ),
        1.0,
    )
    .unwrap();
    turn.retire();
    let scope = turn.take_replay_scope().unwrap();
    assert!(scope.ids.contains(&(IdKind::Message, "a1".to_string())));
    assert!(scope.ids.contains(&(IdKind::Message, "u1".to_string())));
}

#[test]
fn oversized_reply_text_is_an_error() {
    let mut turn = MuseTurn::new(Some("s1".into()));
    turn.acknowledge(&ack("u1", Some("s1"), false), false, 0.0)
        .unwrap();
    let big = "a".repeat(65 * 1024);
    let ev = event(
        "delta.text_append",
        json!({"session_id":"s1","message_id":"a1","reply_to_message_id":"u1","text": big}),
    );
    assert_eq!(
        turn.feed(&ev, 1.0).unwrap_err(),
        TurnError("Muse returned more than 64 KiB of reply text")
    );
}

#[test]
fn too_many_pending_events_is_an_error() {
    let mut turn = MuseTurn::new(Some("s1".into()));
    for i in 0..64 {
        let ev = event(
            "agent.status",
            json!({"session_id":"s1","activity_code":"working","i": i}),
        );
        turn.feed(&ev, 1.0).unwrap();
    }
    let ev = event(
        "agent.status",
        json!({"session_id":"s1","activity_code":"working"}),
    );
    assert!(turn.feed(&ev, 1.0).is_err());
}

#[test]
fn follow_up_keeps_turn_open_until_quiet() {
    let mut turn = MuseTurn::new(Some("s1".into()));
    turn.acknowledge(&ack("u1", Some("s1"), false), false, 0.0)
        .unwrap();
    turn.feed(
        &event(
            "delta.message_done",
            json!({"session_id":"s1","message_id":"a1","reply_to_message_id":"u1","display_text":"ok"}),
        ),
        1.0,
    )
    .unwrap();
    assert!(turn.complete(1.0 + REPLY_QUIET_S));
    // A follow-up message keeps the turn open.
    turn.expect_follow_up();
    assert!(!turn.complete(1.0 + REPLY_QUIET_S));
    turn.acknowledge(&ack("u2", Some("s1"), false), true, 10.0)
        .unwrap();
    // Not complete until DETAIL_REPLY_QUIET_S of silence (no reply to u2 yet).
    assert!(!turn.complete(10.0 + REPLY_QUIET_S));
    assert!(turn.complete(10.0 + DETAIL_REPLY_QUIET_S));
}

#[test]
fn transcript_text_reads_last_assistant_text() {
    let v = json!({"messages":[
        {"role":"user","content":[{"type":"text","text":"q"}]},
        {"role":"assistant","id":"a1","content":[
            {"type":"text","text":"line1"},
            {"type":"image"},
            {"type":"text","text":"line2"}
        ]}
    ]});
    assert_eq!(transcript_text(&v, Some("a1")), "line1\nline2");
    assert_eq!(transcript_text(&json!("plain"), None), "plain");
}

/// Shapes observed live on 2026-11-11 for a main-chat reply: message_start without parent,
/// text_append naming the reply itself as parent, message_done without parent.
#[test]
fn main_chat_reply_with_self_parented_deltas() {
    use serde_json::json;
    let mut turn = MuseTurn::with_options(None, false, None, 0.0, 100.0);
    let ack = json!({"ok": true, "result": {"message_id": "u1"}});
    let held = turn.acknowledge(&ack, false, 100.0).unwrap();
    assert!(held.is_empty());
    let ev = |name: &str, payload: serde_json::Value| json!({"type": "event", "event": name, "seq": 1, "payload": payload});
    let mut out = Vec::new();
    out.extend(
        turn.feed(
            &ev(
                "message.user",
                json!({"message_id": "u1", "role": "user", "event_name": "message.user"}),
            ),
            101.0,
        )
        .unwrap(),
    );
    out.extend(
        turn.feed(
            &ev("delta.message_start", json!({"message_id": "a1"})),
            101.5,
        )
        .unwrap(),
    );
    out.extend(
        turn.feed(
            &ev(
                "delta.text_append",
                json!({"message_id": "a1", "parent_message_id": "a1", "text": "MAIN "}),
            ),
            102.0,
        )
        .unwrap(),
    );
    out.extend(
        turn.feed(
            &ev(
                "delta.text_append",
                json!({"message_id": "a1", "parent_message_id": "a1", "text": "OK"}),
            ),
            102.1,
        )
        .unwrap(),
    );
    out.extend(
        turn.feed(
            &ev("delta.message_done", json!({"message_id": "a1"})),
            102.2,
        )
        .unwrap(),
    );
    let last = out.iter().rev().find_map(|e| match e {
        TurnEvent::Reply(r) => Some(r.clone()),
        _ => None,
    });
    let last = last.expect("a reply event");
    assert_eq!(last.text, "MAIN OK");
    assert!(last.done);
}

/// Live shape: `reply_to_message_id` is present but "" on every delta. It must read as
/// "no parent", not as an unknown parent (which drops the whole reply).
#[test]
fn empty_reply_to_is_no_parent() {
    use serde_json::json;
    let mut turn = MuseTurn::with_options(None, false, None, 0.0, 100.0);
    turn.acknowledge(&json!({"result": {"message_id": "u1"}}), false, 100.0)
        .unwrap();
    let ev = |name: &str, payload: serde_json::Value| json!({"type": "event", "event": name, "payload": payload});
    let base = |extra: serde_json::Value| {
        let mut p = json!({"message_id": "a1", "reply_to_message_id": "", "session_id": "s-main", "is_thread": false});
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        p
    };
    let mut out = turn
        .feed(&ev("delta.message_start", base(json!({}))), 101.0)
        .unwrap();
    out.extend(
        turn.feed(
            &ev(
                "delta.text_append",
                base(json!({"parent_message_id": "a1", "text": "MAIN OK"})),
            ),
            101.1,
        )
        .unwrap(),
    );
    out.extend(
        turn.feed(
            &ev("delta.message_done", base(json!({"parent_message_id": ""}))),
            101.2,
        )
        .unwrap(),
    );
    let done = out
        .iter()
        .any(|e| matches!(e, TurnEvent::Reply(r) if r.done && r.text == "MAIN OK"));
    assert!(done, "reply with empty reply_to must be accepted: {out:?}");
}

#[test]
fn native_choices_follow_the_bound_reply_not_other_chats() {
    let mut turn = MuseTurn::new(Some("s1".into()));
    turn.acknowledge(&ack("u1", Some("s1"), false), false, 0.0)
        .unwrap();
    let payload = json!({"session_id":"s1", "message_id":"a1", "reply_to_message_id":"u1",
        "display_text":"Choose [[hatch_widget:w]]", "widgets":[{"widget_id":"w", "kind":"option", "data":{"options":["Yes", "No"]}}]});
    let out = turn
        .feed(&event("message.assistant", payload.clone()), 1.0)
        .unwrap();
    let TurnEvent::Reply(reply) = &out[0] else {
        panic!("expected reply")
    };
    assert_eq!(reply.choices, ["Yes", "No"]);
    assert!(!reply.display_text.contains("hatch_widget"));
    let mut other = payload;
    other["session_id"] = json!("other");
    other["reply_to_message_id"] = json!("unrelated");
    assert!(turn
        .feed(&event("message.assistant", other), 2.0)
        .unwrap()
        .is_empty());
}

#[test]
fn terminal_event_without_widget_metadata_keeps_resolved_choices() {
    let payload = json!({"display_text":"[[hatch_widget:w]]", "widgets":[{"widget_id":"w", "kind":"option", "data":{"options":["Yes"]}}]});
    let first = update_reply(
        &Reply::new("a".into()),
        "message.assistant",
        payload.as_object().unwrap(),
    )
    .unwrap();
    let done = update_reply(&first, "delta.message_done", json!({}).as_object().unwrap()).unwrap();
    assert_eq!(done.display_text, first.display_text);
    assert_eq!(done.choices, ["Yes"]);
    let revised = update_reply(
        &done,
        "message.assistant",
        json!({"display_text":"Never mind"}).as_object().unwrap(),
    )
    .unwrap();
    assert!(revised.choices.is_empty());
}
