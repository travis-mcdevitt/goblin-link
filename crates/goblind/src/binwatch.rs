//! Notices when the daemon's own binary is replaced or removed, so that an updated or
//! uninstalled pak does not leave the old daemon running (as root, linked to the agent)
//! until the next reboot. The Pak Store unpacks an update over the pak folder (new files
//! renamed into place) and uninstalls by deleting it, and runs nothing of ours either way.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often the binary is looked at: one `stat`, so it costs nothing.
pub const INTERVAL: Duration = Duration::from_secs(3);

/// The file this process runs from, as it was when the process started.
#[derive(Debug, Clone)]
pub struct Binary {
    path: PathBuf,
    id: (u64, u64),
}

impl Binary {
    /// The running binary. `/proc/self/exe` names the file the process was started from
    /// even after it is replaced; its path is taken now, while it still names that file.
    pub fn current() -> Option<Self> {
        let path = std::env::current_exe().ok()?;
        let running = std::fs::metadata("/proc/self/exe").ok()?;
        let binary = Self {
            path,
            id: (running.dev(), running.ino()),
        };
        // Started from a file that was already replaced: the path is not ours to watch.
        binary.unchanged().then_some(binary)
    }

    /// The file at `path` as it is now.
    #[cfg(test)]
    pub fn at(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            path: path.to_owned(),
            id: (meta.dev(), meta.ino()),
        })
    }

    /// Where the binary was started from. A restart runs what is there now.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the path still names the same file. A new file renamed over it, or no file
    /// at all (the pak folder deleted), is a change.
    pub fn unchanged(&self) -> bool {
        std::fs::metadata(&self.path).is_ok_and(|m| (m.dev(), m.ino()) == self.id)
    }

    /// Returns once the binary has been replaced or removed.
    pub async fn changed(&self, every: Duration) {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if !self.unchanged() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_renamed_over_the_binary_is_a_change() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("goblind");
        std::fs::write(&path, "old").unwrap();
        let binary = Binary::at(&path).unwrap();
        assert!(binary.unchanged());
        // Rewritten in place keeps the file: that is not an update the store makes.
        std::fs::write(&path, "same file").unwrap();
        assert!(binary.unchanged());
        let new = tmp.path().join("goblind.tmp");
        std::fs::write(&new, "new").unwrap();
        std::fs::rename(&new, &path).unwrap();
        assert!(!binary.unchanged());
    }

    #[test]
    fn a_removed_binary_or_pak_folder_is_a_change() {
        let tmp = tempfile::tempdir().unwrap();
        let pak = tmp.path().join("Goblin Link.pak");
        std::fs::create_dir_all(pak.join("bin")).unwrap();
        let path = pak.join("bin/goblind");
        std::fs::write(&path, "old").unwrap();
        let binary = Binary::at(&path).unwrap();
        std::fs::remove_dir_all(&pak).unwrap();
        assert!(!binary.unchanged());
    }

    #[test]
    fn the_running_test_binary_is_unchanged() {
        let binary = Binary::current().expect("a test binary runs from a file");
        assert!(binary.unchanged());
    }

    #[tokio::test]
    async fn changed_returns_after_the_binary_goes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("goblind");
        std::fs::write(&path, "old").unwrap();
        let binary = Binary::at(&path).unwrap();
        let every = Duration::from_millis(10);
        let still = tokio::time::timeout(Duration::from_millis(100), binary.changed(every)).await;
        assert!(still.is_err(), "an unchanged binary is not reported");
        std::fs::remove_file(&path).unwrap();
        tokio::time::timeout(Duration::from_secs(5), binary.changed(every))
            .await
            .expect("a removed binary is reported");
    }
}
