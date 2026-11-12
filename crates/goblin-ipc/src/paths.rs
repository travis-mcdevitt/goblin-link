//! Socket locations, the `active` symlink, and runtime directory preparation.

use std::io;
use std::os::unix::fs::{symlink, DirBuilderExt};
use std::path::{Path, PathBuf};

/// A profile name that cannot be used as a socket file name.
#[derive(Debug, thiserror::Error)]
#[error("invalid profile name {0:?}")]
pub struct InvalidProfileName(pub String);

fn check_name(profile: &str) -> Result<(), InvalidProfileName> {
    let ok = !profile.is_empty()
        && profile.len() <= 64
        && profile
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(InvalidProfileName(profile.to_owned()))
    }
}

/// `<runtime_dir>/<profile>.sock`.
pub fn socket_path(runtime_dir: &Path, profile: &str) -> Result<PathBuf, InvalidProfileName> {
    check_name(profile)?;
    Ok(runtime_dir.join(format!("{profile}.sock")))
}

/// `<runtime_dir>/active`, the symlink to the active profile's socket.
pub fn active_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("active")
}

/// Points `<runtime_dir>/active` at `<profile>.sock` (relative target), replacing any previous
/// link atomically.
pub fn set_active(runtime_dir: &Path, profile: &str) -> io::Result<()> {
    check_name(profile).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let tmp = runtime_dir.join(format!(".active.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    symlink(format!("{profile}.sock"), &tmp)?;
    std::fs::rename(&tmp, active_path(runtime_dir)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The socket path the `active` symlink points at.
pub fn resolve_active(runtime_dir: &Path) -> io::Result<PathBuf> {
    let target = std::fs::read_link(active_path(runtime_dir))?;
    Ok(runtime_dir.join(target))
}

/// Creates `dir` (and missing parents) with mode 0700 for the final component when it does not
/// exist yet. An existing directory is left unchanged.
pub fn prepare_runtime_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_validated() {
        let d = Path::new("/run/x");
        assert_eq!(socket_path(d, "dev").unwrap(), d.join("dev.sock"));
        for bad in ["", "a/b", "..", "a.b", "a b"] {
            assert!(socket_path(d, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn active_link_is_replaced_atomically() {
        let t = tempfile::tempdir().unwrap();
        set_active(t.path(), "one").unwrap();
        assert_eq!(resolve_active(t.path()).unwrap(), t.path().join("one.sock"));
        set_active(t.path(), "two").unwrap();
        assert_eq!(resolve_active(t.path()).unwrap(), t.path().join("two.sock"));
        assert!(set_active(t.path(), "../x").is_err());
        let leftovers = std::fs::read_dir(t.path()).unwrap().count();
        assert_eq!(leftovers, 1);
    }

    #[test]
    fn runtime_dir_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("a").join("muse");
        prepare_runtime_dir(&d).unwrap();
        let mode = std::fs::metadata(&d).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        prepare_runtime_dir(&d).unwrap();
    }
}
