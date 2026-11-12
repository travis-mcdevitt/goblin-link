//! Where the file commands may reach, per profile.
//!
//! Each profile has a folder of its own (its "home"), and its consent says how far beyond
//! it `file.read` and `file.write` may go ([`FileScope`]). Whatever the level, three places
//! stay out of reach: the daemon's data directory (every profile's credentials and cached
//! conversations), the other profiles' folders, and the owner's command files (a file
//! written there would become a command).
//!
//! This holds the file commands only. `system.run` is a shell: a profile allowed to use it
//! can reach whatever the account it runs as can.

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

/// A root as the file system names it (it may not exist yet).
fn real(root: &Path) -> PathBuf {
    resolve(root).unwrap_or_else(|_| root.to_path_buf())
}

impl FileRules {
    /// Rules for tests and for running without profiles: one folder, nothing denied.
    pub fn only_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            homes: home.clone(),
            home,
            storage: None,
            denied: Vec::new(),
        }
    }

    /// The real path to use for `path` at `scope`, or why it is refused.
    pub fn check(&self, path: &Path, scope: FileScope) -> Result<PathBuf, String> {
        let resolved = resolve(path)?;
        let home = real(&self.home);
        let in_home = resolved.starts_with(&home);
        let off_limits = self.denied.iter().any(|d| resolved.starts_with(real(d)))
            || (resolved.starts_with(real(&self.homes)) && !in_home);
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
                        .is_some_and(|s| resolved.starts_with(real(s)))
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
