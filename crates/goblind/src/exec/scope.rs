//! Where the file commands may reach, per profile.
//!
//! Each profile has a folder of its own (its "home"), and its consent says how far beyond
//! it `file.read` and `file.write` may go ([`FileScope`]). Whatever the level, three places
//! stay out of reach: the daemon's data directory (every profile's credentials and cached
//! conversations), the other profiles' folders, and the owner's command files (a file
//! written there would become a command).
//!
//! Whatever the level, the file commands also reach only plain files and folders: not a
//! device (a raw disk holds every file on it, the denied ones too), a pipe or socket, nor
//! the kernel's views in `/proc` and `/sys` (`/proc/self/mem` is the daemon's memory, with
//! its tokens in it).
//!
//! This holds the file commands only. `system.run` is a shell: a profile allowed to use it
//! can reach whatever the account it runs as can.

use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use goblin_ipc::types::FileScope;

/// The paths one profile's file commands are judged against.
#[derive(Debug, Clone)]
pub struct FileRules {
    /// The profile's own folder.
    pub home: PathBuf,
    /// The folder holding every profile's home.
    pub homes: PathBuf,
    /// The removable storage, where the device has one.
    pub storage: Option<PathBuf>,
    /// Never reachable (data directory, command files).
    pub denied: Vec<PathBuf>,
}

/// `path` with symbolic links and `..` resolved as far as it exists; the part that does not
/// exist yet (a file about to be written) is appended as given.
fn resolve(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("path must be an absolute path".into());
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path must not contain '..'".into());
    }
    let mut existing = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(real) => {
                let mut out = real;
                for part in rest.iter().rev() {
                    out.push(part);
                }
                return Ok(out);
            }
            Err(_) => {
                let Some(name) = existing.file_name() else {
                    return Err("path cannot be resolved".into());
                };
                rest.push(name.to_owned());
                if !existing.pop() {
                    return Err("path cannot be resolved".into());
                }
            }
        }
    }
}

/// The kernel's own views of processes and devices: never files to read or write.
const KERNEL_VIEWS: [&str; 2] = ["/proc", "/sys"];

/// Whether `meta` is a swap file the kernel is using (`/proc/swaps`).
fn is_swap(meta: &std::fs::Metadata) -> bool {
    let Ok(swaps) = std::fs::read_to_string("/proc/swaps") else {
        return false;
    };
    swap_paths(&swaps).iter().any(|name| {
        std::fs::metadata(name).is_ok_and(|m| m.dev() == meta.dev() && m.ino() == meta.ino())
    })
}

/// The paths in the text of `/proc/swaps`, which writes a space in a name as `\040` (and
/// a tab, newline or backslash likewise in octal).
fn swap_paths(swaps: &str) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    swaps
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .map(|escaped| {
            let raw = escaped.as_bytes();
            let mut out = Vec::with_capacity(raw.len());
            let mut i = 0;
            while i < raw.len() {
                let octal = raw
                    .get(i + 1..i + 4)
                    .filter(|d| raw[i] == b'\\' && d.iter().all(|c| (b'0'..=b'7').contains(c)));
                match octal {
                    Some(d) => {
                        let v = d.iter().fold(0u32, |v, c| v * 8 + u32::from(c - b'0'));
                        out.push(u8::try_from(v).unwrap_or(b'?'));
                        i += 4;
                    }
                    None => {
                        out.push(raw[i]);
                        i += 1;
                    }
                }
            }
            PathBuf::from(std::ffi::OsString::from_vec(out))
        })
        .collect()
}

/// A root as the file system names it (it may not exist yet).
fn real(root: &Path) -> PathBuf {
    resolve(root).unwrap_or_else(|_| root.to_path_buf())
}

// FAT can resolve more than one spelling to a directory. Compare each existing ancestor
// by device and inode too; canonicalize alone does not expand DOS short names.
fn within(path: &Path, root: &Path) -> bool {
    let root = real(root);
    if path.starts_with(&root) {
        return true;
    }
    let Ok(identity) = std::fs::metadata(&root) else {
        return false;
    };
    path.ancestors().any(|ancestor| {
        std::fs::metadata(ancestor)
            .is_ok_and(|m| m.dev() == identity.dev() && m.ino() == identity.ino())
    })
}

impl FileRules {
    /// The real path to use for `path` at `scope`, or why it is refused.
    pub fn check(&self, path: &Path, scope: FileScope) -> Result<PathBuf, String> {
        let resolved = resolve(path)?;
        if KERNEL_VIEWS.iter().any(|k| within(&resolved, Path::new(k))) {
            return Err(format!(
                "{} is off limits on this device",
                path.to_string_lossy()
            ));
        }
        // A path that does not exist yet becomes a plain file when written.
        if let Ok(meta) = std::fs::metadata(&resolved) {
            if !meta.is_file() && !meta.is_dir() {
                return Err(format!(
                    "{} is not a plain file or folder",
                    path.to_string_lossy()
                ));
            }
            // A swap file is plain, and holds whatever memory was swapped out.
            if meta.is_file() && is_swap(&meta) {
                return Err(format!(
                    "{} is off limits on this device",
                    path.to_string_lossy()
                ));
            }
        }
        let home = real(&self.home);
        let in_home = within(&resolved, &home);
        let off_limits = self.denied.iter().any(|d| within(&resolved, d))
            || (within(&resolved, &self.homes) && !in_home);
        if off_limits {
            return Err(format!(
                "{} is off limits on this device",
                path.to_string_lossy()
            ));
        }
        let allowed = match scope {
            FileScope::Home => in_home,
            FileScope::Storage => {
                in_home
                    || self
                        .storage
                        .as_deref()
                        .is_some_and(|s| within(&resolved, s))
            }
            FileScope::Device => true,
        };
        if allowed {
            Ok(resolved)
        } else {
            let reach = match scope {
                FileScope::Home => format!("this profile's folder, {}", home.display()),
                _ => "this profile's folder and the storage card".to_owned(),
            };
            Err(format!(
                "{} is outside what the owner allows here: files are limited to {reach}",
                path.to_string_lossy()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(root: &Path) -> FileRules {
        for d in [
            "card/Muse/work",
            "card/Muse/personal",
            "card/Roms",
            "card/data",
            "sys",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        FileRules {
            home: root.join("card/Muse/work"),
            homes: root.join("card/Muse"),
            storage: Some(root.join("card")),
            denied: vec![root.join("card/data")],
        }
    }

    #[test]
    fn home_scope_keeps_to_the_profile_folder() {
        let dir = tempfile::tempdir().unwrap();
        let r = rules(dir.path());
        let ok = dir.path().join("card/Muse/work/notes/a.txt");
        assert!(
            r.check(&ok, FileScope::Home).is_ok(),
            "a new file in a new folder"
        );
        for out in ["card/Roms/x.gb", "sys/passwd", "card/Muse/personal/a.txt"] {
            assert!(
                r.check(&dir.path().join(out), FileScope::Home).is_err(),
                "{out}"
            );
        }
    }

    #[test]
    fn storage_scope_adds_the_card_but_not_other_profiles_or_data() {
        let dir = tempfile::tempdir().unwrap();
        let r = rules(dir.path());
        assert!(r
            .check(&dir.path().join("card/Roms/x.gb"), FileScope::Storage)
            .is_ok());
        assert!(r
            .check(&dir.path().join("sys/passwd"), FileScope::Storage)
            .is_err());
        assert!(r
            .check(
                &dir.path().join("card/Muse/personal/a.txt"),
                FileScope::Storage
            )
            .is_err());
        assert!(r
            .check(&dir.path().join("card/data/token"), FileScope::Storage)
            .is_err());
    }

    #[test]
    fn device_scope_still_refuses_credentials_and_other_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let r = rules(dir.path());
        assert!(r
            .check(&dir.path().join("sys/passwd"), FileScope::Device)
            .is_ok());
        assert!(r
            .check(
                &dir.path().join("card/data/profiles/x/sdk_token"),
                FileScope::Device
            )
            .is_err());
        assert!(r
            .check(
                &dir.path().join("card/Muse/personal/a.txt"),
                FileScope::Device
            )
            .is_err());
    }

    #[test]
    fn swap_files_are_read_from_proc_swaps() {
        let text = "Filename\t\t\t\tType\t\tSize\tUsed\tPriority\n\
                    /swapfile                               file\t\t1048572\t0\t-2\n\
                    /mnt/SDCARD/my\\040swap                  file\t\t1048572\t0\t-3\n";
        assert_eq!(
            swap_paths(text),
            [
                PathBuf::from("/swapfile"),
                PathBuf::from("/mnt/SDCARD/my swap")
            ]
        );
        assert!(swap_paths("Filename\tType\tSize\tUsed\tPriority\n").is_empty());
    }

    #[test]
    fn only_plain_files_and_folders_at_any_reach() {
        let dir = tempfile::tempdir().unwrap();
        let r = rules(dir.path());
        let socket = dir.path().join("card/Muse/work/sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        for scope in [FileScope::Home, FileScope::Device] {
            let err = r.check(&socket, scope).unwrap_err();
            assert!(err.contains("not a plain file"), "{err}");
        }
        for special in [
            "/dev/null",
            "/proc/self/mem",
            "/proc/self/environ",
            "/proc",
            "/sys/class",
        ] {
            assert!(
                r.check(Path::new(special), FileScope::Device).is_err(),
                "{special}"
            );
        }
        // Ordinary places outside the card are still reachable at this level.
        assert!(r.check(Path::new("/etc"), FileScope::Device).is_ok());
        assert!(r.check(Path::new("/dev"), FileScope::Device).is_ok());
    }

    #[test]
    fn a_different_spelling_of_the_same_file_is_still_denied() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = rules(dir.path());
        let secret = dir.path().join("card/data/token");
        let alias = dir.path().join("card/Roms/TOKEN");
        std::fs::write(&secret, "secret").unwrap();
        std::fs::hard_link(&secret, &alias).unwrap();
        r.denied.push(secret);
        assert!(r.check(&alias, FileScope::Device).is_err());
    }

    #[test]
    fn links_and_dot_dot_do_not_escape() {
        let dir = tempfile::tempdir().unwrap();
        let r = rules(dir.path());
        std::os::unix::fs::symlink(
            dir.path().join("sys"),
            dir.path().join("card/Muse/work/out"),
        )
        .unwrap();
        assert!(r
            .check(
                &dir.path().join("card/Muse/work/out/passwd"),
                FileScope::Home
            )
            .is_err());
        assert!(r
            .check(
                &dir.path().join("card/Muse/work/../personal/a"),
                FileScope::Home
            )
            .is_err());
        assert!(r.check(Path::new("relative/a"), FileScope::Device).is_err());
        // A link into the data directory is refused at every level.
        std::os::unix::fs::symlink(dir.path().join("card/data"), dir.path().join("card/Roms/d"))
            .unwrap();
        assert!(r
            .check(&dir.path().join("card/Roms/d/token"), FileScope::Device)
            .is_err());
    }
}
