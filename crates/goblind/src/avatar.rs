//! The agent's own avatar: `/identity` names a reference image and short
//! clips per state; the files come through `/fs/raw`. This module turns them into what the UI
//! can show cheaply: a PNG, and each clip as raw RGBA frames at a small fixed size and rate
//! (decoded once here by the device's `ffmpeg`, so the UI never decodes video).
//!
//! Files live in `<profile>/avatar/<assets_revision>/`, so a changed avatar is a new
//! directory and the old one can be removed:
//!   avatar.png             the reference image, square, [`FRAME_PX`] × 2 at most
//!   idle.rgba, working.rgba frames back to back, each [`FRAME_PX`]² × 4 bytes
//!   meta.json              {"size": 176, "fps": 10, "clips": {"idle": 50, "working": 50}}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// Frame edge in pixels: about the avatar's largest size on the Brick (during a call).
pub const FRAME_PX: u32 = 176;
/// Frames per second kept from a clip.
pub const FPS: u32 = 10;
/// Largest clip or image fetched.
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
/// The states the handheld shows, and the identity keys that feed them (first that exists).
pub const STATES: [(&str, &[&str]); 2] = [
    ("idle", &["idle", "connecting"]),
    (
        "working",
        &["working", "waiting_for_subagents", "making_something"],
    ),
];

/// What `/identity` says about the avatar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarInfo {
    /// Changes whenever the avatar's files do.
    pub revision: String,
    /// Workspace path of the reference image.
    pub image: Option<String>,
    /// Workspace path of the clip for each handheld state that has one.
    pub clips: BTreeMap<String, String>,
}

/// Reads the avatar block of an `/identity` response. `None` when there is no avatar or no
/// revision to key it by.
pub fn parse(identity: &Value) -> Option<AvatarInfo> {
    let a = identity.get("avatar")?;
    let revision: String = a
        .get("assets_revision")
        .and_then(Value::as_str)?
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect();
    if revision.is_empty() {
        return None;
    }
    let path = |v: Option<&Value>| -> Option<String> {
        let p = v?.as_str()?;
        let ok = !p.is_empty()
            && p.len() <= 512
            && !p.starts_with('/')
            && !p.contains("://")
            && p.split('/').all(|s| !s.is_empty() && s != "." && s != "..");
        ok.then(|| p.to_owned())
    };
    let image = if a.get("image_exists").and_then(Value::as_bool) == Some(false) {
        None
    } else {
        path(a.get("image_path"))
    };
    let mut clips = BTreeMap::new();
    if let Some(variants) = a.get("video_variants") {
        for (state, keys) in STATES {
            if let Some(p) = keys.iter().find_map(|k| path(variants.get(*k))) {
                clips.insert(state.to_owned(), p);
            }
        }
    }
    Some(AvatarInfo {
        revision,
        image,
        clips,
    })
}

/// The directory for a revision under the profile's avatar directory.
pub fn revision_dir(avatar_root: &Path, revision: &str) -> PathBuf {
    avatar_root.join(revision)
}

/// Whether a revision has been fully prepared.
pub fn ready(dir: &Path) -> bool {
    dir.join("meta.json").exists()
}

/// Writes the reference image as a square PNG. Blocking.
pub fn store_image(data: &[u8], dir: &Path) -> Result<(), String> {
    let img = image::load_from_memory(data).map_err(|e| format!("avatar image: {e}"))?;
    let side = img.width().min(img.height());
    // Centre crop to a square, towards the top where a character's face is.
    let x = (img.width() - side) / 2;
    let y = (img.height() - side) / 8;
    let img = img
        .crop_imm(x, y.min(img.height() - side), side, side)
        .resize(
            FRAME_PX * 2,
            FRAME_PX * 2,
            image::imageops::FilterType::Triangle,
        );
    let tmp = dir.join("avatar.png.tmp");
    img.to_rgba8()
        .save_with_format(&tmp, image::ImageFormat::Png)
        .map_err(|e| format!("avatar image: {e}"))?;
    std::fs::rename(&tmp, dir.join("avatar.png")).map_err(|e| format!("avatar image: {e}"))
}

/// Converts a clip to raw RGBA frames with `ffmpeg`; returns how many frames it wrote.
/// Blocking (a five-second clip takes a few seconds on the Brick).
pub fn store_clip(data: &[u8], dir: &Path, state: &str) -> Result<u32, String> {
    let src = dir.join(format!("{state}.mp4"));
    std::fs::write(&src, data).map_err(|e| format!("avatar clip: {e}"))?;
    let tmp = dir.join(format!("{state}.rgba.tmp"));
    let vf = format!(
        "fps={FPS},scale={FRAME_PX}:{FRAME_PX}:force_original_aspect_ratio=increase,crop={FRAME_PX}:{FRAME_PX}"
    );
    let status = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-i"])
        .arg(&src)
        .args(["-an", "-vf", &vf, "-f", "rawvideo", "-pix_fmt", "rgba"])
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("avatar clip: could not run ffmpeg: {e}"))?;
    let _ = std::fs::remove_file(&src);
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err("avatar clip: ffmpeg could not convert it".into());
    }
    let len = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    let frame = (FRAME_PX * FRAME_PX * 4) as u64;
    let frames = (len / frame) as u32;
    if frames == 0 {
        let _ = std::fs::remove_file(&tmp);
        return Err("avatar clip: no frames".into());
    }
    std::fs::rename(&tmp, dir.join(format!("{state}.rgba")))
        .map_err(|e| format!("avatar clip: {e}"))?;
    Ok(frames)
}

/// Writes `meta.json`, which marks the revision as ready.
pub fn write_meta(dir: &Path, clips: &BTreeMap<String, u32>) -> Result<(), String> {
    let meta = json!({"size": FRAME_PX, "fps": FPS, "clips": clips});
    let tmp = dir.join("meta.json.tmp");
    std::fs::write(&tmp, meta.to_string()).map_err(|e| format!("avatar meta: {e}"))?;
    std::fs::rename(&tmp, dir.join("meta.json")).map_err(|e| format!("avatar meta: {e}"))
}

/// Removes every revision directory but `keep`.
pub fn prune(avatar_root: &Path, keep: &str) {
    let Ok(rd) = std::fs::read_dir(avatar_root) else {
        return;
    };
    for e in rd.flatten() {
        if e.file_name().to_str() != Some(keep) && e.path().is_dir() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_identity_avatar() {
        let id = json!({"name": "Pip", "avatar": {
        "image_exists": true, "image_path": "assets/avatar/reference/hatch.jpg",
        "assets_revision": "r_70945eac19d3dc54",
        "video_variants": {
            "idle": "assets/avatar/reference/hatch.mp4",
            "working": "assets/avatar/reference/hatch_working.mp4",
            "waking": "assets/avatar/reference/hatch_waking.mp4"
        }}});
        let a = parse(&id).unwrap();
        assert_eq!(a.revision, "r_70945eac19d3dc54");
        assert_eq!(
            a.image.as_deref(),
            Some("assets/avatar/reference/hatch.jpg")
        );
        assert_eq!(a.clips["idle"], "assets/avatar/reference/hatch.mp4");
        assert_eq!(
            a.clips["working"],
            "assets/avatar/reference/hatch_working.mp4"
        );
        assert_eq!(a.clips.len(), 2, "only the states the handheld shows");
    }

    #[test]
    fn rejects_unsafe_paths_and_missing_revision() {
        let id = json!({"avatar": {"assets_revision": "../x y", "image_path": "/etc/passwd",
            "video_variants": {"idle": "a/../../b.mp4"}}});
        let a = parse(&id).unwrap();
        assert_eq!(a.revision, "xy", "revision reduced to safe characters");
        assert_eq!(a.image, None);
        assert!(a.clips.is_empty());
        assert_eq!(parse(&json!({"avatar": {"image_path": "a.jpg"}})), None);
        assert_eq!(parse(&json!({"name": "x"})), None);
    }

    #[test]
    fn stores_a_square_png_and_meta() {
        let dir = tempfile::tempdir().unwrap();
        let wide = image::RgbaImage::from_pixel(800, 600, image::Rgba([240, 230, 200, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(wide)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        store_image(&bytes, dir.path()).unwrap();
        let back = image::open(dir.path().join("avatar.png")).unwrap();
        assert_eq!(back.width(), back.height());
        assert!(!ready(dir.path()));
        write_meta(dir.path(), &BTreeMap::from([("idle".to_owned(), 50)])).unwrap();
        assert!(ready(dir.path()));
    }
}
