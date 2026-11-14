//! Container-only regression: stopping the supervisor must stop its foreground group.
use std::{fs, process::Command, thread, time::Duration};

#[test]
fn termination_cleans_foreground_and_preserves_the_callers_group() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("pid");
    let mut helper = Command::new(env!("CARGO_BIN_EXE_goblin-nextui-power"))
        .args(["sh", "-c", "echo $$ > \"$1\"; exec sleep 60", "sh"])
        .arg(&marker)
        .env("SYSTEM_PATH", dir.path())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if fs::read_to_string(&marker).is_ok_and(|s| !s.trim().is_empty()) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let child: i32 = fs::read_to_string(marker).unwrap().trim().parse().unwrap();
    assert!(child > 1);
    assert_eq!(unsafe { libc::kill(helper.id() as i32, libc::SIGTERM) }, 0);
    let mut status = None;
    for _ in 0..100 {
        status = helper.try_wait().unwrap();
        if status.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if status.is_none() {
        helper.kill().unwrap();
    }
    assert_eq!(status.and_then(|s| s.code()), Some(143));
    assert!(!std::path::Path::new(&format!("/proc/{child}")).exists());
    assert!(unsafe { libc::kill(std::process::id() as i32, 0) } == 0);
}
