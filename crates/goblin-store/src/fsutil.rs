//! Crash-safe file helpers.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::StoreError;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates `dir` and missing parents; directories created here get mode 0700. Mode changes are
/// best effort because FAT file systems ignore them.
pub fn create_private_dir(dir: &Path) -> Result<(), StoreError> {
    if dir.is_dir() {
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| StoreError::io(dir, e))
}

/// Writes `bytes` to `path` atomically: a temp file in the same directory is written, synced and
/// renamed over `path`, so readers see the old or the new content, never a partial file. The
/// file is created with `mode` (use 0o600 for secrets). Failing to fsync the directory or to
/// chmod (FAT) is not an error.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    create_private_dir(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let write = || -> std::io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        // The open mode is subject to the umask; make it exact. FAT ignores or rejects this.
        let _ = f.set_permissions(fs::Permissions::from_mode(mode));
        drop(f);
        fs::rename(&tmp, path)
    };
    if let Err(e) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(StoreError::io(path, e));
    }
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Serializes `value` as pretty JSON (like the Python SDK's `indent=2`) and writes it atomically.
pub fn write_json_atomic<T: Serialize>(
    path: &Path,
    value: &T,
    mode: u32,
) -> Result<(), StoreError> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|e| StoreError::corrupt(path, e.to_string()))?;
    bytes.push(b'\n');
    write_atomic(path, &bytes, mode)
}

/// Reads and parses a JSON file; `Ok(None)` when it does not exist.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, StoreError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| StoreError::corrupt(path, e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::io(path, e)),
    }
}

/// Removes a file; a missing file is fine.
pub fn remove_file(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StoreError::io(path, e)),
    }
}

/// Renames a corrupt file to `<name>.corrupt` (replacing an older one) so that it is kept for
/// inspection but no longer blocks startup.
pub fn quarantine(path: &Path) {
    let mut target = path.as_os_str().to_owned();
    target.push(".corrupt");
    let _ = fs::rename(path, target);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn writes_with_requested_mode_and_leaves_no_temp_files() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("sub").join("secret");
        write_atomic(&p, b"one", 0o600).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"one");
        assert_eq!(mode(&p), 0o600);
        assert_eq!(mode(p.parent().unwrap()), 0o700);
        write_atomic(&p, b"two", 0o600).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn rewrite_tightens_a_loose_existing_file() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("f");
        fs::write(&p, "old").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o666)).unwrap();
        write_atomic(&p, b"new", 0o600).unwrap();
        assert_eq!(mode(&p), 0o600);
    }

    #[test]
    fn failure_keeps_the_old_content_and_cleans_up() {
        let t = tempfile::tempdir().unwrap();
        // The destination is a non-empty directory, so the final rename fails.
        let p = t.path().join("dest");
        fs::create_dir(&p).unwrap();
        fs::write(p.join("child"), "x").unwrap();
        assert!(write_atomic(&p, b"data", 0o600).is_err());
        assert!(p.join("child").exists());
        let names: Vec<_> = fs::read_dir(t.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "temp file removed");
    }

    #[test]
    fn existing_file_survives_a_failed_write() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("f");
        write_atomic(&p, b"good", 0o600).unwrap();
        // Make the directory read-only so creating the temp file fails (skipped as root).
        fs::set_permissions(t.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let r = write_atomic(&p, b"bad", 0o600);
        fs::set_permissions(t.path(), fs::Permissions::from_mode(0o700)).unwrap();
        if r.is_err() {
            assert_eq!(fs::read(&p).unwrap(), b"good");
        }
    }

    #[test]
    fn json_helpers() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("j.json");
        assert!(read_json::<serde_json::Value>(&p).unwrap().is_none());
        write_json_atomic(&p, &serde_json::json!({"a": 1}), 0o600).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("\n  \"a\": 1"),
            "indent=2 like the Python SDK"
        );
        assert_eq!(read_json::<serde_json::Value>(&p).unwrap().unwrap()["a"], 1);
        fs::write(&p, "{broken").unwrap();
        assert!(matches!(
            read_json::<serde_json::Value>(&p),
            Err(StoreError::Corrupt { .. })
        ));
        quarantine(&p);
        assert!(!p.exists() && t.path().join("j.json.corrupt").exists());
        remove_file(&p).unwrap();
    }
}
