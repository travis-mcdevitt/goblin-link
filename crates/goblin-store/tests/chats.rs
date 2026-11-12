use goblin_ipc::types::*;
use goblin_store::chats::*;
use goblin_store::StoreError;
use serde_json::json;

fn msg(chat: &str, id: &str, at: u64) -> Message {
    Message {
        id: id.into(),
        chat: chat.into(),
        role: Role::Assistant,
        text: format!("text {id}"),
        rich: None,
        attachments: None,
        reaction: None,
        at,
        state: MessageState::Done,
        origin: Origin::Gadget,
    }
}

fn open(limits: CacheLimits) -> (tempfile::TempDir, ChatCache) {
    let t = tempfile::tempdir().unwrap();
    let c = ChatCache::open(&t.path().join("chats"), limits).unwrap();
    (t, c)
}

fn ids(c: &mut ChatCache, chat: &str) -> Vec<String> {
    c.messages(chat)
        .unwrap()
        .iter()
        .map(|m| m.id.clone())
        .collect()
}

#[test]
fn append_replace_in_place_and_delta() {
    let (_t, mut c) = open(CacheLimits::default());
    assert_eq!(c.upsert(msg("main", "a", 1)).unwrap(), Upsert::Appended);
    assert_eq!(c.upsert(msg("main", "b", 2)).unwrap(), Upsert::Appended);
    let mut b2 = msg("main", "b", 2);
    b2.text = "new".into();
    assert_eq!(c.upsert(b2).unwrap(), Upsert::Replaced);
    assert_eq!(ids(&mut c, "main"), ["a", "b"]);
    assert_eq!(c.messages("main").unwrap()[1].text, "new");
    assert!(c.apply_delta("main", "b", "er").unwrap());
    assert_eq!(c.messages("main").unwrap()[1].text, "newer");
    assert!(!c.apply_delta("main", "zzz", "x").unwrap());
}

#[test]
fn eviction_keeps_the_newest_and_marks_history_incomplete() {
    let (_t, mut c) = open(CacheLimits {
        max_messages_per_chat: 3,
        ..CacheLimits::default()
    });
    c.mark_history_complete("main").unwrap();
    for i in 0..5 {
        c.upsert(msg("main", &format!("m{i}"), i)).unwrap();
    }
    assert_eq!(ids(&mut c, "main"), ["m2", "m3", "m4"]);
    let page = c.page("main", None, 10).unwrap();
    assert!(!page.complete, "evicted history is no longer complete");
    assert_eq!(page.next, None);
}

#[test]
fn paging_walks_backwards() {
    let (_t, mut c) = open(CacheLimits::default());
    for i in 0..7 {
        c.upsert(msg("main", &format!("m{i}"), i)).unwrap();
    }
    c.mark_history_complete("main").unwrap();
    let p1 = c.page("main", None, 3).unwrap();
    assert_eq!(
        p1.messages
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["m4", "m5", "m6"]
    );
    assert_eq!(p1.next.as_deref(), Some("m4"));
    assert!(!p1.complete);
    let p2 = c.page("main", p1.next.as_deref(), 3).unwrap();
    assert_eq!(
        p2.messages
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["m1", "m2", "m3"]
    );
    let p3 = c.page("main", p2.next.as_deref(), 3).unwrap();
    assert_eq!(p3.messages.len(), 1);
    assert_eq!(p3.next, None);
    assert!(p3.complete);
    assert!(matches!(
        c.page("main", Some("nope"), 3),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn cursor_is_opaque_and_survives_restart() {
    let (t, mut c) = open(CacheLimits::default());
    assert_eq!(c.cursor("main").unwrap(), ReplayCursor::default());
    let cur = ReplayCursor {
        stream_seq: Some(json!(41)),
        chat_event_seq: Some(json!({"opaque": "x"})),
    };
    c.set_cursor("main", cur.clone()).unwrap();
    c.upsert(msg("main", "a", 1)).unwrap();
    assert!(c.is_dirty());
    c.flush().unwrap();
    assert!(!c.is_dirty());
    drop(c);
    let mut c = ChatCache::open(&t.path().join("chats"), CacheLimits::default()).unwrap();
    assert_eq!(c.cursor("main").unwrap(), cur);
    assert_eq!(ids(&mut c, "main"), ["a"]);
}

#[test]
fn flush_is_private_and_leaves_no_temp_files() {
    use std::os::unix::fs::PermissionsExt;
    let (t, mut c) = open(CacheLimits::default());
    c.upsert(msg("main", "a", 1)).unwrap();
    c.flush().unwrap();
    let dir = t.path().join("chats");
    let file = dir.join("main.json");
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn byte_budget_drops_oldest_messages_on_flush() {
    let (t, mut c) = open(CacheLimits {
        max_bytes_per_chat: 4000,
        ..CacheLimits::default()
    });
    for i in 0..100 {
        let mut m = msg("main", &format!("m{i:03}"), i);
        m.text = "x".repeat(200);
        c.upsert(m).unwrap();
    }
    c.flush().unwrap();
    let size = std::fs::metadata(t.path().join("chats/main.json"))
        .unwrap()
        .len();
    assert!(size <= 4000, "{size}");
    let kept = ids(&mut c, "main");
    assert!(!kept.is_empty() && kept.last().unwrap() == "m099");
    assert!(kept.len() < 100);
}

#[test]
fn side_chats_add_activity_forget_and_listing() {
    let (t, mut c) = open(CacheLimits::default());
    let id = "6f1c0a52-8d0e-4a4e-9a35-1f2f3f6f9d11";
    assert_eq!(c.add_side_chat(id, "Trip", 100).unwrap().title, "Trip");
    c.add_side_chat(id, "Other title ignored", 200).unwrap();
    assert_eq!(c.side_chats().len(), 1);
    assert_eq!(c.side_chats()[0].title, "Trip");
    c.note_activity(id, &"y".repeat(500), 300, 2).unwrap();
    c.upsert(msg(id, "s1", 300)).unwrap();
    c.upsert(msg("main", "a", 50)).unwrap();
    let list = c.chat_list(1, |chat| u32::from(chat == id)).unwrap();
    assert_eq!(list[0].id, "main");
    assert_eq!(list[0].last_text.as_deref(), Some("text a"));
    assert_eq!(list[0].unread, 1);
    assert_eq!(list[1].id, id);
    assert_eq!(list[1].unread, 2);
    assert_eq!(list[1].pending_turns, 1);
    assert_eq!(list[1].last_text.as_ref().unwrap().chars().count(), 120);
    c.set_unread(id, 0).unwrap();
    c.rename_side_chat(id, "Trip 2").unwrap();
    c.flush().unwrap();
    assert!(t.path().join("chats").join(format!("{id}.json")).exists());

    drop(c);
    let mut c = ChatCache::open(&t.path().join("chats"), CacheLimits::default()).unwrap();
    assert_eq!(c.side_chats()[0].title, "Trip 2");
    c.forget(id).unwrap();
    assert!(c.side_chats().is_empty());
    assert!(matches!(c.forget(id), Err(StoreError::NotFound(_))));
    c.flush().unwrap();
    assert!(!t.path().join("chats").join(format!("{id}.json")).exists());
}

#[test]
fn too_many_side_chats_forget_the_least_recent() {
    let (_t, mut c) = open(CacheLimits {
        max_side_chats: 2,
        ..CacheLimits::default()
    });
    c.add_side_chat("a", "A", 10).unwrap();
    c.add_side_chat("b", "B", 20).unwrap();
    c.note_activity("a", "hi", 30, 0).unwrap();
    c.add_side_chat("c", "C", 40).unwrap();
    let mut left: Vec<_> = c.side_chats().iter().map(|s| s.id.clone()).collect();
    left.sort();
    assert_eq!(left, ["a", "c"], "b was least recently active");
}

#[test]
fn chat_ids_cannot_escape_the_directory() {
    let (_t, mut c) = open(CacheLimits::default());
    for bad in ["../x", "a/b", "", "index", "a.b", &"x".repeat(65)] {
        assert!(
            matches!(c.messages(bad), Err(StoreError::InvalidId(_))),
            "{bad}"
        );
    }
    assert!(c.add_side_chat("main", "m", 0).is_err());
    assert!(c.upsert(msg("../evil", "a", 1)).is_err());
}

#[test]
fn corrupt_files_are_quarantined_not_fatal() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("chats");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.json"), "{garbage").unwrap();
    std::fs::write(dir.join("index.json"), "[garbage").unwrap();
    let mut c = ChatCache::open(&dir, CacheLimits::default()).unwrap();
    assert!(c.side_chats().is_empty());
    assert!(c.messages("main").unwrap().is_empty());
    assert!(dir.join("main.json.corrupt").exists());
    assert!(dir.join("index.json.corrupt").exists());
}

#[test]
fn unknown_enum_values_in_cached_messages_do_not_break_loading() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("chats");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.json"),
        r#"{"messages":[{"id":"a","chat":"main","role":"alien","text":"t","at":1,"state":"warp","origin":"gadget"}]}"#,
    )
    .unwrap();
    let mut c = ChatCache::open(&dir, CacheLimits::default()).unwrap();
    assert_eq!(c.messages("main").unwrap()[0].role, Role::Unknown);
}

/// A message learned late is placed by its time, not at the end; replacing keeps its place.
#[test]
fn upsert_by_time_places_late_messages() {
    let (_dir, mut cache) = open(CacheLimits::default());
    for (id, at) in [("a", 10), ("b", 20), ("c", 30)] {
        cache.upsert(msg("main", id, at)).unwrap();
    }
    cache.upsert_by_time(msg("main", "late", 20)).unwrap();
    cache.upsert_by_time(msg("main", "newest", 40)).unwrap();
    cache.upsert_by_time(msg("main", "late", 20)).unwrap();
    let ids: Vec<&str> = cache
        .messages("main")
        .unwrap()
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(ids, ["a", "b", "late", "c", "newest"]);
    // A stored message whose time is corrected moves forward as well.
    cache.upsert_by_time(msg("main", "late", 35)).unwrap();
    let ids: Vec<String> = cache
        .messages("main")
        .unwrap()
        .iter()
        .map(|m| m.id.clone())
        .collect();
    assert_eq!(ids, ["a", "b", "c", "late", "newest"]);
}

#[test]
fn older_history_goes_in_front_without_repeats_and_survives_restart() {
    let (t, mut c) = open(CacheLimits {
        max_messages_per_chat: 6,
        ..CacheLimits::default()
    });
    c.upsert(msg("main", "m10", 100)).unwrap();
    c.upsert(msg("main", "m11", 110)).unwrap();
    assert_eq!(c.history_floor("main").unwrap(), None);
    // The newest page overlaps what is cached: a known id and one not older are left out.
    let page = vec![
        (9, msg("main", "m9", 90)),
        (10, msg("main", "m10", 95)),
        (12, msg("main", "late", 120)),
        (8, msg("main", "m8", 80)),
    ];
    assert_eq!(c.add_older("main", page, 8).unwrap(), 2);
    assert_eq!(ids(&mut c, "main"), ["m8", "m9", "m10", "m11"]);
    assert_eq!(c.history_floor("main").unwrap(), Some(8));
    assert!(!c.full("main").unwrap());
    // Room for two more only: the newest two are kept and the floor stops at them.
    let page = (4..8).map(|i| (i, msg("main", &format!("m{i}"), i as u64 * 10)));
    assert_eq!(c.add_older("main", page.collect(), 4).unwrap(), 2);
    assert_eq!(ids(&mut c, "main"), ["m6", "m7", "m8", "m9", "m10", "m11"]);
    assert_eq!(c.history_floor("main").unwrap(), Some(6));
    assert!(c.full("main").unwrap());
    c.flush().unwrap();
    let mut again = ChatCache::open(&t.path().join("chats"), CacheLimits::default()).unwrap();
    assert_eq!(again.history_floor("main").unwrap(), Some(6));
    // Dropping the oldest for a new message forgets the floor, so paging starts over.
    c.upsert(msg("main", "m12", 120)).unwrap();
    assert_eq!(c.history_floor("main").unwrap(), None);
}
