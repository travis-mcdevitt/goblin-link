//! Media in replies, on the UI side: a small cache of decoded pictures, and a clip player.
//!
//! The daemon stores each image as a PNG and each clip as an MP4 with a poster PNG; the UI
//! only ever opens those local files. Clips are played by the device's `ffmpeg`, which
//! decodes to raw frames on a pipe while a second `ffmpeg` feeds the sound to `aplay` at the
//! system volume. Children are stopped only through their own handles.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use goblin_ui_kit::Image;

/// One picture or clip attached to a message.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Media {
    /// Local PNG to show (the image, or a clip's poster); `None` until the daemon has fetched it.
    pub picture: Option<String>,
    /// Local clip file, for video.
    pub video: Option<String>,
    /// Whether this is a video.
    pub is_video: bool,
    /// What it shows.
    pub label: String,
}

/// Decoded pictures by path. A file that fails to load is remembered as missing, so it is
/// not read again every frame.
///
/// In the app pictures are decoded on a worker thread ([`PictureCache::decode_in_background`]):
/// decoding a full-screen PNG takes the Brick a good part of a second, and done while drawing
/// it froze the screen each time a picture scrolled into view. Until a picture is ready
/// `get` answers `None` (the tile shows its placeholder) and [`PictureCache::poll`] says
/// when to draw again.
#[derive(Default)]
pub struct PictureCache {
    /// Each picture with the count of the `get` that last asked for it.
    pictures: HashMap<String, (Option<Image>, u64)>,
    /// Counts `get` calls, to tell which picture has gone unused longest.
    uses: u64,
    /// The worker thread, once decoding in the background is on.
    worker: Option<Worker>,
    /// Paths handed to the worker and not yet back.
    loading: std::collections::HashSet<String>,
    /// Width and height by path, read from the file's header alone: enough to lay a tile
    /// out without decoding a picture that is not on screen.
    sizes: HashMap<String, Option<(u32, u32)>>,
    /// Heights of chat messages as last drawn, by a key of their content and the layout
    /// width (see `screens::chat`): a message off screen is stepped over, not laid out.
    pub heights: HashMap<u64, f32>,
}

/// The decoding thread's two ends.
struct Worker {
    jobs: std::sync::mpsc::Sender<String>,
    done: std::sync::mpsc::Receiver<(String, Option<Image>)>,
}

/// Most pictures kept decoded (a 1024-pixel image is about 4 MB).
const MAX_PICTURES: usize = 12;

impl PictureCache {
    /// Decodes pictures on a worker thread from now on, instead of inside `get`.
    pub fn decode_in_background(&mut self) {
        if self.worker.is_some() {
            return;
        }
        let (jobs, todo) = std::sync::mpsc::channel::<String>();
        let (finished, done) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("pictures".into())
            .spawn(move || {
                for path in todo {
                    let img = Image::load_png(std::path::Path::new(&path)).ok();
                    if finished.send((path, img)).is_err() {
                        break;
                    }
                }
            });
        // Without a thread, decoding stays where it was.
        if spawned.is_ok() {
            self.worker = Some(Worker { jobs, done });
        }
    }

    /// Takes in the pictures the worker has finished; true if any arrived (draw again).
    pub fn poll(&mut self) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        let arrived: Vec<(String, Option<Image>)> = worker.done.try_iter().collect();
        let any = !arrived.is_empty();
        for (path, img) in arrived {
            // Forgotten while it was being decoded (its file changed): drop this result.
            if self.loading.remove(&path) {
                self.store(path, img);
            }
        }
        any
    }

    /// Whether a picture is still being decoded.
    pub fn loading(&self) -> bool {
        !self.loading.is_empty()
    }

    /// Keeps a decoded picture, dropping the one unused longest when the cache is full.
    fn store(&mut self, path: String, img: Option<Image>) {
        while self.pictures.len() >= MAX_PICTURES {
            let Some(oldest) = self
                .pictures
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.pictures.remove(&oldest);
        }
        self.uses += 1;
        self.pictures.insert(path, (img, self.uses));
    }

    /// The decoded picture at `path`, loading it on first use. With background decoding it
    /// is `None` until the picture is ready.
    pub fn get(&mut self, path: &str) -> Option<&Image> {
        if !self.pictures.contains_key(path) {
            if let Some(worker) = &self.worker {
                if !self.loading.contains(path) && worker.jobs.send(path.to_owned()).is_ok() {
                    self.loading.insert(path.to_owned());
                }
                return None;
            }
            let img = Image::load_png(std::path::Path::new(path)).ok();
            self.store(path.to_owned(), img);
        }
        self.uses += 1;
        let uses = self.uses;
        let entry = self.pictures.get_mut(path)?;
        entry.1 = uses;
        entry.0.as_ref()
    }

    /// Forgets a path (its file changed, e.g. a poster arrived).
    pub fn forget(&mut self, path: &str) {
        self.pictures.remove(path);
        self.loading.remove(path);
        self.sizes.remove(path);
    }

    /// The picture's width and height, from its PNG header (no decoding).
    pub fn size(&mut self, path: &str) -> Option<(u32, u32)> {
        if let Some(known) = self.sizes.get(path) {
            return *known;
        }
        let size = png_size(std::path::Path::new(path));
        // A file that is not there yet may arrive: only a real answer is remembered.
        if size.is_some() {
            self.sizes.insert(path.to_owned(), size);
        }
        size
    }
}

/// Width and height from a PNG file's header.
fn png_size(path: &std::path::Path) -> Option<(u32, u32)> {
    let mut head = [0u8; 24];
    std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    if &head[..8] != b"\x89PNG\r\n\x1a\n" || &head[12..16] != b"IHDR" {
        return None;
    }
    let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let (w, h) = (be(&head[16..20]), be(&head[20..24]));
    (w > 0 && h > 0).then_some((w, h))
}

/// Frame size and rate of clip playback: small enough for the Brick's CPU to decode and for
/// the pipe to carry (640×360 RGBA at 12 fps is about 11 MB/s); the viewer scales it up.
pub const CLIP_W: u32 = 640;
/// Frame height.
pub const CLIP_H: u32 = 360;
/// Frames per second.
pub const CLIP_FPS: u32 = 12;

/// Frames per second for this run: [`CLIP_FPS`], or `GOBLIN_UI_CLIP_FPS` (testing, 1-30).
fn clip_fps() -> u32 {
    std::env::var("GOBLIN_UI_CLIP_FPS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|f| (1..=30).contains(f))
        .unwrap_or(CLIP_FPS)
}

/// A clip being played.
pub struct ClipPlayer {
    video: Child,
    audio: Option<(Child, Child)>,
    frame: Arc<Mutex<Option<Vec<u32>>>>,
    done: Arc<Mutex<bool>>,
    /// Where playback started in the clip, seconds.
    from: f32,
    fps: u32,
    /// Frames decoded so far (the position is `from + frames / fps`).
    frames: Arc<std::sync::atomic::AtomicU32>,
}

/// A clip's length in seconds, read from the device's `ffmpeg` (`Duration: 00:00:10.03`).
/// Blocking, a fraction of a second; `None` if it cannot be told.
pub fn clip_seconds(path: &str) -> Option<f32> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-i", path])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    parse_duration(&String::from_utf8_lossy(&out.stderr))
}

/// The `Duration:` line of `ffmpeg -i` as seconds.
pub fn parse_duration(text: &str) -> Option<f32> {
    let at = text.find("Duration: ")? + "Duration: ".len();
    let stamp = text[at..].split([',', '\n']).next()?.trim();
    let mut parts = stamp.split(':');
    let h: f32 = parts.next()?.parse().ok()?;
    let m: f32 = parts.next()?.parse().ok()?;
    let s: f32 = parts.next()?.parse().ok()?;
    let total = h * 3600.0 + m * 60.0 + s;
    (parts.next().is_none() && total > 0.0).then_some(total)
}

impl ClipPlayer {
    /// Starts playing `path` from `from` seconds in. Frames are letterboxed into
    /// `CLIP_W`×`CLIP_H`.
    pub fn start(path: &str, from: f32) -> Result<ClipPlayer, String> {
        let fps = clip_fps();
        let from = from.max(0.0);
        let seek = format!("{from:.2}");
        // The sound starts first: it has the longer way to the speaker (a decoder, a pipe and
        // the player's buffer), so the picture does not run ahead of it.
        let audio = Self::start_sound(path, &seek);
        let vf = format!(
            "fps={fps},scale={CLIP_W}:{CLIP_H}:force_original_aspect_ratio=decrease,pad={CLIP_W}:{CLIP_H}:(ow-iw)/2:(oh-ih)/2"
        );
        let mut video = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-re", "-ss", &seek, "-i", path])
            .args([
                "-an", "-vf", &vf, "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start the player: {e}"))?;
        let mut out = video.stdout.take().ok_or("no video pipe")?;
        let frame = Arc::new(Mutex::new(None));
        let done = Arc::new(Mutex::new(false));
        let frames = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (f2, d2, n2) = (frame.clone(), done.clone(), frames.clone());
        std::thread::spawn(move || {
            let mut raw = vec![0u8; (CLIP_W * CLIP_H * 4) as usize];
            while out.read_exact(&mut raw).is_ok() {
                let px: Vec<u32> = raw
                    .chunks_exact(4)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], 255]))
                    .collect();
                *f2.lock().unwrap() = Some(px);
                n2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            *d2.lock().unwrap() = true;
        });
        Ok(ClipPlayer {
            video,
            audio,
            frame,
            done,
            from,
            fps,
            frames,
        })
    }

    /// Starts the sound of `path` from `seek` seconds in: the decoder and the player.
    fn start_sound(path: &str, seek: &str) -> Option<(Child, Child)> {
        // Sound: ffmpeg decodes to WAV, aplay plays it on the default device (system volume).
        // A clip with no sound simply ends this pair at once.
        // `GOBLIN_UI_CLIP_SILENT` (testing) plays the picture alone: the sound's decoder is
        // then a program that does not exist, so nothing is started.
        let silent = std::env::var_os("GOBLIN_UI_CLIP_SILENT").is_some();
        Command::new(if silent {
            "/nonexistent/ffmpeg"
        } else {
            "ffmpeg"
        })
        .args([
            "-loglevel",
            "error",
            "-ss",
            seek,
            "-i",
            path,
            "-vn",
            "-f",
            "wav",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut dec| {
            let pipe = dec.stdout.take()?;
            let play = Command::new("aplay")
                .args(["-q", "-"])
                .stdin(Stdio::from(pipe))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match play {
                Ok(p) => Some((dec, p)),
                Err(_) => {
                    let _ = dec.kill();
                    let _ = dec.wait();
                    None
                }
            }
        })
    }

    /// How far into the clip playback is, seconds.
    pub fn position(&self) -> f32 {
        let n = self.frames.load(std::sync::atomic::Ordering::Relaxed);
        self.from + n as f32 / self.fps as f32
    }

    /// The newest frame since the last call, if one arrived.
    pub fn take_frame(&mut self) -> Option<Image> {
        self.frame
            .lock()
            .unwrap()
            .take()
            .map(|px| Image::from_packed(CLIP_W, CLIP_H, px))
    }

    /// Whether the clip has played to its end.
    pub fn finished(&self) -> bool {
        *self.done.lock().unwrap()
    }
}

impl Drop for ClipPlayer {
    fn drop(&mut self) {
        let _ = self.video.kill();
        let _ = self.video.wait();
        if let Some((mut dec, mut play)) = self.audio.take() {
            let _ = play.kill();
            let _ = dec.kill();
            let _ = play.wait();
            let _ = dec.wait();
        }
    }
}

/// The agent's own avatar as the daemon prepared it (`<profile>/avatar/<revision>/`): a still
/// picture and, per state, a clip as raw RGBA frames.
pub struct AvatarAssets {
    /// The directory these came from.
    pub dir: String,
    /// The still picture.
    pub still: Option<Image>,
    size: u32,
    fps: u32,
    clips: HashMap<String, Vec<u32>>,
    /// The frame last handed out, so an unchanged frame is not rebuilt.
    current: Option<(String, usize, Image)>,
}

impl AvatarAssets {
    /// Loads the avatar from `dir`; `None` if it is not ready or unreadable.
    pub fn load(dir: &str) -> Option<AvatarAssets> {
        let root = std::path::Path::new(dir);
        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("meta.json")).ok()?).ok()?;
        let size = meta.get("size")?.as_u64()? as u32;
        let fps = (meta.get("fps")?.as_u64()? as u32).clamp(1, 30);
        if size == 0 || size > 512 {
            return None;
        }
        let frame_len = (size * size) as usize;
        let mut clips = HashMap::new();
        for name in meta
            .get("clips")
            .and_then(|c| c.as_object())
            .into_iter()
            .flatten()
            .map(|(k, _)| k)
        {
            let Ok(raw) = std::fs::read(root.join(format!("{name}.rgba"))) else {
                continue;
            };
            // At most ten seconds, whatever the file holds.
            let frames = (raw.len() / (frame_len * 4)).min((fps * 10) as usize);
            if frames == 0 {
                continue;
            }
            let px: Vec<u32> = raw[..frames * frame_len * 4]
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], 255]))
                .collect();
            clips.insert(name.clone(), px);
        }
        let still = Image::load_png(&root.join("avatar.png")).ok();
        if still.is_none() && clips.is_empty() {
            return None;
        }
        Some(AvatarAssets {
            dir: dir.to_owned(),
            still,
            size,
            fps,
            clips,
            current: None,
        })
    }

    /// Which frame of `state`'s clip shows at `now_ms`, or `None` when that clip is absent.
    pub fn frame_index(&self, state: &str, now_ms: u64) -> Option<usize> {
        let px = self.clips.get(state)?;
        let frames = px.len() / (self.size * self.size) as usize;
        (frames > 0).then(|| ((now_ms * self.fps as u64 / 1000) % frames as u64) as usize)
    }

    /// The frame of `state`'s clip at `now_ms` (falling back to the "idle" clip).
    pub fn frame(&mut self, state: &str, now_ms: u64) -> Option<&Image> {
        let state = if self.clips.contains_key(state) {
            state
        } else {
            "idle"
        };
        let index = self.frame_index(state, now_ms)?;
        let fresh = !matches!(&self.current, Some((s, i, _)) if s == state && *i == index);
        if fresh {
            let len = (self.size * self.size) as usize;
            let px = self.clips.get(state)?[index * len..(index + 1) * len].to_vec();
            self.current = Some((
                state.to_owned(),
                index,
                Image::from_packed(self.size, self.size, px),
            ));
        }
        self.current.as_ref().map(|(_, _, img)| img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small PNG on disk, with a name of its own.
    fn png(dir: &std::path::Path, name: &str) -> String {
        let path = dir.join(name);
        Image::from_packed(8, 6, vec![0xFF20_4060; 48])
            .save_png(&path)
            .expect("png");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn a_picture_decoded_in_the_background_arrives_by_poll() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = png(dir.path(), "a.png");
        let mut pics = PictureCache::default();
        pics.decode_in_background();
        assert!(pics.get(&path).is_none(), "not decoded while drawing");
        assert!(pics.loading());
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !pics.poll() {
            assert!(
                std::time::Instant::now() < until,
                "the picture never arrived"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!pics.loading());
        let img = pics.get(&path).expect("ready");
        assert_eq!((img.w, img.h), (8, 6));
        // A file that is not a picture is remembered as missing, not asked for again.
        let bad = dir.path().join("bad.png").to_string_lossy().into_owned();
        assert!(pics.get(&bad).is_none());
        while !pics.poll() {
            assert!(
                std::time::Instant::now() < until,
                "no answer for the bad file"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(pics.get(&bad).is_none());
        assert!(!pics.loading(), "a missing picture is not decoded again");
    }

    #[test]
    fn a_full_cache_drops_the_picture_unused_longest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths: Vec<String> = (0..=MAX_PICTURES)
            .map(|i| png(dir.path(), &format!("{i}.png")))
            .collect();
        let mut pics = PictureCache::default();
        for path in &paths[..MAX_PICTURES] {
            assert!(pics.get(path).is_some());
        }
        // The first is used again, so the second is now the one unused longest.
        assert!(pics.get(&paths[0]).is_some());
        assert!(pics.get(&paths[MAX_PICTURES]).is_some());
        assert_eq!(pics.pictures.len(), MAX_PICTURES);
        assert!(pics.pictures.contains_key(&paths[0]));
        assert!(!pics.pictures.contains_key(&paths[1]));
    }

    #[test]
    fn reads_the_length_ffmpeg_prints() {
        let text = "Input #0, mov,mp4, from 'x.mp4':\n  Duration: 00:01:10.03, start: 0.000000, bitrate: 2136 kb/s\n";
        assert!((parse_duration(text).unwrap() - 70.03).abs() < 0.01);
        assert_eq!(parse_duration("  Duration: N/A, bitrate: N/A"), None);
        assert_eq!(parse_duration("no such file"), None);
    }
}
