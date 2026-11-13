use std::fs;
use std::time::{Duration, SystemTime};

use goblin_ipc::types::{CommandState, ConsentUpdate, NoticeKind, Route};
use goblin_store::commands::*;
use goblin_store::inbox::*;
use goblin_store::spool::Spool;
use goblin_store::StoreError;

fn notice(title: &str) -> NewNotice {
    NewNotice {
        at: 1,
        kind: NoticeKind::Reply,
        title: title.into(),
        body: None,
        chat: Some("main".into()),
        link: Some(Route("chat/main".into())),
    }
}

#[test]
fn inbox_is_bounded_newest_first_and_tracks_seen() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("n.json");
    let mut inbox = Inbox::open(&path, 3).unwrap();
    for i in 0..5 {
        inbox.push(notice(&format!("t{i}")));
    }
    let titles: Vec<_> = inbox.list(None).into_iter().map(|n| n.title).collect();
    assert_eq!(titles, ["t4", "t3", "t2"]);
    assert_eq!(inbox.list(Some(1)).len(), 1);
    assert_eq!(inbox.unseen(), 3);
    let ids: Vec<String> = vec![inbox.list(None)[0].id.clone(), "n-999".into()];
    assert_eq!(inbox.mark_seen(Some(&ids)), 1);
    assert_eq!(inbox.unseen(), 2);
    assert_eq!(inbox.mark_seen(Some(&ids)), 0, "already seen");
    inbox.save().unwrap();
    drop(inbox);

    let mut inbox = Inbox::open(&path, 3).unwrap();
    assert_eq!(inbox.unseen(), 2);
    let next = inbox.push(notice("t5"));
    assert_eq!(next.id, "n-5", "ids keep increasing across restarts");
    assert_eq!(inbox.mark_seen(None), 2);
    assert_eq!(inbox.unseen(), 0);
    inbox.clear();
    assert!(inbox.list(None).is_empty());
    inbox.save().unwrap();
    assert!(Inbox::open(&path, 3).unwrap().list(None).is_empty());
}

#[test]
fn inbox_smaller_limit_on_reopen_and_corrupt_file() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("n.json");
    let mut inbox = Inbox::open(&path, 10).unwrap();
    for i in 0..6 {
        inbox.push(notice(&format!("t{i}")));
    }
    inbox.save().unwrap();
    assert_eq!(Inbox::open(&path, 2).unwrap().list(None).len(), 2);
    fs::write(&path, "oops").unwrap();
    assert!(Inbox::open(&path, 2).unwrap().list(None).is_empty());
    assert!(t.path().join("n.json.corrupt").exists());
}

#[test]
fn summaries_are_one_line_and_at_most_120_chars() {
    assert_eq!(summarize("ls\n  -l\t/tmp\r\n"), "ls -l /tmp");
    assert_eq!(summarize("a\u{7}b"), "a b");
    let long = "é".repeat(500);
    let s = summarize(&long);
    assert_eq!(s.chars().count(), 120);
    assert!(s.ends_with('…'));
    assert_eq!(summarize(&"x".repeat(120)).chars().count(), 120);
    assert!(!summarize(&"x".repeat(120)).ends_with('…'));
    assert_eq!(summarize(""), "");
}

#[test]
fn command_log_is_a_bounded_ring_with_lifecycle() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("c.json");
    let mut log = CommandLog::open(&path, 3).unwrap();
    let e = log.start("system.run", "echo hi\nsecond line", 10);
    assert_eq!(e.state, CommandState::Running);
    assert_eq!(e.summary, "echo hi second line");
    assert_eq!(log.running(), 1);
    let done = log.finish(&e.id, CommandState::Ok, 42).unwrap();
    assert_eq!((done.state, done.ms), (CommandState::Ok, Some(42)));
    assert_eq!(log.running(), 0);
    let r = log.refused("file.write", "/etc/passwd", 11);
    assert_eq!(r.state, CommandState::Refused);
    for i in 0..3 {
        log.start("device.health", &format!("h{i}"), 20 + i);
    }
    assert_eq!(log.entries(None).len(), 3);
    assert!(log.finish(&e.id, CommandState::Ok, 1).is_none(), "evicted");
    assert_eq!(log.entries(Some(1))[0].summary, "h2");
    log.save().unwrap();
    drop(log);

    // Entries left running by a dead daemon become errors.
    let log = CommandLog::open(&path, 3).unwrap();
    assert!(log
        .entries(None)
        .iter()
        .all(|e| e.state == CommandState::Error));
    assert_eq!(log.running(), 0);
}

#[test]
fn in_memory_log_never_writes() {
    let mut log = CommandLog::in_memory(5);
    log.start("x", "y", 1);
    log.save().unwrap();
}

#[test]
fn consent_defaults_to_new_profile_and_persists_partial_updates() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("consent.json");
    let mut c = ConsentStore::open(&path).unwrap();
    assert_eq!(c.get(), goblin_ipc::types::Consent::for_new_profile());
    let after = c
        .update(&ConsentUpdate {
            shell: Some(true),
            ..Default::default()
        })
        .unwrap();
    assert!(after.shell && after.file_read);
    drop(c);
    let c = ConsentStore::open(&path).unwrap();
    assert!(c.get().shell && !c.get().drop_ins);
    fs::write(&path, "garbage").unwrap();
    assert_eq!(
        ConsentStore::open(&path).unwrap().get(),
        goblin_ipc::types::Consent::for_new_profile(),
        "corrupt -> new profile permissions"
    );
}

#[test]
fn spool_creates_unique_private_files() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let spool = Spool::open(&t.path().join("spool")).unwrap();
    assert_eq!(
        fs::metadata(spool.dir()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let mut seen = std::collections::HashSet::new();
    for _ in 0..50 {
        let (p, _f) = spool.create("note", "wav").unwrap();
        assert!(p.starts_with(spool.dir()));
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(seen.insert(p));
    }
    let (p, _f) = spool.create("../../etc/x y", "w/av").unwrap();
    assert_eq!(p.parent().unwrap(), spool.dir());
    assert!(!p.file_name().unwrap().to_string_lossy().contains('/'));
}

#[test]
fn spool_cleanup_removes_only_stale_regular_files() {
    let t = tempfile::tempdir().unwrap();
    let spool = Spool::open(&t.path().join("spool")).unwrap();
    let old = spool.dir().join("old.bin");
    let fresh = spool.dir().join("fresh.bin");
    fs::write(&old, vec![0u8; 10]).unwrap();
    fs::write(&fresh, "x").unwrap();
    let day_ago = SystemTime::now() - Duration::from_secs(25 * 3600);
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(day_ago)
        .unwrap();
    fs::create_dir(spool.dir().join("subdir")).unwrap();
    std::os::unix::fs::symlink(&old, spool.dir().join("link")).unwrap();

    let report = spool.cleanup(Duration::from_secs(24 * 3600)).unwrap();
    assert_eq!((report.removed, report.bytes), (1, 10));
    assert!(!old.exists() && fresh.exists());
    assert!(spool.dir().join("subdir").exists());
    assert!(
        fs::symlink_metadata(spool.dir().join("link")).is_ok(),
        "symlinks untouched"
    );
    let again = spool
        .cleanup_at(
            Duration::from_secs(60),
            SystemTime::now() + Duration::from_secs(3600),
        )
        .unwrap();
    assert_eq!(again.removed, 1, "fresh.bin is stale an hour later");
}

#[test]
fn spool_resolve_rejects_paths_outside() {
    let t = tempfile::tempdir().unwrap();
    let spool = Spool::open(&t.path().join("spool")).unwrap();
    let inside = spool.dir().join("a.png");
    fs::write(&inside, "x").unwrap();
    assert_eq!(spool.resolve(&inside).unwrap(), inside);
    let outside = t.path().join("secret.txt");
    fs::write(&outside, "s").unwrap();
    assert!(matches!(
        spool.resolve(&outside),
        Err(StoreError::OutsideDirectory(_))
    ));
    let dotdot = spool.dir().join("..").join("secret.txt");
    assert!(matches!(
        spool.resolve(&dotdot),
        Err(StoreError::OutsideDirectory(_))
    ));
    let link = spool.dir().join("sneaky");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    assert!(matches!(
        spool.resolve(&link),
        Err(StoreError::OutsideDirectory(_))
    ));
    assert!(spool.resolve(&spool.dir().join("missing")).is_err());
    assert!(
        spool.resolve(spool.dir()).is_err(),
        "a directory is not a file"
    );
}

/// The device-wide consent file is handed down to each profile once; after that a new
/// profile starts with the least.
#[test]
fn consent_moves_into_the_profiles_once() {
    use goblin_ipc::types::{Consent, FileScope};
    use goblin_store::{Layout, ProfileStore};
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path());
    // Two profiles from before, with no consent of their own, and a device-wide file.
    for name in ["work", "personal"] {
        std::fs::create_dir_all(layout.profiles_dir().join(name)).unwrap();
    }
    std::fs::write(
        layout.consent_file(),
        r#"{"shell":false,"file_read":true,"file_write":true,"drop_ins":true}"#,
    )
    .unwrap();
    let profiles = ProfileStore::open(layout.clone()).unwrap();
    profiles.migrate_consent().unwrap();
    let read = |name: &str| -> Consent {
        let file = profiles.profile(name).unwrap().consent_file();
        serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
    };
    for name in ["work", "personal"] {
        let c = read(name);
        assert!(!c.shell && c.drop_ins, "{name} keeps what the device had");
        assert_eq!(c.files, FileScope::Device);
    }
    assert!(!layout.consent_file().exists());

    // A profile created now, and one that appears without a file, get the least.
    profiles.create("guest").unwrap();
    std::fs::create_dir_all(layout.profiles_dir().join("found")).unwrap();
    profiles.migrate_consent().unwrap();
    for name in ["guest", "found"] {
        assert_eq!(read(name), Consent::for_new_profile(), "{name}");
    }
    // Changing one profile's consent leaves the others alone.
    assert!(read("work").drop_ins);
}
