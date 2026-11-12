//! Hook scripts: small POSIX `sh` programs with a fixed calling contract.
//!
//! # Contract
//!
//! * Invoked as `sh <platform_dir>/<path> <args...>` with the platform directory as the
//!   working directory.
//! * Environment: `GOBLIN_PLATFORM_ID`, `GOBLIN_PLATFORM_DIR`, `GOBLIN_TMP_DIR`, and, when known,
//!   `GOBLIN_DATA_DIR`, `GOBLIN_GAMES_DIR`, `GOBLIN_TOOLS_DIR`. A same-named variable already set in
//!   the caller's environment wins over the value from `platform.toml`.
//! * Standard output carries the result (at most [`MAX_OUTPUT`] bytes are kept); standard error
//!   carries diagnostics.
//! * Exit status 0 is success, 2 means "not supported on this device or in this state" (reported
//!   as unavailable, not as a failure), anything else is a failure.
//! * The script is killed after its time limit (default 5 s).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::config::HookSpec;
use crate::error::{HookFailure, PlatformError, Result};

/// Largest amount of stdout or stderr kept from a hook.
pub const MAX_OUTPUT: usize = 64 * 1024;

/// How long to wait for output pipes to close after the script exited.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// A resolved hook script.
#[derive(Debug, Clone)]
pub struct Hook {
    script: PathBuf,
    name: String,
    timeout: Duration,
    working_dir: PathBuf,
    env: Vec<(String, String)>,
}

impl Hook {
    /// Resolve `spec` against `platform_dir`; `env` is passed to every invocation.
    pub fn new(spec: &HookSpec, platform_dir: &Path, env: Vec<(String, String)>) -> Self {
        Self {
            script: platform_dir.join(spec.path()),
            name: spec.path().to_owned(),
            timeout: Duration::from_millis(spec.timeout_ms()),
            working_dir: platform_dir.to_owned(),
            env,
        }
    }

    /// Path of the script file.
    pub fn script(&self) -> &Path {
        &self.script
    }

    /// Run the hook and return its trimmed standard output.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        self.run_with_input(args, None)
    }

    /// Run the hook, feeding `stdin` to it, and return its trimmed standard output.
    pub fn run_with_input(&self, args: &[&str], stdin: Option<&str>) -> Result<String> {
        let fail = |kind| PlatformError::Hook {
            hook: self.name.clone(),
            kind,
        };
        // `sh` exits 2 for a missing script, which would read as "unsupported".
        if !self.script.is_file() {
            return Err(fail(HookFailure::Spawn(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} not found", self.script.display()),
            ))));
        }
        let mut command = Command::new("sh");
        command
            .arg(&self.script)
            .args(args)
            .current_dir(&self.working_dir)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| fail(HookFailure::Spawn(e)))?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            // A script that ignores its input may exit first; a broken pipe is fine.
            let _ = pipe.write_all(text.as_bytes());
        }
        let out_rx = collect(child.stdout.take());
        let err_rx = collect(child.stderr.take());
        let status = match child.wait_timeout(self.timeout) {
            Ok(Some(status)) => status,
            Ok(None) => {
                // Best effort: the script is being abandoned either way.
                let _ = child.kill();
                let _ = child.wait();
                return Err(fail(HookFailure::Timeout(self.timeout)));
            }
            Err(e) => return Err(fail(HookFailure::Spawn(e))),
        };
        let stdout = out_rx.recv_timeout(DRAIN_GRACE).unwrap_or_default();
        let stderr = err_rx.recv_timeout(DRAIN_GRACE).unwrap_or_default();
        match status.code() {
            Some(0) => Ok(stdout.trim().to_owned()),
            Some(2) => Err(fail(HookFailure::Unsupported(stderr.trim().to_owned()))),
            code => Err(fail(HookFailure::Failed {
                code: code.unwrap_or(-1),
                stderr: stderr.trim().to_owned(),
            })),
        }
    }
}

/// Read a pipe on a helper thread, keeping at most [`MAX_OUTPUT`] bytes and discarding the rest
/// so the script never blocks on a full pipe.
fn collect<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut kept = Vec::new();
        if let Some(mut pipe) = pipe {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let room = MAX_OUTPUT.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
        // The receiver may have given up already; that is fine.
        let _ = tx.send(String::from_utf8_lossy(&kept).into_owned());
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn hook_with(script: &str, timeout_ms: u64) -> (tempfile::TempDir, Hook) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("hooks")).expect("mkdir");
        std::fs::write(dir.path().join("hooks/t.sh"), script).expect("write");
        let spec = HookSpec::Detailed {
            path: "hooks/t.sh".into(),
            timeout_ms: Some(timeout_ms),
        };
        let hook = Hook::new(
            &spec,
            dir.path(),
            vec![("GOBLIN_PLATFORM_ID".into(), "t".into())],
        );
        (dir, hook)
    }

    #[test]
    fn passes_arguments_environment_and_returns_trimmed_stdout() {
        let (_d, hook) = hook_with("echo \"$GOBLIN_PLATFORM_ID:$1:$2\"\n", 2000);
        assert_eq!(hook.run(&["a b", "c"]).expect("run"), "t:a b:c");
    }

    #[test]
    fn runs_in_platform_directory() {
        let (d, hook) = hook_with("pwd\n", 2000);
        let out = hook.run(&[]).expect("run");
        let expected = d.path().canonicalize().expect("canon");
        assert_eq!(PathBuf::from(out).canonicalize().expect("canon"), expected);
    }

    #[test]
    fn stdin_is_delivered() {
        let (_d, hook) = hook_with("cat\n", 2000);
        assert_eq!(
            hook.run_with_input(&[], Some("hello")).expect("run"),
            "hello"
        );
    }

    #[test]
    fn exit_two_is_unsupported() {
        let (_d, hook) = hook_with("echo nope >&2\nexit 2\n", 2000);
        let err = hook.run(&[]).expect_err("must fail");
        assert!(err.is_unavailable(), "{err}");
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn other_exit_is_failure_with_stderr() {
        let (_d, hook) = hook_with("echo bad >&2\nexit 7\n", 2000);
        match hook.run(&[]).expect_err("must fail") {
            PlatformError::Hook {
                kind: HookFailure::Failed { code, stderr },
                ..
            } => {
                assert_eq!(code, 7);
                assert_eq!(stderr, "bad");
            }
            other => panic!("unexpected {other}"),
        }
    }

    #[test]
    fn hanging_hook_is_killed() {
        let (_d, hook) = hook_with("exec sleep 30\n", 200);
        let start = Instant::now();
        let err = hook.run(&[]).expect_err("must time out");
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(matches!(
            err,
            PlatformError::Hook {
                kind: HookFailure::Timeout(_),
                ..
            }
        ));
    }

    #[test]
    fn huge_output_is_bounded_and_does_not_block() {
        let (_d, hook) = hook_with("yes | head -c 500000\n", 5000);
        let len = hook.run(&[]).expect("run").len();
        assert!((MAX_OUTPUT - 2..=MAX_OUTPUT).contains(&len), "{len}");
    }

    #[test]
    fn missing_script_fails_cleanly() {
        let (d, hook) = hook_with("true\n", 1000);
        std::fs::remove_file(d.path().join("hooks/t.sh")).expect("rm");
        assert!(matches!(
            hook.run(&[]).expect_err("must fail"),
            PlatformError::Hook {
                kind: HookFailure::Spawn(_),
                ..
            }
        ));
    }
}
