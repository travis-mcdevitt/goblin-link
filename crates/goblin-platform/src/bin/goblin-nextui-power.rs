//! Foreground Pak supervisor. NextUI owns suspend and shutdown; this owns only
//! the launched process group and the power key while that group is on screen.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
const HOLD: Duration = Duration::from_secs(2);

#[derive(Default)]
struct PowerKey {
    pressed: Option<Instant>,
}
#[derive(Debug, PartialEq)]
enum Action {
    Sleep,
    Shutdown,
}
impl PowerKey {
    fn event(&mut self, value: i32, now: Instant) -> Option<Action> {
        match value {
            1 if self.pressed.is_none() => self.pressed = Some(now),
            0 => {
                return self.pressed.take().map(|at| {
                    if now.duration_since(at) >= HOLD {
                        Action::Shutdown
                    } else {
                        Action::Sleep
                    }
                })
            }
            _ => {}
        }
        None
    }
    fn tick(&mut self, now: Instant) -> Option<Action> {
        if self
            .pressed
            .is_some_and(|at| now.duration_since(at) >= HOLD)
        {
            self.pressed = None;
            Some(Action::Shutdown)
        } else {
            None
        }
    }
}

struct App(Child);
impl App {
    fn signal(&self, signal: i32) {
        // Only a child we spawned in its own group can reach this call. Never
        // signal the launcher's group (which includes NextUI and its services).
        if let Ok(pid) = i32::try_from(self.0.id()) {
            if pid > 1 {
                unsafe {
                    libc::kill(-pid, signal);
                }
            }
        }
    }
    fn suspend(&self, suspend: &Path, settings: &Path) {
        self.signal(libc::SIGSTOP);
        brightness(0);
        let result = Command::new(suspend).status();
        // Brick needs a nonzero raw brightness before restoring saved settings.
        brightness(8);
        if let Err(error) = Command::new(settings).status() {
            eprintln!("restore settings: {error}");
        }
        self.signal(libc::SIGCONT);
        eprintln!("NextUI suspend returned: {result:?}");
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.signal(libc::SIGCONT);
        self.signal(libc::SIGTERM);
        std::thread::sleep(Duration::from_millis(150));
        self.signal(libc::SIGKILL);
        let _ = self.0.wait();
    }
}

fn brightness(raw: u64) {
    if let Ok(display) = OpenOptions::new().read(true).write(true).open("/dev/disp") {
        let args = [0u64, raw, 0, 0];
        // Allwinner DISP_LCD_SET_BRIGHTNESS, the same ABI used by NextUI.
        if unsafe { libc::ioctl(display.as_raw_fd(), 0x102, args.as_ptr()) } < 0 {
            eprintln!("backlight: {}", io::Error::last_os_error());
        }
    }
}

fn power_input() -> io::Result<File> {
    // Event numbers depend on enumeration order; match the actual power driver.
    for entry in fs::read_dir("/sys/class/input")? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("event")
            && fs::read_to_string(entry.path().join("device/name"))
                .is_ok_and(|s| s.trim() == "axp2202-pek")
        {
            return OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(Path::new("/dev/input").join(name));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "NextUI power input not found",
    ))
}

fn event(input: &mut File) -> io::Result<Option<(u16, u16, i32)>> {
    // Linux input_event has two native longs followed by type, code and value.
    let mut bytes = [0u8; 2 * std::mem::size_of::<libc::c_long>() + 8];
    match input.read(&mut bytes) {
        Ok(n) if n == bytes.len() => {
            let p = bytes.len() - 8;
            Ok(Some((
                u16::from_ne_bytes([bytes[p], bytes[p + 1]]),
                u16::from_ne_bytes([bytes[p + 2], bytes[p + 3]]),
                i32::from_ne_bytes([bytes[p + 4], bytes[p + 5], bytes[p + 6], bytes[p + 7]]),
            )))
        }
        Err(e)
            if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
        _ => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "incomplete power event",
        )),
    }
}

fn supervise(
    app: &mut App,
    mut input: Option<File>,
    system: &Path,
    poweroff: &Path,
) -> io::Result<i32> {
    let mut key = PowerKey::default();
    let mut ignore_until = Instant::now();
    loop {
        if STOP.load(Ordering::Relaxed) {
            return Ok(143);
        }
        // Leave the leader unreaped until cleanup has signalled its group, so
        // its PID cannot be reused for an unrelated process in between.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::waitid(
                libc::P_PID,
                app.0.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        if unsafe { info.si_pid() } != 0 {
            let code = unsafe { info.si_status() };
            return Ok(if info.si_code == libc::CLD_EXITED {
                code
            } else {
                128 + code
            });
        }
        let now = Instant::now();
        let mut action = None;
        let mut input_failed = false;
        if let Some(device) = input.as_mut() {
            // Bound each drain so a faulty input cannot starve exit handling.
            for _ in 0..128 {
                match event(device) {
                    Err(error) => {
                        eprintln!("Power input lost; continuing without power control: {error}");
                        input_failed = true;
                        break;
                    }
                    Ok(Some((1, 116, value))) if now >= ignore_until => {
                        action = key.event(value, now);
                        if action.is_some() {
                            break;
                        }
                    }
                    Ok(Some((0, 3, _))) => key = PowerKey::default(), // SYN_DROPPED
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                }
            }
        }
        if input_failed {
            input = None;
            key = PowerKey::default();
        }
        match action.or_else(|| key.tick(now)) {
            Some(Action::Sleep) => {
                app.suspend(
                    &system.join("bin/suspend"),
                    &system.join("bin/syncsettings.elf"),
                );
                key = PowerKey::default();
                ignore_until = Instant::now() + Duration::from_secs(1);
            }
            Some(Action::Shutdown) => {
                // NextUI checks this after the foreground launcher returns.
                let request = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(poweroff);
                match request {
                    Ok(_) => return Ok(0),
                    Err(error) => eprintln!("Could not request shutdown: {error}"),
                }
            }
            None => {}
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn lock_power(path: &Path) -> io::Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::other("another Goblin Pak owns the power button"));
    }
    Ok(lock)
}

fn run() -> io::Result<i32> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let Some(program) = args.first() else {
        return Err(io::Error::other(
            "usage: goblin-nextui-power <program> [args...]",
        ));
    };
    let system = PathBuf::from(
        std::env::var_os("SYSTEM_PATH").unwrap_or_else(|| "/mnt/SDCARD/.system/tg5040".into()),
    );
    let input = if system.join("bin/suspend").is_file() {
        match power_input() {
            Ok(input) => Some(input),
            Err(error) => {
                eprintln!("Power button unavailable: {error}");
                None
            }
        }
    } else {
        eprintln!("NextUI suspend routine unavailable");
        None
    };
    // Advisory lock stays open until the app and its children have exited. The
    // inode is never removed: unlinking would let another launcher bypass it.
    let _lock = if input.is_some() {
        fs::create_dir_all("/run/goblin-link")?;
        let lock = lock_power(Path::new("/run/goblin-link/power.lock"))?;
        Some(lock)
    } else {
        None
    };
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe {
            libc::signal(sig, stop as *const () as libc::sighandler_t);
        }
    }
    let mut app = App(Command::new(program)
        .args(&args[1..])
        .env("SYSTEM_PATH", &system)
        .process_group(0)
        .spawn()?);
    supervise(&mut app, input, &system, Path::new("/tmp/poweroff"))
}
fn main() {
    std::process::exit(match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("power launcher: {error}");
            1
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_hold_repeat_and_unmatched_release() {
        let now = Instant::now();
        let mut key = PowerKey::default();
        assert_eq!(key.event(0, now), None);
        assert_eq!(key.event(1, now), None);
        assert_eq!(key.event(2, now), None);
        assert_eq!(
            key.event(0, now + Duration::from_millis(100)),
            Some(Action::Sleep)
        );
        key.event(1, now);
        assert_eq!(key.tick(now + HOLD), Some(Action::Shutdown));
        assert_eq!(key.event(0, now + HOLD), None);
        key.event(1, now);
        assert_eq!(key.event(0, now + HOLD), Some(Action::Shutdown));
    }
    #[test]
    fn launcher_preserves_exit_and_cleans_its_group() {
        let dir = tempfile::tempdir().unwrap();
        let child_pid = dir.path().join("child");
        let mut app = App(Command::new("sh")
            .arg("-c")
            .arg("sleep 60 & echo $! > \"$1\"; exit 7")
            .arg("sh")
            .arg(&child_pid)
            .process_group(0)
            .spawn()
            .unwrap());
        assert_eq!(
            supervise(&mut app, None, dir.path(), &dir.path().join("off")).unwrap(),
            7
        );
        let pid: i32 = fs::read_to_string(child_pid)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        drop(app);
        // A killed orphan may remain as a zombie until the container init reaps it.
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        assert!(
            stat.is_empty() || stat.split_once(") ").unwrap().1.starts_with('Z'),
            "{stat}"
        );
        assert!(!dir.path().join("off").exists());
    }
    #[test]
    fn failed_suspend_still_resumes_child() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("resumed");
        let app = App(Command::new("sh")
            .arg("-c")
            .arg("sleep .2; echo yes > \"$1\"; sleep 60")
            .arg("sh")
            .arg(&marker)
            .process_group(0)
            .spawn()
            .unwrap());
        app.suspend(
            Path::new("/missing/suspend"),
            Path::new("/missing/settings"),
        );
        for _ in 0..50 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(marker.exists());
    }
    fn input_events(values: &[i32]) -> (File, std::os::unix::net::UnixStream) {
        use std::{
            io::Write,
            os::{fd::OwnedFd, unix::net::UnixStream},
        };
        let (input, mut writer) = UnixStream::pair().unwrap();
        input.set_nonblocking(true).unwrap();
        for value in values {
            let mut bytes = vec![0u8; 2 * std::mem::size_of::<libc::c_long>()];
            bytes.extend(1u16.to_ne_bytes());
            bytes.extend(116u16.to_ne_bytes());
            bytes.extend(value.to_ne_bytes());
            writer.write_all(&bytes).unwrap();
        }
        (File::from(OwnedFd::from(input)), writer)
    }
    #[test]
    fn wake_events_are_drained_and_failed_sleep_restores_settings() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        // The app here is one process that starts no other, and runs until the suspend
        // below has been through (it leaves `paused`), not for a fixed time.
        // - No child: the power press is already waiting, so the stop arrives while the app
        //   is still starting. A shell stopped while it starts a child (`sleep`) sits
        //   waiting on that child, and shows as busy in the kernel ("D"), not as stopped.
        // - No fixed time: on a busy machine a short-lived app can be over before the stop.
        // The count only ends it if the suspend never comes.
        let mut app = App(Command::new("sh")
            .args([
                "-c",
                "i=0; while [ ! -e \"$1\" ] && [ $i -lt 20000000 ]; do i=$((i+1)); done; exit 9",
                "sh",
            ])
            .arg(dir.path().join("paused"))
            .process_group(0)
            .spawn()
            .unwrap());
        // The stand-in for NextUI's suspend waits until the app shows as stopped: on a busy
        // machine that can be a while after the signal, so it waits as long as it takes
        // (half a minute at most), not a fixed moment.
        for (name, script) in [
            (
                "suspend",
                format!(
                    "#!/bin/sh\nfor i in $(seq 1 3000); do\n state=$(ps -o stat= -p {})\n case \"$state\" in *T*) break;; esac\n sleep .01\ndone\nprintf '%s\\n' \"$state\" >> '{}/paused'\nexit 1\n",
                    app.0.id(),
                    dir.path().display()
                ),
            ),
            (
                "syncsettings.elf",
                format!(
                    "#!/bin/sh\necho yes >> '{}/settings'\n",
                    dir.path().display()
                ),
            ),
        ] {
            let path = bin.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        // The second pair represents the wake press already queued by evdev.
        let (input, _writer) = input_events(&[1, 0, 1, 0]);
        assert_eq!(
            supervise(&mut app, Some(input), dir.path(), &dir.path().join("off")).unwrap(),
            9
        );
        let paused = fs::read_to_string(dir.path().join("paused")).unwrap();
        assert!(paused.contains('T'), "the app's state was {paused:?}");
        assert_eq!(
            fs::read_to_string(dir.path().join("settings")).unwrap(),
            "yes\n"
        );
    }
    #[test]
    fn held_key_requests_firmware_shutdown_and_releases_group() {
        let dir = tempfile::tempdir().unwrap();
        let off = dir.path().join("off");
        let mut app = App(Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap());
        let pid = app.0.id();
        let (input, _writer) = input_events(&[1]);
        assert_eq!(
            supervise(&mut app, Some(input), dir.path(), &off).unwrap(),
            0
        );
        assert!(off.exists());
        drop(app);
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
    #[test]
    fn power_lock_excludes_duplicates_and_releases_on_exit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let first = lock_power(&path).unwrap();
        assert!(lock_power(&path).is_err());
        drop(first);
        // The other tests here start processes. One forked while `first` was held carries a
        // copy of it until it execs, and the lock lasts as long as any copy does: so the
        // lock is free a moment after the drop, not always at once.
        let mut again = lock_power(&path);
        for _ in 0..500 {
            if again.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            again = lock_power(&path);
        }
        assert!(again.is_ok(), "{again:?}");
    }
    #[test]
    fn lost_power_input_does_not_close_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App(Command::new("sh")
            .args(["-c", "sleep .1; exit 7"])
            .process_group(0)
            .spawn()
            .unwrap());
        let (input, writer) = input_events(&[]);
        drop(writer);
        assert_eq!(
            supervise(&mut app, Some(input), dir.path(), &dir.path().join("off")).unwrap(),
            7
        );
    }
}
