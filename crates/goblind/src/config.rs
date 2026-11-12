//! Resolving the data and runtime directories.

use std::path::PathBuf;

/// The daemon's directories.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Where profiles, settings and caches live.
    pub data_dir: PathBuf,
    /// Where the IPC socket lives.
    pub runtime_dir: PathBuf,
}

fn uid() -> u32 {
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

/// Resolves the directories from explicit arguments, else platform paths, else
/// the XDG defaults.
pub fn resolve(
    data_dir: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    platform: Option<&goblin_platform::Platform>,
) -> Paths {
    let data_dir = data_dir
        .or_else(|| std::env::var_os("GOBLIND_DATA_DIR").map(PathBuf::from))
        .or_else(|| {
            platform
                .and_then(|p| p.paths().data.clone())
                .map(|d| PathBuf::from(d).join("goblin-link"))
        })
        .or_else(|| std::env::var_os("XDG_DATA_HOME").map(|d| PathBuf::from(d).join("goblin-link")))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/goblin-link"))
        })
        .unwrap_or_else(|| PathBuf::from("/tmp/goblin-link"));

    let runtime_dir = runtime_dir
        .or_else(|| std::env::var_os("GOBLIND_RUNTIME_DIR").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("goblin-link"))
        })
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/goblin-link-{}", uid())));

    Paths {
        data_dir,
        runtime_dir,
    }
}
