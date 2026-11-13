//! Realtime voice calls (`POST /voice/realtime`), ported from the SDK's `voice_call.py` and
//! `voice_gate.py`.
//!
//! One duplex request carries NDJSON both ways. Up: `hatch.voice.call.start`, the
//! `end_call_stop` capability offer, then `input_audio_buffer.append` frames of PCM16LE mono
//! 24 kHz (960 samples). Down: `hatch.voice.session` (ready), input holds, turn and response
//! events and audio deltas. A `session_control` "stop" item (Muse heard a goodbye) ends the
//! call after the last speech has played. Every call ends with `POST /voice/end-call`.
//!
//! Half duplex by default: the microphone is gated while Muse's audible speech plays (the
//! Brick's speaker reaches its microphone) and for a short settle after. Barge-in is a button:
//! it fences the reply's audio, flushes playback and cancels the open response.
//!
//! Audio uses the platform's devices through `arecord` (capture, raw) and `aplay` (raw, at the
//! system volume; Muse never changes volume or mute). Children are signalled only through their
//! own `Child` handles.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use goblin_platform::Platform;
use muse_link::{DuplexStream, LinkSession};

/// PCM format of the call.
pub const SAMPLE_RATE: u32 = 24_000;
/// Samples per input frame (40 ms).
pub const FRAME_SAMPLES: usize = 960;
/// Bytes per input frame.
pub const FRAME_BYTES: usize = FRAME_SAMPLES * 2;
/// Largest audio delta accepted (4 s), and the most reply audio buffered while unconfirmed.
const MAX_AUDIO_BYTES: usize = SAMPLE_RATE as usize * 2 * 4;
const MAX_WAITING_BYTES: usize = SAMPLE_RATE as usize * 2 * 5;
/// After audible speech, keep the microphone closed this long (echo tail).
const SETTLE: Duration = Duration::from_millis(300);
/// Frames quieter than this are not "audible" speech (as in the reference).
const AUDIBLE_RMS: f64 = 300.0;
/// Microphone gain during a call (dB), as for voice notes.
const MIC_GAIN_DB: i32 = 12;
/// Timeouts.
const READY_TIMEOUT: Duration = Duration::from_secs(35);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const OVERALL_TIMEOUT: Duration = Duration::from_secs(600);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest NDJSON line accepted.
const MAX_LINE_BYTES: usize = 1024 * 1024;
const END_CALL_STOP_OFFER: &str = r#"{"type":"hatch.voice.capability.offer","end_call_stop":1}"#;

/// What the owner asks of a running call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallCmd {
    /// Hang up.
    End,
    /// Interrupt Muse's reply.
    BargeIn,
    /// Mute or unmute the microphone (silence keeps flowing so endpointing still works).
    Mute(bool),
}

/// Coarse state for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Starting the call.
    Connecting,
    /// The microphone is open.
    Listening,
    /// Muse is working on what was said.
    Thinking,
    /// Muse is speaking.
    Speaking,
}

/// Reports from the call to the daemon.
pub trait CallObserver: Send + Sync {
    /// The call's state changed.
    fn phase(&self, phase: Phase, muted: bool, seconds: f64);
    /// Text of what is being said (`assistant`: Muse; otherwise the owner), growing until
    /// `done`.
    fn caption(&self, _assistant: bool, _text: &str, _done: bool) {}
}

/// Text carried by a transcript event, if `ev` is one: (Muse spoke, text, is a delta, final).
/// The names follow the realtime family this stream already uses for audio
/// (`response.output_audio.delta`); which of them Muse sends is not yet observed.
pub fn transcript_from_event(ev: &Value) -> Option<(bool, String, bool, bool)> {
    let kind = ev.get("type").and_then(Value::as_str)?;
    let text = |k: &str| ev.get(k).and_then(Value::as_str).map(str::to_owned);
    match kind {
        "response.output_audio_transcript.delta" | "response.audio_transcript.delta" => {
            Some((true, text("delta")?, true, false))
        }
        "response.output_audio_transcript.done" | "response.audio_transcript.done" => {
            Some((true, text("transcript")?, false, true))
        }
        "conversation.item.input_audio_transcription.delta" => {
            Some((false, text("delta")?, true, false))
        }
        "conversation.item.input_audio_transcription.completed" => {
            Some((false, text("transcript")?, false, true))
        }
        _ => None,
    }
}

// ---- reply gate (voice_gate.py) ----------------------------------------------------------------

const MAX_IDS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RState {
    Stale,
    Waiting,
    Playing,
    Continuing,
}

/// What to do with one audio delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Discard it.
    Drop,
    /// Keep it until the caller's turn is confirmed.
    Hold,
    /// Play it.
    Play,
}

/// Fences provider audio after an interruption until the caller's next turn is confirmed.
#[derive(Debug, Default)]
pub struct ReplyGate {
    ids: HashMap<String, RState>,
    items: Vec<String>,
    fenced: bool,
    item: Option<String>,
    confirmed: bool,
    blocked: bool,
    turn_fresh: bool,
    flush_playing: bool,
    flush_waiting: bool,
    /// The response currently open (for `response.cancel`).
    pub open_response: Option<String>,
}

/// A usable identifier: a non-empty string of at most 128 bytes without NUL.
pub fn identifier(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    (!s.is_empty() && !s.contains('\0') && s.len() <= 128).then(|| s.to_owned())
}

impl ReplyGate {
    /// Fences current replies after the caller (or a button) interrupts.
    pub fn interrupt(&mut self, item: Option<String>) {
        self.fenced = true;
        for v in self.ids.values_mut() {
            *v = RState::Stale;
        }
        self.item = item;
        self.confirmed = false;
        self.blocked = false;
        self.turn_fresh = false;
        self.open_response = None;
        self.flush_playing = true;
        self.flush_waiting = true;
    }

    fn replace(&mut self, from: &[RState], to: RState) {
        for v in self.ids.values_mut() {
            if from.contains(v) {
                *v = to;
            }
        }
    }

    fn has(&self, states: &[RState]) -> bool {
        self.ids.values().any(|v| states.contains(v))
    }

    /// Whether reply audio is held for confirmation.
    pub fn waiting(&self) -> bool {
        self.has(&[RState::Waiting])
    }

    /// Whether reply audio is playing.
    pub fn playing(&self) -> bool {
        self.has(&[RState::Playing, RState::Continuing])
    }

    /// (flush playing audio, drop held audio), cleared on read.
    pub fn take_flush(&mut self) -> (bool, bool) {
        let f = (self.flush_playing, self.flush_waiting);
        self.flush_playing = false;
        self.flush_waiting = false;
        f
    }

    /// Applies a non-audio event.
    pub fn event(&mut self, ev: &Value) -> Result<(), String> {
        let kind = ev.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "input_audio_buffer.speech_started" => {
                let item = identifier(ev.get("item_id"));
                if let Some(item) = &item {
                    if self.items.contains(item) {
                        return Ok(());
                    }
                    if self.items.len() == MAX_IDS {
                        return Err("too many caller input identities".into());
                    }
                    self.items.push(item.clone());
                }
                self.interrupt(item);
            }
            "hatch.voice.turn_started" => {
                let item = identifier(ev.get("item_id"));
                let (Some(mine), Some(item)) = (self.item.clone(), item) else {
                    return Ok(());
                };
                if identifier(ev.get("submission_message_id")).is_none() {
                    return Ok(());
                }
                if item != mine {
                    self.flush_waiting |= self.waiting();
                    self.flush_playing |= self.has(&[RState::Continuing]);
                    self.replace(&[RState::Waiting, RState::Continuing], RState::Stale);
                    self.blocked = true;
                    self.turn_fresh = false;
                    return Ok(());
                }
                let had_reply = self.waiting() || self.playing();
                self.replace(&[RState::Waiting, RState::Continuing], RState::Playing);
                self.confirmed = true;
                self.blocked = false;
                self.turn_fresh = !had_reply;
            }
            "hatch.voice.input_suppressed" => {
                if self.item.is_some() && identifier(ev.get("item_id")) == self.item {
                    self.interrupt(None);
                }
            }
            "response.created" | "response.done" | "response.completed" => {
                let id = ev.get("response").and_then(|r| identifier(r.get("id")));
                if let Some(id) = id {
                    if kind == "response.created" {
                        self.open_response = Some(id);
                    } else if self.open_response.as_deref() == Some(id.as_str()) {
                        self.open_response = None;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Decides what to do with an audio delta.
    pub fn audio(&mut self, ev: &Value) -> Result<Verdict, String> {
        let response_id =
            if ev.get("type").and_then(Value::as_str) == Some("continuous_audio_video.delta") {
                identifier(ev.get("response_id"))
            } else {
                None
            };
        let Some(rid) = response_id else {
            return Ok(if self.fenced && (!self.confirmed || self.blocked) {
                Verdict::Drop
            } else {
                Verdict::Play
            });
        };
        if let Some(state) = self.ids.get(&rid) {
            return Ok(verdict(*state));
        }
        if self.ids.len() == MAX_IDS {
            return Err("too many provider response identities".into());
        }
        let state = if !self.fenced {
            RState::Playing
        } else if self.item.is_none() || self.blocked {
            RState::Stale
        } else if self.confirmed {
            self.flush_playing |= self.playing();
            self.replace(&[RState::Playing, RState::Continuing], RState::Stale);
            let s = if self.turn_fresh {
                RState::Playing
            } else {
                RState::Continuing
            };
            self.turn_fresh = false;
            s
        } else {
            self.flush_waiting |= self.waiting();
            self.replace(&[RState::Waiting], RState::Stale);
            RState::Waiting
        };
        self.ids.insert(rid, state);
        Ok(verdict(state))
    }
}

fn verdict(s: RState) -> Verdict {
    match s {
        RState::Waiting => Verdict::Hold,
        RState::Stale => Verdict::Drop,
        _ => Verdict::Play,
    }
}

// ---- event helpers -----------------------------------------------------------------------------

/// The PCM in an audio event, `None` for other events.
pub fn audio_from_event(ev: &Value) -> Result<Option<Vec<u8>>, String> {
    let kind = ev.get("type").and_then(Value::as_str).unwrap_or("");
    let encoded = match kind {
        "continuous_audio_video.delta" => {
            let Some(audio) = ev.get("audio_delta") else {
                return Ok(None);
            };
            if audio.is_null() {
                return Ok(None);
            }
            if let Some(fmt) = audio.get("format") {
                let t = fmt.get("type").and_then(Value::as_str).unwrap_or("");
                let rate = fmt.get("rate").and_then(Value::as_u64);
                if !(t.is_empty() || t == "audio/pcm") || rate != Some(SAMPLE_RATE as u64) {
                    return Err("unsupported audio format; expected PCM16LE mono 24 kHz".into());
                }
            }
            if audio
                .get("num_channels")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                != 1
            {
                return Err("voice audio must be mono".into());
            }
            audio.get("audio_frames")
        }
        "response.output_audio.delta" | "response.audio.delta" => ev.get("delta"),
        _ => return Ok(None),
    };
    let Some(encoded) = encoded.and_then(Value::as_str) else {
        return Err("invalid audio delta".into());
    };
    if encoded.len() > MAX_AUDIO_BYTES.div_ceil(3) * 4 + 4 {
        return Err("oversized audio delta".into());
    }
    let pcm = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "invalid audio base64".to_owned())?;
    if pcm.len() % 2 != 0 || pcm.len() > MAX_AUDIO_BYTES {
        return Err("invalid PCM16 audio length".into());
    }
    Ok(Some(pcm))
}

/// RMS of PCM16LE.
pub fn pcm_rms(pcm: &[u8]) -> f64 {
    let n = pcm.len() / 2;
    if n == 0 {
        return 0.0;
    }
    let sum: f64 = pcm
        .chunks_exact(2)
        .map(|c| {
            let v = i16::from_le_bytes([c[0], c[1]]) as f64;
            v * v
        })
        .sum();
    (sum / n as f64).sqrt()
}

/// Whether this event is a `session_control` "stop" (Muse ends the call after a goodbye).
pub fn is_stop(ev: &Value) -> bool {
    if ev.get("type").and_then(Value::as_str) != Some("conversation.item.added") {
        return false;
    }
    let Some(item) = ev.get("item") else {
        return false;
    };
    let id_ok = item
        .get("call_id")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty() && s.len() <= 128);
    item.get("type").and_then(Value::as_str) == Some("session_control")
        && id_ok
        && item
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| n.trim().eq_ignore_ascii_case("stop"))
}

/// ISO-8601 UTC with milliseconds, as the reference's `isoformat(timespec="milliseconds")`.
pub fn iso_ms(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let ms = d.subsec_millis();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, day) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}+00:00",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Splits arbitrarily chunked NDJSON into objects.
#[derive(Default)]
pub struct NdjsonReader {
    pending: Vec<u8>,
}

impl NdjsonReader {
    /// Splits a chunk into complete NDJSON objects.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        for &b in chunk {
            if self.pending.len() >= MAX_LINE_BYTES {
                return Err("NDJSON line too large".into());
            }
            self.pending.push(b);
            if b == b'\n' {
                let line = std::mem::take(&mut self.pending);
                if line.iter().all(|c| c.is_ascii_whitespace()) {
                    continue;
                }
                let v: Value =
                    serde_json::from_slice(&line).map_err(|_| "invalid NDJSON record")?;
                if !v.is_object() {
                    return Err("NDJSON record must be an object".into());
                }
                out.push(v);
            }
        }
        Ok(out)
    }
}

// ---- audio devices -------------------------------------------------------------------------------

/// Microphone frames from `arecord`, one channel, `FRAME_BYTES` each.
struct Mic {
    child: Child,
    _gain: Option<goblin_platform::MicGainGuard>,
}

impl Mic {
    fn start(platform: &Platform, tx: mpsc::Sender<Vec<u8>>) -> Result<Mic, String> {
        let audio = platform.config().audio.as_ref().ok_or("no audio")?;
        let device = audio
            .capture
            .clone()
            .ok_or("this device has no microphone")?;
        let channels = audio.capture_channels.clamp(1, 2) as usize;
        let use_ch = audio.capture_use;
        let gain = platform.set_mic_gain(MIC_GAIN_DB).ok();
        let mut child = Command::new("arecord")
            .args(["-q", "-D", &device, "-f", "S16_LE", "-t", "raw"])
            .args(["-r", &SAMPLE_RATE.to_string(), "-c", &channels.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start the microphone: {e}"))?;
        let mut out = child.stdout.take().ok_or("no capture pipe")?;
        std::thread::spawn(move || {
            let mut raw = vec![0u8; FRAME_SAMPLES * 2 * channels];
            loop {
                if out.read_exact(&mut raw).is_err() {
                    return;
                }
                let mono = crate::voice::select_channel(&raw, channels, use_ch);
                let bytes: Vec<u8> = mono.iter().flat_map(|s| s.to_le_bytes()).collect();
                // Never block capture: a frame that cannot be queued is dropped.
                if tx.try_send(bytes).is_err() && tx.is_closed() {
                    return;
                }
            }
        });
        Ok(Mic { child, _gain: gain })
    }
}

impl Drop for Mic {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct Playback {
    queue: VecDeque<Vec<u8>>,
    bytes: usize,
    generation: u64,
    stopped: bool,
    child: Option<Child>,
}

impl Playback {
    fn flush(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.queue.clear();
        self.bytes = 0;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Raw playback with a bounded queue; a flush bypasses the queue and closes the player.
struct Speaker {
    playback: Arc<(Mutex<Playback>, Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Speaker {
    fn start(platform: &Platform) -> Speaker {
        let pcm = platform
            .config()
            .audio
            .as_ref()
            .and_then(|a| a.playback.clone())
            .unwrap_or_else(|| "default".into());
        Self::with_spawn(move || {
            Command::new("aplay")
                .args(["-q", "-D", &pcm, "-t", "raw", "-f", "S16_LE"])
                .args(["-r", &SAMPLE_RATE.to_string(), "-c", "1"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()
        })
    }

    fn with_spawn(spawn: impl Fn() -> Option<Child> + Send + 'static) -> Speaker {
        let playback = Arc::new((Mutex::new(Playback::default()), Condvar::new()));
        let shared = playback.clone();
        let worker = std::thread::spawn(move || {
            let mut sink: Option<std::process::ChildStdin> = None;
            let mut writer_generation = 0;
            loop {
                let (data, generation) = {
                    let (lock, wake) = &*shared;
                    let mut state = lock.lock().unwrap();
                    while state.queue.is_empty() && !state.stopped {
                        state = wake.wait(state).unwrap();
                    }
                    if state.stopped {
                        break;
                    }
                    let Some(data) = state.queue.pop_front() else {
                        continue;
                    };
                    state.bytes = state.bytes.saturating_sub(data.len());
                    if writer_generation != state.generation || state.child.is_none() {
                        sink = None;
                        writer_generation = state.generation;
                    }
                    if sink.is_none() {
                        if let Some(mut child) = spawn() {
                            sink = child.stdin.take();
                            state.child = Some(child);
                            if let Some(input) = &sink {
                                let fd = input.as_raw_fd();
                                // SAFETY: the worker owns this pipe. Nonblocking writes let
                                // it notice a flush even if a player stops reading.
                                let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                                if flags < 0
                                    || unsafe {
                                        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK)
                                    } < 0
                                {
                                    sink = None;
                                    state.flush();
                                }
                            }
                        }
                    }
                    (data, state.generation)
                };
                let Some(input) = sink.as_mut() else {
                    continue;
                };
                let mut remaining = data.as_slice();
                while !remaining.is_empty() {
                    {
                        let state = shared.0.lock().unwrap();
                        if state.stopped || state.generation != generation {
                            break;
                        }
                    }
                    match input.write(remaining) {
                        Ok(0) => break,
                        Ok(n) => remaining = &remaining[n..],
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5))
                        }
                        Err(_) => {
                            let mut state = shared.0.lock().unwrap();
                            if state.generation == generation {
                                state.flush();
                            }
                            break;
                        }
                    }
                }
            }
        });
        Speaker {
            playback,
            worker: Some(worker),
        }
    }

    fn play(&self, pcm: Vec<u8>) -> Result<(), String> {
        if pcm.is_empty() {
            return Ok(());
        }
        let mut state = self.playback.0.lock().unwrap();
        if state.queue.len() >= 128 || pcm.len() > MAX_WAITING_BYTES.saturating_sub(state.bytes) {
            return Err("voice playback buffer full".into());
        }
        state.bytes += pcm.len();
        state.queue.push_back(pcm);
        self.playback.1.notify_one();
        Ok(())
    }

    fn flush(&self) {
        self.playback.0.lock().unwrap().flush();
    }

    fn stop(mut self) {
        {
            let mut state = self.playback.0.lock().unwrap();
            state.stopped = true;
            state.flush();
        }
        self.playback.1.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// ---- the call ---------------------------------------------------------------------------------

/// Runs one call until it ends; returns the reason (`local_end`, `remote_stop`, `remote_end`,
/// `idle_timeout`, `overall_timeout` or an error description).
pub async fn run(
    session: LinkSession,
    platform: Arc<Platform>,
    device_id: String,
    thread_id: Option<String>,
    mut cmds: mpsc::Receiver<CallCmd>,
    observer: Arc<dyn CallObserver>,
    greeting: bool,
) -> String {
    let call_id = muse_link::random_uuid();
    let started_wall = SystemTime::now();
    let started = Instant::now();
    let mut call = Call {
        gate: ReplyGate::default(),
        speaker: None,
        playing_until: Instant::now(),
        heard_audio: false,
        held: false,
        muted: false,
        waiting_pcm: Vec::new(),
        active_turns: HashMap::new(),
        activity: Instant::now(),
        phase: Phase::Connecting,
        started,
        observer: observer.clone(),
        kinds_seen: std::collections::HashSet::new(),
        caption_muse: String::new(),
        caption_owner: String::new(),
    };
    call.set_phase(Phase::Connecting);

    let query = format!("/voice/realtime?call_id={call_id}");
    let headers = [muse_proto_header(
        "Content-Type",
        "application/x-ndjson; charset=utf-8",
    )];
    let mut stream = match session.open_duplex("POST", &query, &headers).await {
        Ok(s) => s,
        Err(e) => return format!("could not start the call: {e}"),
    };
    let mut start = json!({
        "type": "hatch.voice.call.start", "call_id": call_id, "device": "mcu-wearable",
        "device_id": device_id, "call_media_type": "audio",
        "proactive_greeting_enabled": greeting,
    });
    if let Some(t) = &thread_id {
        start["thread_id"] = json!(t);
    }
    let mut reason = match send_line(&mut stream, &start).await {
        Err(e) => format!("could not start the call: {e}"),
        Ok(()) => {
            let _ = stream
                .send(format!("{END_CALL_STOP_OFFER}\n").as_bytes())
                .await;
            match stream.wait_response(READY_TIMEOUT).await {
                Err(e) => format!("Muse did not take the call: {e}"),
                Ok(_) => {
                    call.speaker = Some(Speaker::start(&platform));
                    call.converse(&mut stream, &platform, &mut cmds, &call_id, &thread_id)
                        .await
                }
            }
        }
    };
    if let Some(s) = call.speaker.take() {
        tokio::task::spawn_blocking(move || s.stop()).await.ok();
    }

    // Always tell Muse the call ended (the admission may have had effects).
    let mut body = json!({
        "call_id": call_id,
        "call_started_at": iso_ms(started_wall),
        "call_ended_at": iso_ms(SystemTime::now().max(started_wall + Duration::from_millis(1))),
    });
    if let Some(t) = &call.thread_for_end(&thread_id) {
        body["thread_id"] = json!(t);
    }
    let end_body = body.to_string();
    let end_headers = [muse_proto_header("Content-Type", "application/json")];
    let end = session.request("POST", "/voice/end-call", end_body.as_bytes(), &end_headers);
    match tokio::time::timeout(CLEANUP_TIMEOUT, end).await {
        Ok(Ok((status, _))) if (200..300).contains(&status) => log::info!("call: end-call ok"),
        Ok(Ok((status, _))) => log::info!("end-call returned HTTP {status}"),
        Ok(Err(e)) => log::info!("end-call failed: {e}"),
        Err(_) => log::info!("end-call timed out"),
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.half_close()).await;
    stream.close().await;
    if reason.is_empty() {
        reason = "remote_end".into();
    }
    reason
}

fn muse_proto_header(name: &str, value: &str) -> muse_link::Header {
    muse_link::Header::new(name, value)
}

async fn send_line(stream: &mut DuplexStream, v: &Value) -> Result<(), String> {
    let mut line = v.to_string();
    line.push('\n');
    stream
        .send(line.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

struct Call {
    gate: ReplyGate,
    speaker: Option<Speaker>,
    /// Muse's audible speech is expected to play until then (plus `SETTLE`).
    playing_until: Instant,
    heard_audio: bool,
    held: bool,
    muted: bool,
    waiting_pcm: Vec<u8>,
    /// submission message id → caller input item, for the idle timeout.
    active_turns: HashMap<String, String>,
    activity: Instant,
    phase: Phase,
    started: Instant,
    observer: Arc<dyn CallObserver>,
    kinds_seen: std::collections::HashSet<String>,
    caption_muse: String,
    caption_owner: String,
}

impl Call {
    fn thread_for_end(&self, thread: &Option<String>) -> Option<String> {
        thread.clone()
    }

    fn set_phase(&mut self, p: Phase) {
        if p != self.phase {
            log::info!("call: {p:?}");
        }
        self.phase = p;
        self.observer
            .phase(p, self.muted, self.started.elapsed().as_secs_f64());
    }

    /// The microphone is closed: held by Muse, or Muse's speech is (about to be) audible.
    fn gated(&self) -> bool {
        self.held || Instant::now() < self.playing_until + SETTLE
    }

    fn flush(&mut self) {
        self.waiting_pcm.clear();
        self.playing_until = Instant::now();
        if let Some(s) = &self.speaker {
            s.flush();
        }
    }

    fn apply_flush(&mut self) {
        let (playing, waiting) = self.gate.take_flush();
        if playing {
            self.flush();
        } else if waiting {
            self.waiting_pcm.clear();
        }
    }

    fn queue_pcm(&mut self, pcm: Vec<u8>) -> Result<(), String> {
        self.heard_audio = true;
        let now = Instant::now();
        let dur = Duration::from_secs_f64(pcm.len() as f64 / (SAMPLE_RATE as f64 * 2.0));
        let audible = pcm.chunks(FRAME_BYTES).any(|f| pcm_rms(f) >= AUDIBLE_RMS);
        let base = self.playing_until.max(now);
        if audible {
            self.playing_until = base + dur;
            self.activity = now;
            if self.phase != Phase::Speaking {
                self.set_phase(Phase::Speaking);
            }
        } else if self.playing_until > now {
            // Quiet audio inside speech still takes its time.
            self.playing_until = base + dur;
        }
        if let Some(s) = &self.speaker {
            s.play(pcm)?;
        }
        Ok(())
    }

    async fn converse(
        &mut self,
        stream: &mut DuplexStream,
        platform: &Arc<Platform>,
        cmds: &mut mpsc::Receiver<CallCmd>,
        _call_id: &str,
        _thread_id: &Option<String>,
    ) -> String {
        let mut reader = NdjsonReader::default();
        let mut ready = false;
        let ready_deadline = Instant::now() + READY_TIMEOUT;
        let (mic_tx, mut mic_rx) = mpsc::channel::<Vec<u8>>(16);
        let mut mic: Option<Mic> = None;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut mic_tx = Some(mic_tx);
        loop {
            tokio::select! {
                chunk = stream.next_chunk() => {
                    let Some(chunk) = chunk else {
                        return self.drain("remote_end").await;
                    };
                    let chunk = match chunk {
                        Ok(c) => c,
                        Err(e) => return format!("call stream failed: {e}"),
                    };
                    let events = match reader.push(&chunk) {
                        Ok(v) => v,
                        Err(e) => return e,
                    };
                    for ev in events {
                        match self.on_event(&ev, &mut ready) {
                            Ok(None) => {}
                            Ok(Some(EventOutcome::Ready)) => {
                                // Open the microphone once Muse is listening.
                                if let Some(tx) = mic_tx.take() {
                                    match Mic::start(platform, tx) {
                                        Ok(m) => mic = Some(m),
                                        Err(e) => return e,
                                    }
                                }
                                self.set_phase(Phase::Listening);
                            }
                            Ok(Some(EventOutcome::Stop)) => {
                                drop(mic.take());
                                return self.drain("remote_stop").await;
                            }
                            Err(e) => return e,
                        }
                    }
                }
                frame = mic_rx.recv(), if mic.is_some() => {
                    let Some(mut pcm) = frame else {
                        drop(mic.take());
                        self.flush();
                        return "the microphone stopped".into();
                    };
                    if !ready || self.gated() {
                        continue; // never send speech captured while Muse talks
                    }
                    if pcm_rms(&pcm) >= AUDIBLE_RMS && !self.muted {
                        self.activity = Instant::now();
                    }
                    if self.muted {
                        pcm.iter_mut().for_each(|b| *b = 0);
                    }
                    let ev = json!({
                        "type": "input_audio_buffer.append",
                        "audio": base64::engine::general_purpose::STANDARD.encode(&pcm),
                        "send_timestamp_ms": SystemTime::now().duration_since(UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64).unwrap_or(0),
                    });
                    if let Err(e) = send_line(stream, &ev).await {
                        return format!("call stream failed: {e}");
                    }
                }
                cmd = cmds.recv() => {
                    match cmd {
                        None | Some(CallCmd::End) => {
                            drop(mic.take());
                            self.flush();
                            return "local_end".into();
                        }
                        Some(CallCmd::BargeIn) => {
                            let response = self.gate.open_response.clone();
                            self.gate.interrupt(None);
                            self.apply_flush();
                            self.set_phase(Phase::Listening);
                            if let Some(id) = response {
                                let ev = json!({"type": "response.cancel", "response_id": id});
                                let _ = tokio::time::timeout(
                                    Duration::from_secs(1), send_line(stream, &ev)).await;
                            }
                        }
                        Some(CallCmd::Mute(m)) => {
                            self.muted = m;
                            let p = self.phase;
                            self.set_phase(p);
                        }
                    }
                }
                _ = tick.tick() => {
                    let now = Instant::now();
                    if now.duration_since(self.started) >= OVERALL_TIMEOUT {
                        return "overall_timeout".into();
                    }
                    if !ready && now >= ready_deadline {
                        return "Muse did not get ready in time".into();
                    }
                    if ready && self.active_turns.is_empty()
                        && now.duration_since(self.activity) >= IDLE_TIMEOUT
                        && now >= self.playing_until
                    {
                        return "idle_timeout".into();
                    }
                    // Speech finished: back to listening.
                    if self.phase == Phase::Speaking && now >= self.playing_until + SETTLE {
                        let next = if self.active_turns.is_empty() {
                            Phase::Listening
                        } else {
                            Phase::Thinking
                        };
                        self.set_phase(next);
                    }
                }
            }
        }
    }

    /// Lets the last speech play (bounded), then ends.
    async fn drain(&mut self, reason: &str) -> String {
        let limit = Instant::now() + Duration::from_secs(6);
        while Instant::now() < self.playing_until.min(limit) {
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        reason.to_owned()
    }

    fn on_event(&mut self, ev: &Value, ready: &mut bool) -> Result<Option<EventOutcome>, String> {
        let kind = ev
            .get("type")
            .and_then(Value::as_str)
            .ok_or("voice event has no type")?
            .to_owned();
        // Which event kinds a call carries is evidence worth keeping: log each kind once.
        if self.kinds_seen.insert(kind.clone()) {
            log::info!("call event kind: {kind}");
        }
        if let Some((assistant, text, delta, done)) = transcript_from_event(ev) {
            let line = if assistant {
                &mut self.caption_muse
            } else {
                &mut self.caption_owner
            };
            if delta {
                line.push_str(&text);
            } else {
                *line = text;
            }
            self.observer.caption(assistant, line, done);
            if done {
                line.clear();
            }
        }
        let mut outcome = None;
        match kind.as_str() {
            "error" => {
                let code = ev
                    .get("error")
                    .and_then(|e| e.get("code"))
                    .and_then(Value::as_str);
                if !*ready || code == Some("session_dead") {
                    return Err("the voice session failed".into());
                }
                // Operation errors do not end the call. Never log the message (user data).
                log::info!("voice operation reported an error; call continues");
                return Ok(None);
            }
            "hatch.voice.session" => {
                if *ready {
                    return Err("duplicate voice readiness".into());
                }
                *ready = true;
                self.activity = Instant::now();
                outcome = Some(EventOutcome::Ready);
            }
            "hatch.voice.switch_input_hold" => {
                self.held = ev
                    .get("held")
                    .and_then(Value::as_bool)
                    .ok_or("invalid input hold event")?;
            }
            "input_audio_buffer.speech_started" => {
                self.activity = Instant::now();
            }
            "conversation.item.added" if is_stop(ev) => {
                if !*ready {
                    return Err("stop received before readiness".into());
                }
                return Ok(Some(EventOutcome::Stop));
            }
            "hatch.voice.turn_started" => {
                let mid = identifier(ev.get("submission_message_id"));
                let item = identifier(ev.get("item_id"));
                if let (Some(mid), Some(item)) = (mid, item) {
                    if self.active_turns.len() < 64 {
                        self.active_turns.insert(mid, item);
                    }
                    self.activity = Instant::now();
                    if self.phase == Phase::Listening {
                        self.set_phase(Phase::Thinking);
                    }
                }
            }
            "hatch.voice.turn_settled" => {
                if let Some(mid) = identifier(ev.get("response_message_id")) {
                    if let Some(item) = self.active_turns.get(&mid).cloned() {
                        self.active_turns.retain(|_, v| *v != item);
                        self.activity = Instant::now();
                    }
                }
            }
            "response.created" | "response.done" | "response.completed" => {
                self.activity = Instant::now();
            }
            _ => {}
        }
        let was_waiting = self.gate.waiting();
        self.gate.event(ev)?;
        self.apply_flush();
        if was_waiting && !self.gate.waiting() {
            if self.gate.playing() && !self.waiting_pcm.is_empty() {
                let held = std::mem::take(&mut self.waiting_pcm);
                self.queue_pcm(held)?;
            }
            self.waiting_pcm.clear();
        }
        if let Some(pcm) = audio_from_event(ev)? {
            if pcm.iter().all(|&b| b == 0) && !self.heard_audio && self.waiting_pcm.is_empty() {
                // Zero keepalives: no audio state change.
                if *ready {
                    let _ = self.gate.audio(ev)?;
                    self.apply_flush();
                }
                return Ok(outcome);
            }
            if !*ready {
                return Err("audio received before readiness".into());
            }
            let verdict = self.gate.audio(ev)?;
            self.apply_flush();
            match verdict {
                Verdict::Drop => {}
                Verdict::Hold => {
                    if self.waiting_pcm.len() + pcm.len() > MAX_WAITING_BYTES {
                        return Err("unconfirmed voice reply buffer full".into());
                    }
                    self.waiting_pcm.extend_from_slice(&pcm);
                }
                Verdict::Play => self.queue_pcm(pcm)?,
            }
        }
        Ok(outcome)
    }
}

enum EventOutcome {
    Ready,
    Stop,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_is_bounded_and_flush_interrupts_a_blocked_writer() {
        let speaker = Speaker::with_spawn(|| {
            Command::new("sleep")
                .arg("30")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()
        });
        assert!(speaker.play(vec![0; MAX_WAITING_BYTES + 1]).is_err());
        speaker.play(vec![1; MAX_WAITING_BYTES]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while speaker.playback.0.lock().unwrap().child.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(speaker.playback.0.lock().unwrap().child.is_some());
        speaker.play(vec![2; MAX_WAITING_BYTES]).unwrap();
        assert!(speaker.play(vec![3]).is_err());
        let started = Instant::now();
        speaker.flush();
        {
            let state = speaker.playback.0.lock().unwrap();
            assert!(state.queue.is_empty() && state.child.is_none());
            assert_eq!(state.bytes, 0);
        }
        speaker.stop();
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn iso_timestamps() {
        let t = UNIX_EPOCH + Duration::from_millis(1_794_445_131_419);
        assert_eq!(iso_ms(t), "2026-11-12T00:58:51.419+00:00");
        assert_eq!(iso_ms(UNIX_EPOCH), "1970-01-01T00:00:00.000+00:00");
    }

    #[test]
    fn ndjson_across_chunks() {
        let mut r = NdjsonReader::default();
        assert!(r.push(br#"{"type":"a"}"#).unwrap().is_empty());
        let v = r.push(b"\n\n{\"type\":\"b\"}\n").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1]["type"], "b");
        assert!(r.push(b"[1]\n").is_err());
    }

    #[test]
    fn audio_deltas() {
        let pcm = [1u8, 0, 2, 0];
        let b64 = base64::engine::general_purpose::STANDARD.encode(pcm);
        let cavd = json!({"type": "continuous_audio_video.delta", "response_id": "r1",
            "audio_delta": {"format": {"type": "audio/pcm", "rate": 24000}, "audio_frames": b64}});
        assert_eq!(audio_from_event(&cavd).unwrap().unwrap(), pcm);
        let resp = json!({"type": "response.output_audio.delta", "delta": b64});
        assert_eq!(audio_from_event(&resp).unwrap().unwrap(), pcm);
        let bad = json!({"type": "continuous_audio_video.delta",
            "audio_delta": {"format": {"rate": 16000}, "audio_frames": b64}});
        assert!(audio_from_event(&bad).is_err());
        assert!(audio_from_event(&json!({"type": "other"}))
            .unwrap()
            .is_none());
    }

    #[test]
    fn transcript_events() {
        let d = json!({"type": "response.output_audio_transcript.delta", "delta": "Good "});
        assert_eq!(
            transcript_from_event(&d),
            Some((true, "Good ".into(), true, false))
        );
        let f = json!({"type": "conversation.item.input_audio_transcription.completed",
            "transcript": "hello"});
        assert_eq!(
            transcript_from_event(&f),
            Some((false, "hello".into(), false, true))
        );
        assert_eq!(
            transcript_from_event(&json!({"type": "response.done"})),
            None
        );
    }

    #[test]
    fn stop_control() {
        let stop = json!({"type": "conversation.item.added",
            "item": {"type": "session_control", "call_id": "c1", "name": " Stop "}});
        assert!(is_stop(&stop));
        let other = json!({"type": "conversation.item.added",
            "item": {"type": "message", "call_id": "c1", "name": "stop"}});
        assert!(!is_stop(&other));
    }

    fn delta(rid: &str) -> Value {
        json!({"type": "continuous_audio_video.delta", "response_id": rid,
            "audio_delta": {"audio_frames": ""}})
    }

    #[test]
    fn untagged_audio_resumes_only_after_the_next_turn_is_confirmed() {
        let mut g = ReplyGate::default();
        let audio = json!({"type":"response.output_audio.delta", "delta":""});
        assert_eq!(g.audio(&audio).unwrap(), Verdict::Play);
        g.interrupt(None);
        assert_eq!(g.audio(&audio).unwrap(), Verdict::Drop);
        g.event(&json!({"type":"input_audio_buffer.speech_started", "item_id":"next"}))
            .unwrap();
        assert_eq!(g.audio(&audio).unwrap(), Verdict::Drop);
        g.event(&json!({"type":"hatch.voice.turn_started", "item_id":"next", "submission_message_id":"m"})).unwrap();
        assert_eq!(g.audio(&audio).unwrap(), Verdict::Play);
        g.event(&json!({"type":"hatch.voice.turn_started", "item_id":"other", "submission_message_id":"n"})).unwrap();
        assert_eq!(g.audio(&audio).unwrap(), Verdict::Drop);
    }

    #[test]
    fn gate_plays_until_interrupted_then_fences_old_reply() {
        let mut g = ReplyGate::default();
        assert_eq!(g.audio(&delta("r1")).unwrap(), Verdict::Play);
        // The caller starts speaking: r1 becomes stale and playback flushes.
        g.event(&json!({"type": "input_audio_buffer.speech_started", "item_id": "i1"}))
            .unwrap();
        assert_eq!(g.take_flush(), (true, true));
        assert_eq!(g.audio(&delta("r1")).unwrap(), Verdict::Drop);
        // New audio before the caller's turn is confirmed is held...
        assert_eq!(g.audio(&delta("r2")).unwrap(), Verdict::Hold);
        // ...and plays once the turn for i1 starts.
        g.event(&json!({"type": "hatch.voice.turn_started", "item_id": "i1",
            "submission_message_id": "m1"}))
            .unwrap();
        assert!(g.playing());
        assert_eq!(g.audio(&delta("r2")).unwrap(), Verdict::Play);
    }

    #[test]
    fn gate_blocks_reply_to_someone_elses_turn() {
        let mut g = ReplyGate::default();
        g.event(&json!({"type": "input_audio_buffer.speech_started", "item_id": "i1"}))
            .unwrap();
        g.event(
            &json!({"type": "hatch.voice.turn_started", "item_id": "other",
            "submission_message_id": "m9"}),
        )
        .unwrap();
        assert_eq!(g.audio(&delta("r5")).unwrap(), Verdict::Drop);
    }

    #[test]
    fn rms() {
        assert_eq!(pcm_rms(&[]), 0.0);
        let loud: Vec<u8> = std::iter::repeat_n(1000i16.to_le_bytes(), 4)
            .flatten()
            .collect();
        assert!((pcm_rms(&loud) - 1000.0).abs() < 0.01);
    }
}
