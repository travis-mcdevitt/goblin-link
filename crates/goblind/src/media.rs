//! Images and video in replies.
//!
//! Muse sends media as a `delta.presentation` event (kind `image` or `video`) whose payload
//! lists workspace files. This module turns such an event into a chat
//! message with attachments, downloads the files through `/fs/raw`, and prepares what the UI
//! shows: a PNG thumbnail per image (decoded here, since the UI only reads PNG) and, for a
//! clip, the MP4 plus a poster frame made by the device's `ffmpeg`.
//!
//! Files live in `<profile>/media/`, named by a hash of their workspace path, so a file is
//! fetched once. Sizes are bounded; anything larger is left as a named placeholder. The
//! folder is bounded too: [`prune`] drops the files unused for longest once it passes
//! [`MAX_STORE_BYTES`]; a message whose file is gone shows its named placeholder again.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use serde_json::Value;
use sha2::{Digest, Sha256};

use goblin_ipc::types::{Attachment, AttachmentKind};

/// Largest image fetched (Muse's generated images are well under 1 MiB).
pub const MAX_IMAGE_BYTES: usize = 12 * 1024 * 1024;
/// Largest clip fetched.
pub const MAX_VIDEO_BYTES: usize = 48 * 1024 * 1024;
/// Most the media folder may hold before the files unused for longest are dropped.
pub const MAX_STORE_BYTES: u64 = 256 * 1024 * 1024;
/// Files used within this long are never dropped, so the chat on screen keeps its media.
pub const KEEP_RECENT: Duration = Duration::from_secs(24 * 3600);
/// Longest side of a stored image: enough to fill the Brick's 1024×768 screen.
const VIEW_PX: u32 = 1024;

/// One media file named by a presentation event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRef {
    /// Image or video.
    pub kind: AttachmentKind,
    /// Workspace path as `/fs/raw` takes it (`workspace/imagine_media/x.webp`).
    pub path: String,
    /// What it shows.
    pub label: String,
    /// MIME type.
    pub mime: String,
    /// Size in bytes, when stated.
    pub bytes: Option<u64>,
}

/// The media in a `delta.presentation` event, in order (empty for other events and kinds).
pub fn refs_from_event(ev: &Value) -> Vec<MediaRef> {
    if ev.get("event_name").and_then(Value::as_str) != Some("delta.presentation") {
        return Vec::new();
    }
    let Some(payload) = ev.get("payload") else {
        return Vec::new();
    };
    let (kind, key) = match payload.get("kind").and_then(Value::as_str) {
        Some("image") => (AttachmentKind::Image, "images"),
        Some("video") => (AttachmentKind::Video, "videos"),
        _ => return Vec::new(),
    };
    let items = payload
        .get("data")
        .and_then(|d| d.get(key))
        .and_then(Value::as_array);
    let mut out = Vec::new();
    for item in items.into_iter().flatten() {
        if item.get("missing").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(raw) = item.get("path").and_then(Value::as_str) else {
            continue;
        };
        let Some(path) = workspace_path(raw) else {
            continue;
        };
        out.push(MediaRef {
            kind,
            path,
            label: item
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(120)
                .collect(),
            mime: item
                .get("mime")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            bytes: item.get("byte_len").and_then(Value::as_u64),
        });
    }
    out
}

/// `sandbox://workspace/a/b.webp` → `workspace/a/b.webp`. Only plain relative paths under
/// `workspace/` are accepted: no `..`, no empty parts, no other scheme.
pub fn workspace_path(raw: &str) -> Option<String> {
    let p = raw.strip_prefix("sandbox://").unwrap_or(raw);
    if p.contains("://") || p.starts_with('/') || p.len() > 512 {
        return None;
    }
    if !p.starts_with("workspace/") {
        return None;
    }
    if p.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
        return None;
    }
    Some(p.to_owned())
}

/// A stable file stem for a workspace path.
pub fn stem(path: &str) -> String {
    let digest = Sha256::digest(path.as_bytes());
    digest.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

/// Where a reference's files live in `dir`: the picture the UI shows (the image, or a clip's
/// poster), and the clip itself for video.
pub fn local_files(dir: &Path, r: &MediaRef) -> (PathBuf, Option<PathBuf>) {
    let s = stem(&r.path);
    match r.kind {
        AttachmentKind::Video => (
            dir.join(format!("{s}.poster.png")),
            Some(dir.join(format!("{s}.mp4"))),
        ),
        _ => (dir.join(format!("{s}.png")), None),
    }
}

/// The attachment for a reference: `path` is the local picture once it exists, `url` the
/// local clip for video.
pub fn attachment(dir: &Path, r: &MediaRef) -> Attachment {
    let (picture, clip) = local_files(dir, r);
    Attachment {
        kind: r.kind,
        name: if r.label.is_empty() {
            match r.kind {
                AttachmentKind::Video => "Video".into(),
                _ => "Image".into(),
            }
        } else {
            r.label.clone()
        },
        path: picture
            .exists()
            .then(|| picture.to_string_lossy().into_owned()),
        url: clip
            .filter(|c| c.exists())
            .map(|c| c.to_string_lossy().into_owned()),
        bytes: r.bytes,
        mime: (!r.mime.is_empty()).then(|| r.mime.clone()),
        source: Some(r.path.clone()),
    }
}

/// The media of a stored message whose files are not in `dir` (never fetched, or dropped by
/// [`prune`]), as references to fetch again. Empty when everything is there.
pub fn missing(dir: &Path, attachments: &[Attachment]) -> Vec<MediaRef> {
    let refs: Vec<MediaRef> = attachments
        .iter()
        .filter(|a| matches!(a.kind, AttachmentKind::Image | AttachmentKind::Video))
        .filter_map(|a| {
            Some(MediaRef {
                kind: a.kind,
                path: workspace_path(a.source.as_deref()?)?,
                label: a.name.clone(),
                mime: a.mime.clone().unwrap_or_default(),
                bytes: a.bytes,
            })
        })
        .collect();
    let any_missing = refs.iter().any(|r| {
        let (picture, clip) = local_files(dir, r);
        !clip.unwrap_or(picture).exists()
    });
    if any_missing {
        refs
    } else {
        Vec::new()
    }
}

/// Decodes an image (WebP, JPEG or PNG) and writes it as a PNG no larger than the screen.
/// Blocking: call off the async threads.
pub fn store_image(data: &[u8], out: &Path) -> Result<(), String> {
    let img = image::load_from_memory(data).map_err(|e| format!("could not decode image: {e}"))?;
    let img = if img.width().max(img.height()) > VIEW_PX {
        img.resize(VIEW_PX, VIEW_PX, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let tmp = out.with_extension("png.tmp");
    img.to_rgba8()
        .save_with_format(&tmp, image::ImageFormat::Png)
        .map_err(|e| format!("could not write image: {e}"))?;
    std::fs::rename(&tmp, out).map_err(|e| format!("could not store image: {e}"))
}

/// Stores a clip and makes its poster frame. Blocking. The poster is best effort: without it
/// the UI shows a plain tile with the play mark.
pub fn store_video(data: &[u8], clip: &Path, poster: &Path) -> Result<(), String> {
    let tmp = clip.with_extension("mp4.tmp");
    std::fs::write(&tmp, data).map_err(|e| format!("could not store clip: {e}"))?;
    std::fs::rename(&tmp, clip).map_err(|e| format!("could not store clip: {e}"))?;
    if let Err(e) = make_poster(clip, poster) {
        log::info!("no poster frame for a clip: {e}");
    }
    Ok(())
}

/// Makes a clip's poster: the device's `ffmpeg` writes the first frame as a JPEG (the Brick's
/// build has no PNG encoder), which is then stored as the PNG the UI reads. Blocking.
pub fn make_poster(clip: &Path, poster: &Path) -> Result<(), String> {
    let jpg = poster.with_extension("jpg.tmp");
    let made = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-i"])
        .arg(clip)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale=640:-2",
            "-f",
            "image2",
            "-c:v",
            "mjpeg",
        ])
        .args(["-q:v", "3"])
        .arg(&jpg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let result = match made {
        Ok(s) if s.success() => std::fs::read(&jpg)
            .map_err(|e| format!("could not read the frame: {e}"))
            .and_then(|data| store_image(&data, poster)),
        Ok(_) => Err("ffmpeg could not read the clip".into()),
        Err(e) => Err(format!("could not run ffmpeg: {e}")),
    };
    let _ = std::fs::remove_file(&jpg);
    result
}

/// Files being fetched right now, so two tasks never fetch (and write) the same one.
static IN_FLIGHT: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// The right to fetch one file; released when dropped.
pub struct Claim(PathBuf);

/// Claims `file` for fetching, or `None` when another task is already fetching it.
pub fn claim(file: &Path) -> Option<Claim> {
    let mut busy = IN_FLIGHT.lock().unwrap();
    if busy.iter().any(|p| p == file) {
        return None;
    }
    busy.push(file.to_owned());
    Some(Claim(file.to_owned()))
}

impl Drop for Claim {
    fn drop(&mut self) {
        IN_FLIGHT.lock().unwrap().retain(|p| p != &self.0);
    }
}

/// Marks a stored file as used now, so [`prune`] keeps what is still being looked at.
pub fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().append(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// Keeps `dir` within `cap` bytes by removing the files unused for longest, never one used
/// within `keep_recent`. Returns how many bytes were freed. Leftover `.tmp` files from an
/// interrupted store go first, whatever the total.
pub fn prune(dir: &Path, cap: u64, keep_recent: Duration) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut freed = 0;
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let used = meta.modified().unwrap_or(now);
        let old = now.duration_since(used).unwrap_or_default() >= keep_recent;
        let path = e.path();
        if old && path.extension().is_some_and(|x| x == "tmp") {
            if std::fs::remove_file(&path).is_ok() {
                freed += meta.len();
            }
            continue;
        }
        files.push((used, meta.len(), path));
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (used, len, path) in files {
        if total <= cap || now.duration_since(used).unwrap_or_default() < keep_recent {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
            freed += len;
        }
    }
    freed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn image_and_video_events() {
        let ev = json!({"event_name": "delta.presentation", "payload": {"kind": "image", "data": {
        "images": [
            {"path": "sandbox://workspace/imagine_media/a.webp", "mime": "image/webp",
             "byte_len": 23854, "label": "red circle", "missing": false},
            {"path": "sandbox://workspace/imagine_media/gone.webp", "missing": true},
            {"path": "https://example.com/x.png"}
        ]}}});
        let refs = refs_from_event(&ev);
        assert_eq!(refs.len(), 1, "missing and non-workspace items are skipped");
        assert_eq!(refs[0].path, "workspace/imagine_media/a.webp");
        assert_eq!(refs[0].kind, AttachmentKind::Image);
        assert_eq!(refs[0].bytes, Some(23854));

        let v = json!({"event_name": "delta.presentation", "payload": {"kind": "video", "data": {
            "videos": [{"path": "sandbox://workspace/imagine_media/b.mp4", "mime": "video/mp4"}]}}});
        assert_eq!(refs_from_event(&v)[0].kind, AttachmentKind::Video);
        assert!(refs_from_event(&json!({"event_name": "message.assistant"})).is_empty());
    }

    #[test]
    fn only_plain_workspace_paths() {
        assert_eq!(
            workspace_path("sandbox://workspace/a/b.png").as_deref(),
            Some("workspace/a/b.png")
        );
        assert_eq!(
            workspace_path("workspace/a.png").as_deref(),
            Some("workspace/a.png")
        );
        assert_eq!(workspace_path("sandbox://workspace/../etc/passwd"), None);
        assert_eq!(workspace_path("sandbox://other/a.png"), None);
        assert_eq!(workspace_path("/etc/passwd"), None);
        assert_eq!(workspace_path("workspace//a.png"), None);
    }

    #[test]
    fn stores_a_png_no_larger_than_the_screen() {
        let dir = tempfile::tempdir().unwrap();
        let big = image::RgbaImage::from_pixel(2000, 1000, image::Rgba([200, 30, 30, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(big)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        let out = dir.path().join("x.png");
        store_image(&bytes, &out).unwrap();
        let back = image::open(&out).unwrap();
        assert_eq!((back.width(), back.height()), (1024, 512));
        assert!(store_image(b"not an image", &dir.path().join("y.png")).is_err());
    }

    #[test]
    fn a_stored_message_names_its_missing_media() {
        let dir = tempfile::tempdir().unwrap();
        let r = MediaRef {
            kind: AttachmentKind::Image,
            path: "workspace/a.webp".into(),
            label: "A".into(),
            mime: "image/webp".into(),
            bytes: Some(3),
        };
        let stored = vec![attachment(dir.path(), &r)];
        assert_eq!(missing(dir.path(), &stored), vec![r.clone()]);
        std::fs::write(local_files(dir.path(), &r).0, b"x").unwrap();
        assert!(missing(dir.path(), &stored).is_empty());
        // Without a source (stored by an earlier version) there is nothing to fetch.
        let mut old = stored.clone();
        old[0].source = None;
        std::fs::remove_file(local_files(dir.path(), &r).0).unwrap();
        assert!(missing(dir.path(), &old).is_empty());
    }

    #[test]
    fn a_file_is_claimed_by_one_task_at_a_time() {
        let f = Path::new("/m/claimed.png");
        let first = claim(f).expect("free");
        assert!(claim(f).is_none());
        assert!(claim(Path::new("/m/other.png")).is_some());
        drop(first);
        assert!(claim(f).is_some());
    }

    #[test]
    fn local_names_are_stable_and_separate() {
        let dir = Path::new("/m");
        let img = MediaRef {
            kind: AttachmentKind::Image,
            path: "workspace/a.webp".into(),
            label: String::new(),
            mime: String::new(),
            bytes: None,
        };
        let vid = MediaRef {
            kind: AttachmentKind::Video,
            path: "workspace/b.mp4".into(),
            ..img.clone()
        };
        let (p1, c1) = local_files(dir, &img);
        let (p2, c2) = local_files(dir, &vid);
        assert!(p1.to_string_lossy().ends_with(".png") && c1.is_none());
        assert!(p2.to_string_lossy().ends_with(".poster.png"));
        assert!(c2.unwrap().to_string_lossy().ends_with(".mp4"));
        assert_ne!(stem("workspace/a.webp"), stem("workspace/b.mp4"));
    }

    #[test]
    fn prune_drops_the_longest_unused_first_and_spares_recent_files() {
        let dir = tempfile::tempdir().unwrap();
        let age = |name: &str, hours: u64| {
            let p = dir.path().join(name);
            std::fs::write(&p, vec![0u8; 1000]).unwrap();
            let f = std::fs::File::options().append(true).open(&p).unwrap();
            f.set_modified(SystemTime::now() - Duration::from_secs(hours * 3600))
                .unwrap();
            p
        };
        let oldest = age("a.png", 72);
        let old = age("b.mp4", 48);
        let recent = age("c.png", 1);
        let stale_tmp = age("d.png.tmp", 30);
        let day = Duration::from_secs(24 * 3600);
        // Under the cap: only the stale temporary file goes.
        assert_eq!(prune(dir.path(), 10_000, day), 1000);
        assert!(!stale_tmp.exists() && oldest.exists());
        // Over it: the oldest goes, and that is enough.
        assert_eq!(prune(dir.path(), 2500, day), 1000);
        assert!(!oldest.exists() && old.exists() && recent.exists());
        // A recent file stays even when the cap cannot be met.
        assert_eq!(prune(dir.path(), 0, day), 1000);
        assert!(!old.exists() && recent.exists());
        // Using a file makes it recent.
        let again = age("e.png", 100);
        touch(&again);
        assert_eq!(prune(dir.path(), 0, day), 0);
        assert!(again.exists());
    }
}
