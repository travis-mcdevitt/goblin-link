//! Voice notes and spoken replies, using the platform's ALSA devices through `arecord`,
//! `ffmpeg` and `aplay` (all present on the Brick's firmware).
//!
//! Capture: `arecord` writes raw S16LE at 16 kHz with the platform's channel count; the channel
//! named by `audio.capture_use` becomes a mono WAV. The microphone gain is raised for the take
//! and restored by the platform's guard.
//!
//! Playback: the reply MP3 is decoded by `ffmpeg` into a WAV in memory and played by `aplay` on
//! the platform's playback PCM, at the **system volume**: Muse never changes the volume or the
//! speaker mute. The user sets the level with the volume keys, as for games (NextUI's `keymon`
//! applies them to the hardware). Callers skip playback when the system volume is 0.
//!
//! Children are signalled only through their own `Child` handle (one pid, never a group).

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use goblin_platform::config::CaptureUse;
use goblin_platform::{MicGainGuard, Platform};

/// Capture rate. Muse accepts any WAV; 16 kHz mono keeps a minute well under 2 MiB.
pub const RATE: u32 = 16_000;
/// Longest note: 60 s of 16 kHz mono is about 1.9 MiB, under Muse's 2 MiB limit.
pub const MAX_NOTE: Duration = Duration::from_secs(60);
/// Shorter takes are treated as accidental presses.
pub const MIN_NOTE: Duration = Duration::from_millis(500);
/// Microphone gain while recording (dB). 0.3: speech at +12 dB measured −30 to −46 dBFS.
pub const MIC_GAIN_DB: i32 = 12;
/// Normalisation target peak (fraction of full scale) and the most gain it may add.
const TARGET_PEAK: f32 = 0.5;
const MAX_GAIN: f32 = 8.0;

/// A take in progress.
pub struct Recording {
    child: Child,
    reader: Option<JoinHandle<Vec<u8>>>,
    /// The microphone's current level, 0..1 (f32 bits), updated about 20 times a second.
    pub level: Arc<std::sync::atomic::AtomicU32>,
    _gain: Option<MicGainGuard>,
    started: Instant,
    channels: usize,
    use_ch: CaptureUse,
    /// The chat the note goes to.
    pub chat: String,
}

/// Starts recording on the platform's capture device.
pub fn start_capture(platform: &Platform, chat: &str) -> Result<Recording, String> {
    let audio = platform
        .config()
        .audio
        .as_ref()
        .ok_or("this device has no audio")?;
    let device = audio
        .capture
        .clone()
        .ok_or("this device has no microphone")?;
    let channels = audio.capture_channels.clamp(1, 2) as usize;
    // Best effort: a platform without a gain hook records at its current gain.
    let gain = platform.set_mic_gain(MIC_GAIN_DB).ok();
    let secs = MAX_NOTE.as_secs() + 1;
    let mut child = Command::new("arecord")
        .args(["-q", "-D", &device, "-f", "S16_LE", "-t", "raw"])
        .args(["-r", &RATE.to_string(), "-c", &channels.to_string()])
        .args(["-d", &secs.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start the microphone: {e}"))?;
    let mut out = child.stdout.take().ok_or("no capture pipe")?;
    let level = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let meter = level.clone();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; 3200]; // 50 ms of 16 kHz stereo
        loop {
            match out.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Peak of this chunk as a 0..1 level (stored as bits of an f32).
                    let peak = chunk[..n & !1]
                        .chunks_exact(2)
                        .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
                        .max()
                        .unwrap_or(0);
                    let v = (peak as f32 / 32768.0).min(1.0);
                    meter.store(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
                    buf.extend_from_slice(&chunk[..n]);
                }
            }
        }
        buf
    });
    Ok(Recording {
        child,
        level,
        reader: Some(reader),
        _gain: gain,
        started: Instant::now(),
        channels,
        use_ch: audio.capture_use,
        chat: chat.to_owned(),
    })
}

impl Recording {
    /// Whether capture ended by itself or reached the note limit.
    pub fn finished(&mut self) -> bool {
        self.elapsed() >= MAX_NOTE
            || self
                .child
                .try_wait()
                .map_or(true, |status| status.is_some())
    }

    #[cfg(test)]
    pub(crate) fn finished_for_test() -> Self {
        let mut child = Command::new("/bin/true").spawn().unwrap();
        child.wait().unwrap();
        Self {
            child,
            reader: None,
            level: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            _gain: None,
            started: Instant::now(),
            channels: 1,
            use_ch: CaptureUse::Left,
            chat: "main".into(),
        }
    }

    /// How long the take has run.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Stops the take and returns it as a mono 16 kHz WAV (normalised), or `None` when it is
    /// too short. The microphone gain is restored when this returns.
    pub fn finish(mut self) -> Result<Option<Vec<u8>>, String> {
        let took = self.started.elapsed();
        // arecord stops on SIGKILL of its own pid; the pipe then closes and the reader ends.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let raw = self
            .reader
            .take()
            .and_then(|r| r.join().ok())
            .unwrap_or_default();
        if took < MIN_NOTE {
            return Ok(None);
        }
        let mono = select_channel(&raw, self.channels, self.use_ch);
        if mono.is_empty() {
            return Err("the microphone returned no audio".into());
        }
        Ok(Some(wav_bytes(&normalise(mono), RATE)))
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Picks one channel (or the mix) from interleaved S16LE.
pub fn select_channel(raw: &[u8], channels: usize, use_ch: CaptureUse) -> Vec<i16> {
    let frame = 2 * channels.max(1);
    raw.chunks_exact(frame)
        .map(|f| {
            let s = |i: usize| i16::from_le_bytes([f[2 * i], f[2 * i + 1]]);
            if channels < 2 {
                return s(0);
            }
            match use_ch {
                CaptureUse::Left => s(0),
                CaptureUse::Right => s(1),
                _ => ((s(0) as i32 + s(1) as i32) / 2) as i16,
            }
        })
        .collect()
}

/// Raises a quiet take so its peak reaches [`TARGET_PEAK`], adding at most [`MAX_GAIN`].
pub fn normalise(mut samples: Vec<i16>) -> Vec<i16> {
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as f32;
    if peak < 1.0 {
        return samples;
    }
    let gain = (TARGET_PEAK * 32767.0 / peak).min(MAX_GAIN);
    if gain <= 1.0 {
        return samples;
    }
    for s in &mut samples {
        *s = (*s as f32 * gain).round().clamp(-32768.0, 32767.0) as i16;
    }
    samples
}

/// A PCM16 mono WAV file.
pub fn wav_bytes(samples: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

/// The `aplay` currently speaking, so `voice.stop_speaking` can end it.
#[derive(Default)]
pub struct Speaker {
    generation: AtomicU64,
    current: Mutex<Option<Arc<Mutex<Child>>>>,
}

impl Speaker {
    /// Stops the reply being spoken, if any.
    pub fn stop(&self) {
        let mut current = self.current.lock().unwrap();
        self.generation.fetch_add(1, Ordering::Relaxed);
        if let Some(child) = current.take() {
            let _ = child.lock().unwrap().kill();
        }
    }

    /// Identifies speech requested since the last stop.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    fn start_playback(
        &self,
        generation: u64,
        spawn: impl FnOnce() -> Result<Child, String>,
    ) -> Result<Option<Arc<Mutex<Child>>>, String> {
        let mut current = self.current.lock().unwrap();
        if generation != self.generation() {
            return Ok(None);
        }
        let child = Arc::new(Mutex::new(spawn()?));
        *current = Some(child.clone());
        Ok(Some(child))
    }

    /// Decodes `mp3` and plays it at the system volume. Blocking; callers serialise and run
    /// it off the async threads.
    pub fn play_mp3(&self, platform: &Platform, mp3: &[u8], generation: u64) -> Result<(), String> {
        if generation != self.generation() {
            return Ok(());
        }
        let audio = platform
            .config()
            .audio
            .as_ref()
            .ok_or("this device has no audio")?;
        let pcm = audio.playback.clone().unwrap_or_else(|| "default".into());
        let wav = decode_mp3(mp3)?;
        // Timeout: the WAV's own length (48 kHz mono PCM16) plus a margin.
        let secs = wav.len() as u64 / (48_000 * 2) + 30;
        let Some(child) = self.start_playback(generation, || {
            Command::new("aplay")
                .args(["-q", "-D", &pcm, "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("could not start playback: {e}"))
        })?
        else {
            return Ok(());
        };
        let mut stdin = child.lock().unwrap().stdin.take();
        let writer = std::thread::spawn(move || {
            if let Some(stdin) = stdin.as_mut() {
                let _ = stdin.write_all(&wav);
            }
            drop(stdin);
        });
        // Wait for aplay; poll so `stop` can kill it meanwhile.
        let limit = Instant::now() + Duration::from_secs(secs);
        let finished = loop {
            if let Ok(Some(_)) = child.lock().unwrap().try_wait() {
                break true;
            }
            if Instant::now() > limit {
                let _ = child.lock().unwrap().kill();
                break false;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let _ = writer.join();
        self.current.lock().unwrap().take();
        if finished {
            Ok(())
        } else {
            Err("playback did not finish in time".into())
        }
    }
}

/// MP3 → 48 kHz mono PCM16 WAV with `ffmpeg`.
fn decode_mp3(mp3: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-i", "pipe:0"])
        .args([
            "-f",
            "wav",
            "-acodec",
            "pcm_s16le",
            "-ac",
            "1",
            "-ar",
            "48000",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start the decoder: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no decoder pipe")?;
    let input = mp3.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child
        .wait_with_output()
        .map_err(|e| format!("decoder failed: {e}"))?;
    let _ = writer.join();
    if !out.status.success() || out.stdout.len() <= 44 {
        return Err("could not decode the reply audio".into());
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_speech_cannot_start_a_late_player() {
        let speaker = Speaker::default();
        let generation = speaker.generation();
        speaker.stop();
        assert!(speaker
            .start_playback(generation, || panic!("cancelled speech must not spawn"))
            .unwrap()
            .is_none());
        assert!(speaker
            .start_playback(
                speaker.generation(),
                || Err("new playback attempted".into())
            )
            .is_err());
    }

    #[test]
    fn left_channel_of_stereo() {
        let raw: Vec<u8> = [1i16, 100, 2, 200, 3, 300]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        assert_eq!(select_channel(&raw, 2, CaptureUse::Left), vec![1, 2, 3]);
        assert_eq!(
            select_channel(&raw, 2, CaptureUse::Right),
            vec![100, 200, 300]
        );
    }

    #[test]
    fn normalise_raises_quiet_takes_with_a_cap() {
        let quiet = normalise(vec![0, 1000, -1000]);
        assert_eq!(quiet[1], 8000, "capped at 8x");
        let mid = normalise(vec![0, 8192, -8192]);
        assert_eq!(mid[1], 16384, "brought to half scale");
        let loud = normalise(vec![0, 30000]);
        assert_eq!(loud[1], 30000, "never lowered");
        assert_eq!(normalise(vec![0, 0]), vec![0, 0]);
    }

    #[test]
    fn wav_header() {
        let w = wav_bytes(&[0, 1, -1], 16_000);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes([w[24], w[25], w[26], w[27]]), 16_000);
        assert_eq!(u32::from_le_bytes([w[40], w[41], w[42], w[43]]), 6);
        assert_eq!(w.len(), 50);
    }
}
