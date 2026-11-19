//! Runs the real Brick hook scripts that only touch ordinary files (autostart and
//! registration) against temporary directories. The auto.sh tests cover NextUI before
//! 6.11.0 (no boot hooks); the boot hook tests cover 6.11.0 and later.

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
        ("GOBLIN_HOOK_RUNNER", missing.as_path()),
        ("GOBLIN_BOOT_HOOK", missing.as_path()),
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
    let no_runner = dir.path().join("no-run_hooks.sh");
    let hook_file = dir.path().join("boot.d/goblin-link.sh");
    let envs = [
        ("GOBLIN_AUTO_SH", auto.as_path()),
        ("GOBLIN_BOOT_SH", boot.as_path()),
        ("GOBLIN_HOOK_RUNNER", no_runner.as_path()),
        ("GOBLIN_BOOT_HOOK", hook_file.as_path()),
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
    let no_runner = dir.path().join("no-run_hooks.sh");
    let hook_file = dir.path().join("boot.d/goblin-link.sh");
    let envs = [
        ("GOBLIN_AUTO_SH", auto.as_path()),
        ("GOBLIN_BOOT_SH", boot.as_path()),
        ("GOBLIN_HOOK_RUNNER", no_runner.as_path()),
        ("GOBLIN_BOOT_HOOK", hook_file.as_path()),
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
    let no_runner = dir.path().join("no-run_hooks.sh");
    let hook_file = dir.path().join("boot.d/goblin-link.sh");
    let envs = [
        ("GOBLIN_AUTO_SH", auto.as_path()),
        ("GOBLIN_BOOT_SH", boot.as_path()),
        ("GOBLIN_HOOK_RUNNER", no_runner.as_path()),
        ("GOBLIN_BOOT_HOOK", hook_file.as_path()),
    ];
    assert!(run("autostart.sh", &envs, &["enable"]).status.success());
    assert!(std::fs::read_to_string(&auto)
        .expect("read")
        .starts_with("#!/bin/sh"));
    assert!(!run("autostart.sh", &envs, &["bogus"]).status.success());
}

/// A NextUI with boot hooks: an auto.sh holding someone else's line, a stand-in for
/// `run_hooks.sh`, and where our hook goes.
struct BootHooks {
    dir: tempfile::TempDir,
    auto: PathBuf,
    runner: PathBuf,
    hook: PathBuf,
    boot: PathBuf,
}

const OTHER_AUTO: &str = "#!/bin/sh\n\ntest -f x && x # Other.pak-on-boot\n";

impl BootHooks {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let auto = dir.path().join("auto.sh");
        std::fs::write(&auto, OTHER_AUTO).expect("write");
        let runner = dir.path().join("run_hooks.sh");
        std::fs::write(&runner, "#!/bin/sh\n").expect("write");
        let hook = dir.path().join(".hooks/boot.d/goblin-link.sh");
        let boot = boot_script(dir.path());
        BootHooks {
            dir,
            auto,
            runner,
            hook,
            boot,
        }
    }

    fn run(&self, arg: &str) -> Output {
        run(
            "autostart.sh",
            &[
                ("GOBLIN_AUTO_SH", self.auto.as_path()),
                ("GOBLIN_BOOT_SH", self.boot.as_path()),
                ("GOBLIN_HOOK_RUNNER", self.runner.as_path()),
                ("GOBLIN_BOOT_HOOK", self.hook.as_path()),
            ],
            &[arg],
        )
    }

    fn ok(&self, arg: &str) {
        let out = self.run(arg);
        assert!(
            out.status.success(),
            "{arg}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn status(&self) -> String {
        stdout(&self.run("status"))
    }

    fn auto_text(&self) -> String {
        std::fs::read_to_string(&self.auto).expect("read auto.sh")
    }

    /// Adds the auto.sh line an earlier version wrote, as that version did it.
    fn old_auto_line(&self) {
        let missing = self.dir.path().join("missing");
        let out = run(
            "autostart.sh",
            &[
                ("GOBLIN_AUTO_SH", self.auto.as_path()),
                ("GOBLIN_BOOT_SH", self.boot.as_path()),
                ("GOBLIN_HOOK_RUNNER", missing.as_path()),
                ("GOBLIN_BOOT_HOOK", missing.as_path()),
            ],
            &["enable"],
        );
        assert!(out.status.success());
        assert!(self.auto_text().contains("goblin-link-on-boot"));
    }
}

/// Runs the hook the way NextUI's run_hooks.sh does: in a background subshell with output
/// dropped, then waits for it. Returns how long the wait took.
fn run_like_nextui(hook: &Path) -> std::time::Duration {
    let start = std::time::Instant::now();
    let status = Command::new("sh")
        .args(["-c", "( \"$1\" ) >/dev/null 2>&1 & wait", "run_hooks"])
        .arg(hook)
        .status()
        .expect("sh runs");
    assert!(status.success());
    start.elapsed()
}

#[test]
fn boot_hook_install_remove_and_idempotence() {
    use std::os::unix::fs::PermissionsExt;
    let t = BootHooks::new();
    assert_eq!(t.status(), "disabled");
    t.ok("enable");
    assert_eq!(t.status(), "enabled");
    let text = std::fs::read_to_string(&t.hook).expect("hook written");
    assert!(text.starts_with("#!/bin/sh\n"), "{text}");
    assert!(text.contains(&t.boot.display().to_string()), "{text}");
    let mode = std::fs::metadata(&t.hook)
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "NextUI runs the hook directly");
    // auto.sh is not ours to edit when boot hooks exist.
    assert_eq!(t.auto_text(), OTHER_AUTO);

    t.ok("enable");
    assert_eq!(std::fs::read_to_string(&t.hook).expect("read"), text);
    t.ok("migrate");
    assert_eq!(std::fs::read_to_string(&t.hook).expect("read"), text);

    t.ok("disable");
    assert_eq!(t.status(), "disabled");
    assert!(!t.hook.exists());
    t.ok("disable");
    assert_eq!(t.auto_text(), OTHER_AUTO);
}

#[test]
fn boot_hook_migrates_the_old_auto_sh_line() {
    // The first launch of this version (launch.sh runs "migrate").
    let t = BootHooks::new();
    t.old_auto_line();
    assert_eq!(t.status(), "enabled");
    t.ok("migrate");
    assert_eq!(t.status(), "enabled");
    assert!(t.hook.is_file());
    assert_eq!(t.auto_text(), OTHER_AUTO);

    // Turning the switch on moves the line too.
    let t = BootHooks::new();
    t.old_auto_line();
    t.ok("enable");
    assert!(t.hook.is_file());
    assert_eq!(t.auto_text(), OTHER_AUTO);

    // Turning it off removes the line and installs nothing.
    let t = BootHooks::new();
    t.old_auto_line();
    t.ok("disable");
    assert_eq!(t.status(), "disabled");
    assert!(!t.hook.exists());
    assert_eq!(t.auto_text(), OTHER_AUTO);

    // With start on boot off, migrating touches nothing.
    let t = BootHooks::new();
    t.ok("migrate");
    assert!(!t.hook.exists());
    assert!(!t.hook.parent().expect("dir").exists());
    assert_eq!(t.auto_text(), OTHER_AUTO);

    // NextUI before 6.11.0 keeps the auto.sh line.
    let t = BootHooks::new();
    t.old_auto_line();
    std::fs::remove_file(&t.runner).expect("rm");
    t.ok("migrate");
    assert!(!t.hook.exists());
    assert!(t.auto_text().contains("goblin-link-on-boot"));
}

#[test]
fn boot_hook_leaves_a_foreign_file_alone() {
    let t = BootHooks::new();
    std::fs::create_dir_all(t.hook.parent().expect("dir")).expect("mkdir");
    std::fs::write(&t.hook, "#!/bin/sh\necho theirs\n").expect("write");
    assert!(!t.run("enable").status.success());
    t.ok("disable");
    assert_eq!(
        std::fs::read_to_string(&t.hook).expect("read"),
        "#!/bin/sh\necho theirs\n"
    );
}

#[test]
fn boot_hook_detaches_and_does_nothing_once_the_pak_is_gone() {
    use std::os::unix::fs::PermissionsExt;
    let mut t = BootHooks::new();
    // A pak path with a space and a quote, which the hook must take literally.
    let pak = t.dir.path().join("Goblin Link's.pak");
    std::fs::create_dir_all(&pak).expect("mkdir");
    let ran = t.dir.path().join("ran");
    t.boot = pak.join("on-boot.sh");
    std::fs::write(
        &t.boot,
        format!(
            "#!/bin/sh\necho noise\nsleep 3\necho \"$0\" > '{}'\n",
            ran.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(&t.boot, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    t.ok("enable");

    // NextUI waits for boot hooks; ours must not hold up the menu for the daemon's start.
    let waited = run_like_nextui(&t.hook);
    assert!(waited < std::time::Duration::from_secs(2), "{waited:?}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !ran.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(
        std::fs::read_to_string(&ran)
            .expect("on-boot.sh ran")
            .trim(),
        t.boot.display().to_string()
    );

    // A store uninstall deletes the pak folder but not the hook.
    std::fs::remove_file(&ran).expect("rm");
    std::fs::remove_dir_all(&pak).expect("rm pak");
    let out = Command::new(&t.hook).output().expect("hook runs");
    assert!(out.status.success());
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!ran.exists());
}

/// An installed pak on a scratch SD card: the real on-boot.sh and autostart.sh, and a
/// stand-in start-daemon.sh that records each start. With `hooks`, NextUI runs boot hooks
/// (6.11.0 and later); without, only auto.sh.
struct Install {
    dir: tempfile::TempDir,
    pak: PathBuf,
    auto: PathBuf,
    runner: PathBuf,
    hook: PathBuf,
    starts: PathBuf,
    hooks: bool,
}

impl Install {
    fn new(hooks: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let auto = dir.path().join("auto.sh");
        std::fs::write(&auto, OTHER_AUTO).expect("write");
        let runner = dir.path().join("run_hooks.sh");
        if hooks {
            std::fs::write(&runner, "#!/bin/sh\n").expect("write");
        }
        let t = Install {
            pak: dir.path().join("Tools/Goblin Link.pak"),
            hook: dir.path().join(".hooks/boot.d/goblin-link.sh"),
            starts: dir.path().join("starts"),
            dir,
            auto,
            runner,
            hooks,
        };
        t.unpack();
        t
    }

    /// What the Pak Store does to install or update: write the pak's files over the folder,
    /// deleting nothing.
    fn unpack(&self) {
        use std::os::unix::fs::PermissionsExt;
        let hooks = self.pak.join("platforms/tg5040-brick-nextui/hooks");
        std::fs::create_dir_all(&hooks).expect("mkdir");
        let exe = std::fs::Permissions::from_mode(0o755);
        let on_boot = self.pak.join("on-boot.sh");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/nextui-pak/on-boot.sh"),
            &on_boot,
        )
        .expect("copy on-boot.sh");
        std::fs::set_permissions(&on_boot, exe.clone()).expect("chmod");
        let autostart = hooks.join("autostart.sh");
        std::fs::copy(hook("autostart.sh"), &autostart).expect("copy autostart.sh");
        std::fs::set_permissions(&autostart, exe).expect("chmod");
        std::fs::write(
            self.pak.join("start-daemon.sh"),
            format!(
                "LOGS='{}'\nPIDFILE=/nonexistent\nrunning() {{ true; }}\necho start >> '{}'\n",
                self.dir.path().display(),
                self.starts.display()
            ),
        )
        .expect("write start-daemon.sh");
    }

    /// What the Pak Store does to uninstall: delete the pak folder, and only that.
    fn uninstall(&self) {
        std::fs::remove_dir_all(&self.pak).expect("rm pak");
    }

    /// Runs the installed pak's autostart.sh, as the Settings switch and launch.sh do.
    fn autostart(&self, arg: &str) -> String {
        let out = Command::new("sh")
            .arg(
                self.pak
                    .join("platforms/tg5040-brick-nextui/hooks/autostart.sh"),
            )
            .arg(arg)
            .env("GOBLIN_AUTO_SH", &self.auto)
            .env("GOBLIN_HOOK_RUNNER", &self.runner)
            .env("GOBLIN_BOOT_HOOK", &self.hook)
            .env_remove("GOBLIN_BOOT_SH")
            .output()
            .expect("sh runs");
        assert!(
            out.status.success(),
            "{arg}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout(&out)
    }

    /// Starts the handheld: NextUI runs our boot hook, or auto.sh before 6.11.0. Returns
    /// whether the daemon was started.
    fn boot(&self) -> bool {
        let _ = std::fs::remove_file(&self.starts);
        if self.hooks {
            if self.hook.exists() {
                run_like_nextui(&self.hook);
            }
        } else {
            // Its status is the last line's, someone else's; only what ran matters.
            Command::new("sh")
                .arg(&self.auto)
                .status()
                .expect("sh runs");
        }
        // on-boot.sh runs in the background either way.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
        while !self.starts.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        self.starts.exists()
    }

    fn auto_text(&self) -> String {
        std::fs::read_to_string(&self.auto).expect("read auto.sh")
    }

    /// Whether a hook or auto.sh line of ours is left on the card.
    fn boot_entry(&self) -> bool {
        self.hook.exists() || self.auto_text().contains("goblin-link-on-boot")
    }
}

#[test]
fn start_on_boot_belongs_to_one_install() {
    for hooks in [true, false] {
        let t = Install::new(hooks);
        assert_eq!(t.autostart("status"), "disabled", "hooks {hooks}");
        assert!(!t.boot(), "hooks {hooks}: off by default");

        t.autostart("enable");
        assert_eq!(t.autostart("status"), "enabled", "hooks {hooks}");
        assert!(t.boot(), "hooks {hooks}: enabled");

        // An update in place keeps the choice, through first launch too.
        t.unpack();
        t.autostart("migrate");
        assert_eq!(t.autostart("status"), "enabled", "hooks {hooks}");
        assert!(t.boot(), "hooks {hooks}: updated");

        // Uninstall and reinstall to the same path, then boot before opening the app.
        t.uninstall();
        t.unpack();
        assert!(
            t.boot_entry(),
            "hooks {hooks}: the uninstall left the entry"
        );
        assert_eq!(t.autostart("status"), "disabled", "hooks {hooks}");
        assert!(!t.boot(), "hooks {hooks}: reinstalled");
        // Opening the app (launch.sh runs migrate) clears the leftover.
        t.autostart("migrate");
        assert!(!t.boot_entry(), "hooks {hooks}");
        assert_eq!(t.auto_text(), OTHER_AUTO, "hooks {hooks}");

        // Turning it on again works, and turning it off stops the next start.
        t.autostart("enable");
        assert!(t.boot(), "hooks {hooks}: enabled again");
        t.autostart("disable");
        assert_eq!(t.autostart("status"), "disabled", "hooks {hooks}");
        assert!(!t.boot(), "hooks {hooks}: disabled");
        assert_eq!(t.auto_text(), OTHER_AUTO, "hooks {hooks}");
    }
}

/// An opt-in made before the consent file existed cannot be told apart from one left by an
/// uninstalled copy, so it counts as off until the switch is turned on again.
#[test]
fn start_on_boot_from_before_the_consent_file_needs_turning_on_again() {
    for hooks in [true, false] {
        let t = Install::new(hooks);
        t.autostart("enable");
        std::fs::remove_file(t.pak.join(".start-on-boot")).expect("rm consent");
        assert_eq!(t.autostart("status"), "disabled", "hooks {hooks}");
        assert!(!t.boot(), "hooks {hooks}");
        t.autostart("migrate");
        assert!(!t.boot_entry(), "hooks {hooks}");
        t.autostart("enable");
        assert!(t.boot(), "hooks {hooks}");
    }
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
    assert!(launch.contains(&cmd_s), "{launch}");
    assert!(tools.join("Muse Test.pak/pak.json").is_file());

    let add = run("register.sh", &envs, &["add", "game", "Muse Game", &cmd_s]);
    assert!(add.status.success());
    assert!(tools.join("Muse Game.pak/launch.sh").is_file());
    assert!(!games.join("Ports (PORTS)/Muse Game.sh").exists());

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

#[test]
fn standalone_game_launches_without_ports_and_captures_bounded_output() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let tools = dir.path().join("tools");
    let games = dir.path().join("games");
    let bundle = dir.path().join("installed copy");
    std::fs::create_dir_all(&bundle).unwrap();
    let cmd = bundle.join("start.sh");
    std::fs::write(
        &cmd,
        "#!/bin/sh\ncat relative.txt\necho diagnostic >&2\nhead -c 100000 /dev/zero\nexit 7\n",
    )
    .unwrap();
    std::fs::write(bundle.join("relative.txt"), "installed copy\n").unwrap();
    std::fs::set_permissions(&cmd, std::fs::Permissions::from_mode(0o755)).unwrap();
    let envs = [
        ("GOBLIN_TOOLS_DIR", tools.as_path()),
        ("GOBLIN_GAMES_DIR", games.as_path()),
    ];
    let legacy = games.join("Ports (PORTS)/Tetris.sh");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "#!/bin/sh\n# goblin-link-entry\n").unwrap();
    assert!(run(
        "register.sh",
        &envs,
        &["add", "game", "Tetris", cmd.to_str().unwrap()]
    )
    .status
    .success());
    assert!(!legacy.exists());
    let launch = tools.join("Tetris.pak/launch.sh");
    assert!(Command::new("sh")
        .arg(&launch)
        .current_dir("/")
        .status()
        .unwrap()
        .success());
    let output = std::fs::read(bundle.join(".goblin-launch.log")).unwrap();
    assert_eq!(output.len(), 65536);
    assert!(output.starts_with(b"installed copy\ndiagnostic\n"));
    assert_eq!(
        std::fs::read_to_string(bundle.join(".goblin-launch.status")).unwrap(),
        "7\n"
    );
    // A second launch replaces the log rather than appending indefinitely.
    std::fs::write(&cmd, "#!/bin/sh\necho repaired\n").unwrap();
    assert!(Command::new("sh").arg(launch).status().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(bundle.join(".goblin-launch.log")).unwrap(),
        "repaired\n"
    );
    std::fs::write(&legacy, "#!/bin/sh\necho unrelated\n").unwrap();
    assert!(run("register.sh", &envs, &["remove", "game", "Tetris"])
        .status
        .success());
    assert!(legacy.exists(), "never remove a foreign Ports entry");
    assert!(!tools.join("Tetris.pak").exists());
}

#[test]
fn game_registration_does_not_replace_or_remove_an_existing_app() {
    let dir = tempfile::tempdir().unwrap();
    let tools = dir.path().join("tools");
    let envs = [
        ("GOBLIN_TOOLS_DIR", tools.as_path()),
        ("GOBLIN_GAMES_DIR", dir.path()),
    ];
    let app = boot_script(dir.path());
    assert!(run(
        "register.sh",
        &envs,
        &["add", "tool", "Same", app.to_str().unwrap()]
    )
    .status
    .success());
    let other = dir.path().join("other.sh");
    std::fs::copy(&app, &other).unwrap();
    assert!(!run(
        "register.sh",
        &envs,
        &["add", "game", "Same", other.to_str().unwrap()]
    )
    .status
    .success());
    assert!(run("register.sh", &envs, &["remove", "game", "Same"])
        .status
        .success());
    assert!(tools.join("Same.pak/launch.sh").exists());
    // The same build can change kind without deleting its replacement entry.
    assert!(run(
        "register.sh",
        &envs,
        &["add", "game", "Same", app.to_str().unwrap()]
    )
    .status
    .success());
    assert!(run("register.sh", &envs, &["remove", "tool", "Same"])
        .status
        .success());
    assert!(tools.join("Same.pak/launch.sh").exists());
}

#[test]
fn registered_game_uses_packaged_power_supervisor() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let pak = dir.path().join("Goblin Link.pak");
    let hooks = pak.join("platforms/tg5040-brick-nextui/hooks");
    let bin = pak.join("bin");
    let tools = dir.path().join("tools");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(hook("register.sh"), hooks.join("register.sh")).unwrap();
    let power = bin.join("goblin-nextui-power");
    std::fs::write(
        &power,
        "#!/bin/sh\necho supervised > \"$0.used\"\nexec \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&power, std::fs::Permissions::from_mode(0o755)).unwrap();
    let app = boot_script(dir.path());
    assert!(Command::new("sh")
        .arg(hooks.join("register.sh"))
        .args(["add", "game", "Test"])
        .arg(app)
        .env("GOBLIN_TOOLS_DIR", &tools)
        .env("GOBLIN_GAMES_DIR", dir.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new("sh")
        .arg(tools.join("Test.pak/launch.sh"))
        .status()
        .unwrap()
        .success());
    assert_eq!(
        std::fs::read_to_string(bin.join("goblin-nextui-power.used")).unwrap(),
        "supervised\n"
    );
}
