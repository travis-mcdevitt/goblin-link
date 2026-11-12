//! Runs the real Brick hook scripts that only touch ordinary files (autostart and
//! registration) against temporary directories.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn hook(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../platforms/tg5040-brick-nextui/hooks")
        .join(name)
}

fn run(script: &str, envs: &[(&str, &Path)], args: &[&str]) -> Output {
    let mut cmd = Command::new("sh");
    cmd.arg(hook(script)).args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("sh runs")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

/// An executable stand-in for the pak's on-boot.sh.
fn boot_script(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let boot = dir.join("on-boot.sh");
    std::fs::write(&boot, "#!/bin/sh\n").expect("write");
    std::fs::set_permissions(&boot, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    boot
}

/// Without a boot script to run (the hooks are not inside a pak), enabling fails and
/// auto.sh is left alone.
#[test]
fn autostart_enable_needs_a_boot_script() {
    let dir = tempfile::tempdir().expect("tempdir");
    let auto = dir.path().join("auto.sh");
    let original = "#!/bin/sh\n";
    std::fs::write(&auto, original).expect("write");
    let missing = dir.path().join("on-boot.sh");
    let envs = [
        ("GOBLIN_AUTO_SH", auto.as_path()),
        ("GOBLIN_BOOT_SH", missing.as_path()),
    ];
    assert!(!run("autostart.sh", &envs, &["enable"]).status.success());
    assert_eq!(stdout(&run("autostart.sh", &envs, &["status"])), "disabled");
    assert_eq!(std::fs::read_to_string(&auto).expect("read"), original);
}

#[test]
fn autostart_enable_disable_preserves_existing_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let auto = dir.path().join("auto.sh");
    // Deliberately no trailing newline.
    std::fs::write(&auto, "#!/bin/sh\n\ntest -f x && x # Other.pak-on-boot").expect("write");
    let boot = boot_script(dir.path());
    let envs = [
        ("GOBLIN_AUTO_SH", auto.as_path()),
        ("GOBLIN_BOOT_SH", boot.as_path()),
    ];

    assert_eq!(stdout(&run("autostart.sh", &envs, &["status"])), "disabled");
    assert!(run("autostart.sh", &envs, &["enable"]).status.success());
    assert_eq!(stdout(&run("autostart.sh", &envs, &["status"])), "enabled");
    // Idempotent.
    assert!(run("autostart.sh", &envs, &["enable"]).status.success());
    let text = std::fs::read_to_string(&auto).expect("read");
    assert_eq!(text.matches("goblin-link-on-boot").count(), 1, "{text}");
    assert!(
        text.contains(&format!("\"{}\" &", boot.display())),
        "{text}"
    );
    assert!(text.contains("Other.pak-on-boot\n"), "{text}");

    assert!(run("autostart.sh", &envs, &["disable"]).status.success());
    assert_eq!(stdout(&run("autostart.sh", &envs, &["status"])), "disabled");
    let text = std::fs::read_to_string(&auto).expect("read");
    assert!(
        text.contains("Other.pak-on-boot") && !text.contains("goblin-link"),
        "{text}"
    );
}

#[test]
fn autostart_round_trip_is_byte_identical_for_newline_terminated_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let auto = dir.path().join("auto.sh");
    let original = "#!/bin/sh\n\ntest -f x && x # Other.pak-on-boot\n";
    std::fs::write(&auto, original).expect("write");
    let boot = boot_script(dir.path());
    let envs = [
        ("GOBLIN_AUTO_SH", auto.as_path()),
        ("GOBLIN_BOOT_SH", boot.as_path()),
    ];
    assert!(run("autostart.sh", &envs, &["enable"]).status.success());
    assert!(run("autostart.sh", &envs, &["disable"]).status.success());
    assert_eq!(std::fs::read_to_string(&auto).expect("read"), original);
}

#[test]
fn autostart_creates_missing_file_and_rejects_bad_usage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let auto = dir.path().join("auto.sh");
    let boot = boot_script(dir.path());
    let envs = [
        ("GOBLIN_AUTO_SH", auto.as_path()),
        ("GOBLIN_BOOT_SH", boot.as_path()),
    ];
    assert!(run("autostart.sh", &envs, &["enable"]).status.success());
    assert!(std::fs::read_to_string(&auto)
        .expect("read")
        .starts_with("#!/bin/sh"));
    assert!(!run("autostart.sh", &envs, &["bogus"]).status.success());
}

#[test]
fn register_tool_round_trip_and_ownership() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tools = dir.path().join("tools");
    let games = dir.path().join("games");
    let envs = [
        ("GOBLIN_TOOLS_DIR", tools.as_path()),
        ("GOBLIN_GAMES_DIR", games.as_path()),
    ];
    let cmd = dir.path().join("prog");
    std::fs::write(&cmd, "#!/bin/sh\n").expect("write");
    assert!(Command::new("chmod")
        .arg("+x")
        .arg(&cmd)
        .status()
        .expect("chmod")
        .success());
    let cmd_s = cmd.to_string_lossy().into_owned();

    let add = run("register.sh", &envs, &["add", "tool", "Muse Test", &cmd_s]);
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let launch = std::fs::read_to_string(tools.join("Muse Test.pak/launch.sh")).expect("launch");
    assert!(launch.contains(&format!("exec \"{cmd_s}\"")), "{launch}");
    assert!(tools.join("Muse Test.pak/pak.json").is_file());

    let add = run("register.sh", &envs, &["add", "game", "Muse Game", &cmd_s]);
    assert!(add.status.success());
    assert!(games.join("Ports (PORTS)/Muse Game.sh").is_file());

    assert!(run("register.sh", &envs, &["remove", "tool", "Muse Test"])
        .status
        .success());
    assert!(!tools.join("Muse Test.pak").exists());
    assert!(run("register.sh", &envs, &["remove", "game", "Muse Game"])
        .status
        .success());
    assert!(!games.join("Ports (PORTS)/Muse Game.sh").exists());
}

#[test]
fn register_refuses_foreign_entries_and_unsafe_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tools = dir.path().join("tools");
    let envs = [
        ("GOBLIN_TOOLS_DIR", tools.as_path()),
        ("GOBLIN_GAMES_DIR", dir.path()),
    ];
    let cmd = dir.path().join("prog");
    std::fs::write(&cmd, "#!/bin/sh\n").expect("write");
    assert!(Command::new("chmod")
        .arg("+x")
        .arg(&cmd)
        .status()
        .expect("chmod")
        .success());
    let cmd_s = cmd.to_string_lossy().into_owned();

    let foreign = tools.join("Theirs.pak");
    std::fs::create_dir_all(&foreign).expect("mkdir");
    std::fs::write(foreign.join("launch.sh"), "#!/bin/sh\necho theirs\n").expect("write");
    assert!(
        !run("register.sh", &envs, &["add", "tool", "Theirs", &cmd_s])
            .status
            .success()
    );
    assert!(run("register.sh", &envs, &["remove", "tool", "Theirs"])
        .status
        .success());
    assert!(
        foreign.join("launch.sh").is_file(),
        "foreign entry must survive remove"
    );

    for bad in ["../x", ".hid", "a/b", "q\"uote", ""] {
        assert!(
            !run("register.sh", &envs, &["add", "tool", bad, &cmd_s])
                .status
                .success(),
            "{bad}"
        );
    }
    assert!(!run(
        "register.sh",
        &envs,
        &["add", "tool", "Ok", "relative/prog"]
    )
    .status
    .success());
    assert!(
        !run("register.sh", &envs, &["add", "tool", "Ok", "/bin/x$(id)"])
            .status
            .success()
    );
    assert!(!run(
        "register.sh",
        &envs,
        &["add", "tool", "Ok", "/does/not/exist"]
    )
    .status
    .success());
}

#[test]
fn hooks_without_hardware_exit_unsupported() {
    // On a development host none of the Brick sysfs files exist.
    if Path::new("/sys/class/motor/voltage").exists() {
        return;
    }
    assert_eq!(run("rumble.sh", &[], &["50", "50"]).status.code(), Some(2));
    assert_eq!(run("stay-awake.sh", &[], &["on"]).status.code(), Some(2));
    assert_eq!(run("output.sh", &[], &["mute-get"]).status.code(), Some(2));
}
