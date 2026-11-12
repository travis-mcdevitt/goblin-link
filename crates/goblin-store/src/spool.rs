//! The spool directory: where audio, images and dropped files are exchanged with clients.

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::fsutil::create_private_dir;
use crate::StoreError;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// What a cleanup pass removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// Files deleted.
    pub removed: usize,
    /// Bytes freed.
    pub bytes: u64,
}

/// A spool directory (mode 0700).
#[derive(Debug, Clone)]
pub struct Spool {
    dir: PathBuf,
}

fn sanitize(part: &str) -> String {
    let s: String = part
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect();
    if s.is_empty() {
        "f".into()
    } else {
        s
    }
}

impl Spool {
    /// Opens the spool, creating the directory if needed.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        create_private_dir(dir)?;
        let dir = fs::canonicalize(dir).map_err(|e| StoreError::io(dir, e))?;
        Ok(Self { dir })
    }

    /// The (canonical) spool directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Creates a new, uniquely named file `<prefix>-<pid>-<n>-<nanos>.<ext>` (mode 0600) and
    /// returns its path and handle. `prefix` and `ext` are reduced to safe characters.
    pub fn create(&self, prefix: &str, ext: &str) -> Result<(PathBuf, File), StoreError> {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let ext = sanitize(ext);
        for _ in 0..8 {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = format!(
                "{}-{}-{n}-{nanos:09}.{ext}",
                sanitize(prefix),
                std::process::id()
            );
            let path = self.dir.join(name);
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(f) => return Ok((path, f)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(StoreError::io(path, e)),
            }
        }
        Err(StoreError::io(
            &self.dir,
            std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no unique name found"),
        ))
    }

    /// Checks that `path` names an existing regular file inside the spool (after resolving
    /// symlinks and `..`) and returns its canonical path. Use it on every client-supplied path.
    pub fn resolve(&self, path: &Path) -> Result<PathBuf, StoreError> {
        let canon = fs::canonicalize(path).map_err(|e| StoreError::io(path, e))?;
        if canon.starts_with(&self.dir) && canon.is_file() {
            Ok(canon)
        } else {
            Err(StoreError::OutsideDirectory(path.to_owned()))
        }
    }

    /// Deletes regular files not modified for more than `max_age`. Directories and symlinks are
    /// left alone.
    pub fn cleanup(&self, max_age: Duration) -> Result<CleanupReport, StoreError> {
        self.cleanup_at(max_age, SystemTime::now())
    }

    /// Like [`cleanup`](Self::cleanup) with an explicit clock.
    pub fn cleanup_at(
        &self,
        max_age: Duration,
        now: SystemTime,
    ) -> Result<CleanupReport, StoreError> {
        let mut report = CleanupReport::default();
        let entries = fs::read_dir(&self.dir).map_err(|e| StoreError::io(&self.dir, e))?;
        for entry in entries.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let age = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok());
            if age.is_some_and(|a| a > max_age) && fs::remove_file(entry.path()).is_ok() {
                report.removed += 1;
                report.bytes += meta.len();
            }
        }
        Ok(report)
    }
}
