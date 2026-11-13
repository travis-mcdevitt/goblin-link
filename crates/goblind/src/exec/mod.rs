//! The commands a Muse can invoke on this device (port of `executor.py`).
//!
//! `system.run`, the owner's drop-in commands and file operations run with the
//! configured unprivileged `run_as` account when the daemon is root. Output is
//! capped at 96 KiB per stream, a timed-out command's whole process group is
//! killed, and file writes are atomic with an optional SHA-256 check.
//!
//! Consent is enforced here via [`command_allowed`]: a disabled command returns
//! a clear refusal and is never executed, though it stays registered so the
//! Muse can tell the owner why.

pub mod dropins;
pub mod fileops;
pub mod scope;

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use goblin_ipc::types::{Consent, FileScope};
use serde_json::{json, Map, Value};

use dropins::DropInCommand;

const DEFAULT_TIMEOUT_S: u64 = 120;
const MAX_TIMEOUT_S: u64 = 600;
const KILL_GRACE: Duration = Duration::from_secs(2);
const MAX_OUTPUT_BYTES: usize = 96 * 1024;
/// The upstream SDK runs commands with bash. Handheld firmwares often ship only BusyBox, so
/// fall back to POSIX `sh` there and say so in the command description.
fn shell_path() -> &'static str {
    if std::path::Path::new("/bin/bash").exists() {
        "/bin/bash"
    } else {
        "/bin/sh"
    }
}

fn shell_name() -> &'static str {
    if shell_path() == "/bin/bash" {
        "bash"
    } else {
        "POSIX sh (bash is not installed on this device)"
    }
}

const SAFE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The account a command runs as (resolved from `/etc/passwd`).
#[derive(Debug, Clone)]
pub struct RunAs {
    /// Account name.
    pub name: String,
    /// User id.
    pub uid: u32,
    /// Group id.
    pub gid: u32,
    /// Home directory.
    pub home: String,
}

impl RunAs {
    /// Looks up an account by name in `/etc/passwd`.
    pub fn lookup(name: &str) -> Option<RunAs> {
        let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
        for line in passwd.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() >= 6 && f[0] == name {
                return Some(RunAs {
                    name: name.to_owned(),
                    uid: f[2].parse().ok()?,
                    gid: f[3].parse().ok()?,
                    home: f[5].to_owned(),
                });
            }
        }
        None
    }

    /// The current account.
    pub fn current() -> RunAs {
        RunAs {
            name: std::env::var("USER").unwrap_or_else(|_| "user".into()),
            uid: current_uid(),
            gid: current_uid(),
            home: std::env::var("HOME").unwrap_or_else(|_| "/".into()),
        }
    }
}

fn current_uid() -> u32 {
    // Avoid a libc dependency: read the real uid from /proc/self/status.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|v| v.split_whitespace().next())
                .and_then(|u| u.parse().ok())
        })
        .unwrap_or(1000)
}

fn is_root() -> bool {
    current_uid() == 0
}

/// Whether a command is permitted by the consent toggles.
pub fn command_allowed(command: &str, consent: &Consent, is_drop_in: bool) -> bool {
    match command {
        "system.run" => consent.shell,
        "file.read" | "file.list" => consent.file_read,
        "file.write" => consent.file_write,
        "device.health" | "handheld.about" => true,
        "build.install" | "build.list" | "build.remove" => {
            consent.builds != goblin_ipc::types::BuildAccess::Off
        }
        _ => !is_drop_in || consent.drop_ins,
    }
}

/// A clear refusal result for a command turned off by consent.
pub fn refusal(command: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("ok".into(), json!(false));
    m.insert(
        "error".into(),
        json!(format!("{command} is turned off on this device")),
    );
    m
}

fn ok(payload: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("ok".into(), json!(true));
    m.insert("payload".into(), payload);
    m
}

fn err(message: impl Into<String>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("ok".into(), json!(false));
    m.insert("error".into(), json!(message.into()));
    m
}

/// Produces `device.health` payloads.
pub type HealthFn = Arc<dyn Fn() -> Value + Send + Sync>;

/// Runs commands for the Muse.
pub struct Executor {
    account: RunAs,
    drop_ins: Vec<DropInCommand>,
    health: HealthFn,
    /// Where the file commands may reach; `None` leaves them unrestricted (tests).
    rules: Option<scope::FileRules>,
    /// The tools this profile's Muse built and installed.
    built: std::sync::RwLock<Vec<DropInCommand>>,
}

impl Executor {
    /// Builds an executor running as `account`, with `drop_ins` and a health source.
    pub fn new(account: RunAs, drop_ins: Vec<DropInCommand>, health: HealthFn) -> Self {
        Self {
            account,
            drop_ins,
            health,
            rules: None,
            built: std::sync::RwLock::new(Vec::new()),
        }
    }

    /// Replaces the tools Muse built. One that shares a name with a command the owner
    /// wrote is left out: the owner's stands.
    pub fn set_built(&self, tools: Vec<DropInCommand>) {
        let tools = tools
            .into_iter()
            .filter(|t| !self.is_drop_in(&t.name))
            .collect();
        *self.built.write().unwrap() = tools;
    }

    /// The removable storage, where the device has one.
    pub fn storage_root(&self) -> Option<std::path::PathBuf> {
        self.rules.as_ref().and_then(|r| r.storage.clone())
    }

    /// Whether `command` is a tool Muse built.
    pub fn is_built(&self, command: &str) -> bool {
        self.built.read().unwrap().iter().any(|c| c.name == command)
    }

    /// The real path of a folder Muse names, if it is inside the profile's own folder.
    pub fn in_home(&self, path: &str) -> Result<std::path::PathBuf, String> {
        match &self.rules {
            Some(rules) => rules.check(std::path::Path::new(path), FileScope::Home),
            None => Ok(std::path::PathBuf::from(path)),
        }
    }

    /// Holds the file commands to `rules`, and starts the shell in the profile's folder.
    pub fn with_rules(mut self, rules: scope::FileRules) -> Self {
        self.account.home = rules.home.to_string_lossy().into_owned();
        self.rules = Some(rules);
        self
    }

    /// The `commands_v2` map for `link.register`: the built-ins plus drop-ins.
    pub fn specs(&self) -> Value {
        let mut map = Map::new();
        map.insert("system.run".into(), json!({
            "description": format!("Run a shell command on this Linux device with {} and return its stdout, stderr and exit code. Output over 96 KiB per stream is truncated.", shell_name()),
            "required": {"command": {"type": "string", "description": "Shell command line to run."}},
            "optional": {
                "cwd": {"type": "string", "description": "Working directory. Default: the account's home."},
                "timeout_ms": {"type": "integer", "description": "Kill the command after this long. Default 120000, max 600000."}
            },
            "timeout_ms": MAX_TIMEOUT_S * 1000 + 5000
        }));
        map.insert("file.read".into(), json!({
            "description": "Read up to 64 KiB of a file, base64-encoded. Call again with next_offset until eof.",
            "required": {"path": {"type": "string", "description": "Absolute path."}},
            "optional": {
                "offset": {"type": "integer", "description": "Byte offset. Default 0."},
                "limit": {"type": "integer", "description": "Max bytes, up to 65536."}
            }
        }));
        map.insert("file.write".into(), json!({
            "description": "Write a file in chunks of up to 64 KiB (base64). Start at offset 0, send chunks in order, and set final=true on the last one; the file is replaced atomically, and only if sha256 (when given) matches.",
            "required": {
                "path": {"type": "string", "description": "Absolute destination path."},
                "data_b64": {"type": "string", "description": "This chunk's bytes, base64-encoded."}
            },
            "optional": {
                "offset": {"type": "integer", "description": "Byte offset of this chunk. Default 0."},
                "final": {"type": "boolean", "description": "True on the last chunk. Default false."},
                "sha256": {"type": "string", "description": "Hex SHA-256 of the whole file, checked on the final chunk."},
                "create_parents": {"type": "boolean", "description": "Create missing parent directories. Default false."},
                "overwrite": {"type": "boolean", "description": "Replace an existing file. Default true."}
            }
        }));
        map.insert("device.health".into(), json!({
            "description": "Device status: uptime, load, memory, disk, temperature, software version.",
            "required": {},
            "optional": {}
        }));
        map.insert("file.list".into(), json!({
            "description": "List a folder: each entry's name, whether it is a file, a folder or a link, and a file's size. Up to 500 entries. Reaches as far as file.read does.",
            "required": {"path": {"type": "string", "description": "Absolute path of the folder."}},
            "optional": {}
        }));
        map.insert("handheld.about".into(), json!({
            "description": "Ask the handheld itself what it is and what you may do on it. This is the device's own account, not your record of it: the model, processor and memory; its operating system, version and the repository that documents it; what its owner expects of anything running there (os_conventions); screen, controls, radios and USB; where its software is kept (software: places to look with file.list and file.read); what is connected and how loud it is right now (state); and every permission the owner has set for you, with what each means (access). Call it before building for this handheld or acting on it.",
            "required": {},
            "optional": {}
        }));
        map.insert("build.install".into(), json!({
            "description": format!("Your own folder on this device is {}. {}", self.account.home, "Install something you built onto this handheld: a tool (a command you can then call here), an app (listed among the owner's tools) or a game (listed among their games). Build and test it on your own computer first; this device only receives the result. Write the finished files into a folder inside your own folder on this device with file.write, with a goblin.json at its top: {\"name\": lowercase words joined by dots (a tool needs at least two, like battery.check, and it becomes the command name), \"kind\": \"tool\" | \"app\" | \"game\", \"title\": what the launcher shows (apps and games), \"description\": what it is for, \"run\": [path of the program inside the folder, then arguments (tools only)], and for a tool \"required\" / \"optional\" parameter tables ({name: {\"type\", \"description\"}}) and \"timeout_ms\"}. A tool receives its parameters as one JSON object on stdin and should print a JSON object. Call handheld.about for what the device is. Plain files only, no links. Installing under a name you already installed replaces it. The owner may have to approve the install on the handheld: the result says whether it is installed or waiting. After a tool is installed or removed the device reconnects, and the tool is callable from then on."),
            "required": {"path": {"type": "string", "description": "Absolute path of the folder, inside your own folder on this device."}},
            "optional": {}
        }));
        map.insert("build.list".into(), json!({
            "description": "List what you have installed on this handheld, and what is waiting for the owner to approve.",
            "required": {},
            "optional": {}
        }));
        map.insert("build.remove".into(), json!({
            "description": "Remove something you installed on this handheld (or withdraw one that is waiting). Only items this profile installed can be removed.",
            "required": {"name": {"type": "string", "description": "The item's name, as in its goblin.json."}},
            "optional": {}
        }));
        for cmd in &self.drop_ins {
            map.insert(cmd.name.clone(), cmd.spec());
        }
        for cmd in self.built.read().unwrap().iter() {
            map.insert(cmd.name.clone(), cmd.spec());
        }
        Value::Object(map)
    }

    /// Whether `command` is one of this executor's drop-ins.
    pub fn is_drop_in(&self, command: &str) -> bool {
        self.drop_ins.iter().any(|c| c.name == command)
    }

    /// Runs one command with the file commands free to reach any path the rules do not
    /// always deny.
    pub fn run(
        &self,
        command: &str,
        params: &Map<String, Value>,
        timeout_ms: Option<i64>,
    ) -> Map<String, Value> {
        self.run_scoped(command, params, timeout_ms, FileScope::Device)
    }

    /// Runs one command and returns the `link.result` body. `files` is how far the file
    /// commands may reach for the profile being served.
    pub fn run_scoped(
        &self,
        command: &str,
        params: &Map<String, Value>,
        timeout_ms: Option<i64>,
        files: FileScope,
    ) -> Map<String, Value> {
        // A file command's path is judged first and replaced by its real location.
        let mut checked;
        let mut params = params;
        if let (Some(rules), "file.read" | "file.write" | "file.list") = (&self.rules, command) {
            let Some(path) = params.get("path").and_then(Value::as_str) else {
                return err("path must be an absolute path");
            };
            match rules.check(std::path::Path::new(path), files) {
                Ok(real) => {
                    checked = params.clone();
                    checked.insert("path".into(), json!(real.to_string_lossy()));
                    params = &checked;
                }
                Err(why) => {
                    log::info!("refused {command}: outside the profile's file reach");
                    return err(why);
                }
            }
        }
        match command {
            "system.run" => self.system_run(params, timeout_ms),
            "file.read" => match fileops::read(params) {
                Ok(v) => ok(v),
                Err(e) => err(e),
            },
            "file.list" => match fileops::list(params) {
                Ok(v) => ok(v),
                Err(e) => err(e),
            },
            "file.write" => match fileops::write(params) {
                Ok(v) => ok(v),
                Err(e) => err(e),
            },
            "device.health" => ok((self.health)()),
            other => {
                let built = self
                    .built
                    .read()
                    .unwrap()
                    .iter()
                    .find(|c| c.name == other)
                    .cloned();
                if let Some(cmd) = self.drop_ins.iter().find(|c| c.name == other) {
                    self.run_drop_in(cmd, params)
                } else if let Some(cmd) = built {
                    self.run_drop_in(&cmd, params)
                } else {
                    err(format!("unsupported command: {other}"))
                }
            }
        }
    }

    fn child_env(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".into(), self.account.home.clone()),
            ("USER".into(), self.account.name.clone()),
            ("LOGNAME".into(), self.account.name.clone()),
            ("PATH".into(), SAFE_PATH.into()),
            (
                "LANG".into(),
                std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".into()),
            ),
        ]
    }

    fn configure(&self, cmd: &mut Command, cwd: &str) {
        cmd.env_clear();
        for (k, v) in self.child_env() {
            cmd.env(k, v);
        }
        cmd.current_dir(cwd);
        cmd.process_group(0); // own process group, so a timeout can kill the group
        if is_root() && self.account.uid != 0 {
            cmd.uid(self.account.uid);
            cmd.gid(self.account.gid);
        }
    }

    fn system_run(
        &self,
        params: &Map<String, Value>,
        timeout_ms: Option<i64>,
    ) -> Map<String, Value> {
        let command = params.get("command").and_then(Value::as_str).unwrap_or("");
        if command.trim().is_empty() {
            return err("command is required");
        }
        let requested = params
            .get("timeout_ms")
            .and_then(Value::as_i64)
            .filter(|v| *v > 0)
            .or(timeout_ms.filter(|v| *v > 0));
        let timeout_s = match requested {
            Some(ms) => ((ms / 1000) as u64).min(MAX_TIMEOUT_S),
            None => DEFAULT_TIMEOUT_S,
        };
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.account.home)
            .to_owned();
        log::info!("system.run as {} (timeout {timeout_s}s)", self.account.name);
        let started = Instant::now();
        let mut cmd = Command::new(shell_path());
        cmd.arg("-c").arg(command);
        self.configure(&mut cmd, &cwd);
        match run_child(cmd, Duration::from_secs(timeout_s), None) {
            Ok(res) => {
                let (out, out_cut) = clip(&res.stdout);
                let (errr, err_cut) = clip(&res.stderr);
                ok(json!({
                    "stdout": out,
                    "stderr": errr,
                    "exit_code": res.exit_code,
                    "timed_out": res.timed_out,
                    "truncated": out_cut || err_cut,
                    "duration_ms": started.elapsed().as_millis() as u64,
                }))
            }
            Err(e) => err(format!("could not start command: {e}")),
        }
    }

    fn run_drop_in(
        &self,
        command: &DropInCommand,
        params: &Map<String, Value>,
    ) -> Map<String, Value> {
        let checked = match command.check_params(params) {
            Ok(c) => c,
            Err(e) => return err(e.to_string()),
        };
        let request = match serde_json::to_vec(&Value::Object(checked)) {
            Ok(r) => r,
            Err(_) => return err("could not encode parameters"),
        };
        let timeout = Duration::from_millis(command.timeout_ms as u64);
        log::info!("{} as {}", command.name, self.account.name);
        let mut cmd = Command::new(&command.argv[0]);
        cmd.args(&command.argv[1..]);
        self.configure(&mut cmd, &self.account.home);
        match run_child(cmd, timeout, Some(request)) {
            Ok(res) => {
                if res.timed_out {
                    return err(format!(
                        "{} timed out after {}s",
                        command.name,
                        command.timeout_ms / 1000
                    ));
                }
                if res.exit_code != 0 {
                    let tail = String::from_utf8_lossy(&res.stderr);
                    let last = tail.lines().last().unwrap_or("");
                    let reason = if last.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", &last[..last.len().min(500)])
                    };
                    return err(format!(
                        "{} exited with {}{reason}",
                        command.name, res.exit_code
                    ));
                }
                let (out, cut) = clip(&res.stdout);
                if !cut {
                    if let Ok(Value::Object(payload)) = serde_json::from_str::<Value>(&out) {
                        return ok(Value::Object(payload));
                    }
                }
                if cut {
                    ok(json!({"output": out, "truncated": true}))
                } else {
                    ok(json!({"output": out}))
                }
            }
            Err(e) => err(format!("could not start {}: {e}", command.argv[0])),
        }
    }
}

struct ChildResult {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: i32,
    timed_out: bool,
}

/// Runs `cmd` with its own process group, capturing bounded output and killing
/// the whole group on timeout.
fn run_child(
    mut cmd: Command,
    timeout: Duration,
    stdin: Option<Vec<u8>>,
) -> std::io::Result<ChildResult> {
    use wait_timeout::ChildExt;

    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let pgid = child.id();

    if let (Some(data), Some(mut sink)) = (stdin, child.stdin.take()) {
        // Write stdin on a thread so a full pipe cannot deadlock us.
        std::thread::spawn(move || {
            let _ = sink.write_all(&data);
        });
    }

    let out_buf = Arc::new(Mutex::new(Vec::new()));
    let err_buf = Arc::new(Mutex::new(Vec::new()));
    let out_reader = spawn_reader(child.stdout.take(), out_buf.clone());
    let err_reader = spawn_reader(child.stderr.take(), err_buf.clone());

    let (status, timed_out) = match child.wait_timeout(timeout)? {
        Some(status) => (status.code().unwrap_or(-1), false),
        None => {
            kill_group(pgid);
            let status = child
                .wait_timeout(KILL_GRACE)?
                .and_then(|s| s.code())
                .unwrap_or(-1);
            (status, true)
        }
    };
    // Readers EOF once the pipes close; give them a brief moment.
    let _ = out_reader.join();
    let _ = err_reader.join();

    let stdout = out_buf.lock().unwrap().clone();
    let stderr = err_buf.lock().unwrap().clone();
    Ok(ChildResult {
        stdout,
        stderr,
        exit_code: status,
        timed_out,
    })
}

fn spawn_reader<R: Read + Send + 'static>(
    pipe: Option<R>,
    buf: Arc<Mutex<Vec<u8>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Some(mut pipe) = pipe else { return };
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut b = buf.lock().unwrap();
                    if b.len() <= MAX_OUTPUT_BYTES {
                        let room = MAX_OUTPUT_BYTES + 1 - b.len();
                        b.extend_from_slice(&chunk[..n.min(room)]);
                    }
                }
            }
        }
    })
}

/// Why a group kill was refused. Every refusal is a safety stop, never a retry.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KillRefused {
    /// 0 and 1 are never a child's group: `kill(0)` hits our own group and
    /// `kill(-1)` hits every process this user can signal.
    Reserved,
    /// Out of range for `pid_t`; negating it could wrap.
    Range,
    /// The target is our own process group.
    OwnGroup,
    /// The process is not the leader of the group we think it leads.
    NotLeader,
}

/// Checks that signalling `-pgid` can only reach the child's own group.
pub(crate) fn validate_group(pgid: u32) -> Result<libc::pid_t, KillRefused> {
    if pgid <= 1 {
        return Err(KillRefused::Reserved);
    }
    let pid = libc::pid_t::try_from(pgid).map_err(|_| KillRefused::Range)?;
    // SAFETY: getpgrp/getpgid take no pointers and only read kernel state.
    let own = unsafe { libc::getpgrp() };
    if pid == own {
        return Err(KillRefused::OwnGroup);
    }
    if unsafe { libc::getpgid(pid) } != pid {
        return Err(KillRefused::NotLeader);
    }
    Ok(pid)
}

/// SIGKILLs the child's process group. Uses the syscall directly: an earlier
/// version shelled out to `kill -KILL -<pgid>`, and a host-wide user-session
/// kill coincided with that test, so no external `kill` and no broadcast
/// pid can ever be reached from here.
fn kill_group(pgid: u32) {
    match validate_group(pgid) {
        // SAFETY: pid > 1 and is a group we created and lead; the negated value
        // addresses exactly that group.
        Ok(pid) => unsafe {
            libc::kill(-pid, libc::SIGKILL);
        },
        Err(why) => log::warn!("refusing to kill process group {pgid}: {why:?}"),
    }
}

/// Clips output to the 96 KiB message budget, accounting for JSON escape growth.
fn clip(data: &[u8]) -> (String, bool) {
    let mut cut = data.len() > MAX_OUTPUT_BYTES;
    let end = data.len().min(MAX_OUTPUT_BYTES);
    let mut text = String::from_utf8_lossy(&data[..end]).into_owned();
    while json_len(&text) > MAX_OUTPUT_BYTES {
        let keep = text.len() * 3 / 4;
        text.truncate(floor_char_boundary(&text, keep));
        cut = true;
    }
    (text, cut)
}

fn json_len(text: &str) -> usize {
    // Length of the JSON string without the surrounding quotes.
    serde_json::to_string(text)
        .map(|s| s.len().saturating_sub(2))
        .unwrap_or(text.len())
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn exec() -> Executor {
        Executor::new(
            RunAs::current(),
            Vec::new(),
            Arc::new(|| json!({"version": "test"})),
        )
    }

    #[test]
    fn system_run_captures_output_and_exit_code() {
        let mut p = Map::new();
        p.insert("command".into(), json!("printf hello; exit 3"));
        let r = exec().run("system.run", &p, None);
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["payload"]["stdout"], json!("hello"));
        assert_eq!(r["payload"]["exit_code"], json!(3));
        assert_eq!(r["payload"]["timed_out"], json!(false));
    }

    #[test]
    fn kill_guard_refuses_broadcast_and_own_group() {
        assert_eq!(validate_group(0), Err(KillRefused::Reserved));
        assert_eq!(validate_group(1), Err(KillRefused::Reserved));
        assert_eq!(validate_group(u32::MAX), Err(KillRefused::Range));
        let own = unsafe { libc::getpgrp() } as u32;
        assert_eq!(validate_group(own), Err(KillRefused::OwnGroup));
        // Our own pid is not a group leader under cargo test, or is the own group.
        assert!(validate_group(std::process::id()).is_err());
    }

    #[test]
    fn system_run_times_out_and_kills_group() {
        let mut p = Map::new();
        // Spawn a child that would outlive the timeout; the whole group is killed.
        p.insert("command".into(), json!("sleep 30 & sleep 30"));
        p.insert("timeout_ms".into(), json!(500));
        let start = Instant::now();
        let r = exec().run("system.run", &p, None);
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "kill should be prompt"
        );
        assert_eq!(r["payload"]["timed_out"], json!(true));
    }

    #[test]
    fn device_health_returns_payload() {
        let r = exec().run("device.health", &Map::new(), None);
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["payload"]["version"], json!("test"));
    }

    #[test]
    fn unknown_command_errors() {
        let r = exec().run("nope.x", &Map::new(), None);
        assert_eq!(r["ok"], json!(false));
    }

    #[test]
    fn consent_gate() {
        let mut consent = Consent::default();
        assert!(command_allowed("system.run", &consent, false));
        consent.shell = false;
        assert!(!command_allowed("system.run", &consent, false));
        assert!(command_allowed("device.health", &consent, false));
        consent.drop_ins = false;
        assert!(!command_allowed("pump.set", &consent, true));
        assert_eq!(refusal("system.run")["ok"], json!(false));
    }

    #[test]
    fn file_write_and_read_via_executor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        let data = base64::engine::general_purpose::STANDARD.encode(b"content");
        let mut p = Map::new();
        p.insert("path".into(), json!(path.to_string_lossy()));
        p.insert("data_b64".into(), json!(data));
        p.insert("final".into(), json!(true));
        let e = exec();
        assert_eq!(e.run("file.write", &p, None)["ok"], json!(true));
        let mut rp = Map::new();
        rp.insert("path".into(), json!(path.to_string_lossy()));
        let r = e.run("file.read", &rp, None);
        assert_eq!(r["ok"], json!(true));
    }
}
