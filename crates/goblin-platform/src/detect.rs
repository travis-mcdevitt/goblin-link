//! Finding the platforms directory and choosing the platform that matches this device.

use std::path::{Path, PathBuf};

use crate::config::{DetectRules, PlatformConfig};
use crate::error::{PlatformError, Result};

/// Environment variable that forces a platform id, bypassing detection.
pub const FORCE_ENV: &str = "GOBLIN_PLATFORM";
/// Environment variable naming the platforms directory.
pub const PLATFORMS_DIR_ENV: &str = "GOBLIN_PLATFORMS_DIR";

/// What detection can observe about the running system.
pub trait SystemProbe {
    /// Value of an environment variable.
    fn env(&self, name: &str) -> Option<String>;
    /// Contents of `/proc/device-tree/model`, if present.
    fn device_tree_model(&self) -> Option<String>;
}

/// The real system.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostProbe;

impl SystemProbe for HostProbe {
    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn device_tree_model(&self) -> Option<String> {
        std::fs::read("/proc/device-tree/model").ok().map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .trim_matches('\0')
                .trim()
                .to_owned()
        })
    }
}

/// True when `rules` match the probed system on a non-fallback criterion.
pub fn rules_match(rules: &DetectRules, probe: &dyn SystemProbe) -> bool {
    let env_ok = !rules.env.is_empty()
        && rules
            .env
            .iter()
            .all(|(k, v)| probe.env(k).as_deref() == Some(v.as_str()));
    let model_ok = match (&rules.device_tree_model_contains, probe.device_tree_model()) {
        (Some(needle), Some(model)) => model.contains(needle.as_str()),
        _ => false,
    };
    env_ok || model_ok
}

/// Pick the matching platform among `configs`: a forced id, else the first non-fallback match
/// in id order, else the fallback platform.
pub fn choose<'a>(
    configs: &'a [PlatformConfig],
    probe: &dyn SystemProbe,
) -> Option<&'a PlatformConfig> {
    if let Some(forced) = probe.env(FORCE_ENV) {
        return configs.iter().find(|c| c.id == forced);
    }
    configs
        .iter()
        .find(|c| rules_match(&c.detect, probe))
        .or_else(|| configs.iter().find(|c| c.detect.fallback))
}

/// Read every `<dir>/<id>/platform.toml`, sorted by id.
pub fn load_all(platforms_dir: &Path) -> Result<Vec<(PathBuf, PlatformConfig)>> {
    let entries =
        std::fs::read_dir(platforms_dir).map_err(|e| PlatformError::io(platforms_dir, e))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| PlatformError::io(platforms_dir, e))?;
        let dir = entry.path();
        let file = dir.join("platform.toml");
        if !file.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&file).map_err(|e| PlatformError::io(&file, e))?;
        let config = PlatformConfig::parse(&text, &file)?;
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        if config.id != dir_name {
            return Err(PlatformError::Invalid(format!(
                "{}: id {:?} does not match directory name {dir_name:?}",
                file.display(),
                config.id
            )));
        }
        found.push((dir, config));
    }
    found.sort_by(|a, b| a.1.id.cmp(&b.1.id));
    Ok(found)
}

/// Locate the platforms directory: `GOBLIN_PLATFORMS_DIR`, then next to the executable
/// (`../platforms`, `../share/goblin-link/platforms`, `./platforms`), then `./platforms`.
pub fn locate_platforms_dir(probe: &dyn SystemProbe) -> Result<PathBuf> {
    if let Some(dir) = probe.env(PLATFORMS_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    let mut candidates = Vec::new();
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_owned))
    {
        candidates.push(exe_dir.join("../platforms"));
        candidates.push(exe_dir.join("../share/goblin-link/platforms"));
        candidates.push(exe_dir.join("platforms"));
    }
    candidates.push(PathBuf::from("platforms"));
    candidates
        .into_iter()
        .find(|c| c.is_dir())
        .ok_or(PlatformError::NoPlatformsDir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Fake {
        env: BTreeMap<String, String>,
        model: Option<String>,
    }

    impl SystemProbe for Fake {
        fn env(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }
        fn device_tree_model(&self) -> Option<String> {
            self.model.clone()
        }
    }

    fn cfg(id: &str, detect: &str) -> PlatformConfig {
        PlatformConfig::parse(
            &format!("id = \"{id}\"\n[detect]\n{detect}\n"),
            Path::new("t"),
        )
        .expect("parse")
    }

    fn fake(env: &[(&str, &str)], model: Option<&str>) -> Fake {
        Fake {
            env: env
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            model: model.map(Into::into),
        }
    }

    fn set() -> Vec<PlatformConfig> {
        vec![
            cfg(
                "brick",
                "env = { PLATFORM = \"tg5040\" }\ndevice_tree_model_contains = \"sun50iw10\"",
            ),
            cfg("generic", "fallback = true"),
        ]
    }

    #[test]
    fn matches_by_environment() {
        let configs = set();
        let p = fake(&[("PLATFORM", "tg5040")], None);
        assert_eq!(choose(&configs, &p).map(|c| c.id.as_str()), Some("brick"));
    }

    #[test]
    fn matches_by_device_tree_model() {
        let configs = set();
        let p = fake(&[], Some("sun50iw10 "));
        assert_eq!(choose(&configs, &p).map(|c| c.id.as_str()), Some("brick"));
    }

    #[test]
    fn wrong_env_value_falls_back() {
        let configs = set();
        let p = fake(&[("PLATFORM", "rg35xx")], Some("other"));
        assert_eq!(choose(&configs, &p).map(|c| c.id.as_str()), Some("generic"));
    }

    #[test]
    fn force_variable_wins_and_unknown_id_is_none() {
        let configs = set();
        let p = fake(&[(FORCE_ENV, "generic"), ("PLATFORM", "tg5040")], None);
        assert_eq!(choose(&configs, &p).map(|c| c.id.as_str()), Some("generic"));
        let p = fake(&[(FORCE_ENV, "nope")], None);
        assert!(choose(&configs, &p).is_none());
    }

    #[test]
    fn no_match_without_fallback() {
        let configs = vec![cfg("brick", "device_tree_model_contains = \"sun50iw10\"")];
        assert!(choose(&configs, &fake(&[], None)).is_none());
    }

    #[test]
    fn load_all_rejects_directory_name_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("wrong");
        std::fs::create_dir(&sub).expect("mkdir");
        std::fs::write(sub.join("platform.toml"), "id = \"right\"\n").expect("write");
        let err = load_all(dir.path()).expect_err("must fail");
        assert!(err.to_string().contains("does not match"));
    }

    #[test]
    fn load_all_sorts_and_skips_plain_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        for id in ["b", "a"] {
            let sub = dir.path().join(id);
            std::fs::create_dir(&sub).expect("mkdir");
            std::fs::write(sub.join("platform.toml"), format!("id = \"{id}\"\n")).expect("write");
        }
        std::fs::create_dir(dir.path().join("empty")).expect("mkdir");
        let ids: Vec<_> = load_all(dir.path())
            .expect("load")
            .into_iter()
            .map(|(_, c)| c.id)
            .collect();
        assert_eq!(ids, ["a", "b"]);
    }
}
