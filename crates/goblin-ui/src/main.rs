//! SDL2 shell for the Goblin Link UI.
//!
//! Creates a fullscreen window and a streaming texture, uploads the [`goblin_ui_kit::Canvas`] the
//! app renders, and maps SDL joystick/controller events to [`goblin_ui_kit::Button`]s using the
//! platform input map. The loop is event-driven: it blocks on events when idle (near-0% CPU) and
//! switches to vsync-paced polling while animating. Also implements `--input-test`,
//! `--screenshot` and `--fps`.
//!
//! Desktop keyboard fallback (for development): arrows = D-pad, Enter = A, Backspace = B,
//! A/S = X/Y, Q/W = L1/R1, E/R = L2/R2, Tab = Select, M = Menu, F/G = F1/F2, Space = Start,
//! Esc = quit.

mod script_input;

use goblin_ui::App;
use goblin_ui_kit::{Button, Canvas, InputMap, InputState, Mode, Theme, Timing, Ui};
use std::time::Instant;

struct Options {
    width: Option<u32>,
    height: Option<u32>,
    mode: Mode,
    ui_size: goblin_ui_kit::UiSize,
    fps: bool,
    input_test: bool,
    screenshot: Option<(String, String)>,
    benchmark: bool,
    gpu_bench: bool,
    font_dir: Option<String>,
    /// Use the real daemon over IPC instead of the mock (optionally at this socket).
    live: Option<Option<String>>,
}

fn parse_args() -> Result<Options, String> {
    let mut o = Options {
        width: None,
        height: None,
        mode: Mode::Light,
        ui_size: goblin_ui_kit::UiSize::Normal,
        fps: false,
        input_test: false,
        screenshot: None,
        benchmark: false,
        gpu_bench: false,
        font_dir: None,
        live: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input-test" => o.input_test = true,
            "--benchmark" => o.benchmark = true,
            "--gpu-bench" => o.gpu_bench = true,
            "--fps" => o.fps = true,
            "--dark" => o.mode = Mode::Dark,
            "--light" => o.mode = Mode::Light,
            "--ui-size" => {
                let v = args
                    .next()
                    .ok_or("--ui-size needs compact|normal|comfortable")?;
                o.ui_size = goblin_ui_kit::UiSize::from_id(&v).ok_or("unknown --ui-size")?;
            }
            "--size" => {
                let v = args.next().ok_or("--size needs WxH")?;
                let (w, h) = v.split_once('x').ok_or("--size WxH")?;
                o.width = Some(w.parse().map_err(|_| "bad width")?);
                o.height = Some(h.parse().map_err(|_| "bad height")?);
            }
            "--live" => o.live = Some(o.live.take().flatten()),
            "--socket" => o.live = Some(Some(args.next().ok_or("--socket needs a path")?)),
            "--font-dir" => o.font_dir = Some(args.next().ok_or("--font-dir needs a path")?),
            "--screenshot" => {
                let s = args.next().ok_or("--screenshot needs <screen> <out>")?;
                let out = args.next().ok_or("--screenshot needs <out>")?;
                o.screenshot = Some((s, out));
            }
            "--help" | "-h" => {
                println!(
                    "goblin-ui [--size WxH] [--dark|--light] [--fps] [--input-test]\n         [--screenshot <screen> <out.png>] [--benchmark] [--font-dir <dir>]\n         [--live [--socket <path>]]   talk to goblind (default socket: $GOBLIN_SOCKET or <runtime>/active)"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(o)
}

fn make_ui(font_dir: &Option<String>) -> Result<Ui, String> {
    match font_dir {
        Some(d) => Ui::with_font_dir(std::path::Path::new(d)).map_err(|e| e.to_string()),
        None => Ui::new().map_err(|e| e.to_string()),
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("goblin-ui: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let opts = parse_args()?;

    // Headless screenshot path: no window needed.
    if let Some((screen, out)) = &opts.screenshot {
        let mut ui = make_ui(&opts.font_dir)?;
        let w = opts.width.unwrap_or(1024);
        let h = opts.height.unwrap_or(768);
        let canvas = goblin_ui::render_screen(&mut ui, screen, w, h, opts.mode, 1500, opts.ui_size)
            .ok_or_else(|| format!("unknown screen: {screen}"))?;
        canvas
            .to_image()
            .save_png(std::path::Path::new(out))
            .map_err(|e| e.to_string())?;
        println!("wrote {out} ({w}x{h})");
        return Ok(());
    }

    // Headless benchmark: measures the software-render frame time (the A53-bound cost) while
    // scrolling the chat, plus the idle (no-change) cost. No display required.
    if opts.benchmark {
        return run_benchmark(&opts);
    }
    if opts.gpu_bench {
        return run_gpu_bench();
    }

    run_sdl(opts)
}

fn run_benchmark(opts: &Options) -> Result<(), String> {
    let mut ui = make_ui(&opts.font_dir)?;
    let w = opts.width.unwrap_or(1024);
    let h = opts.height.unwrap_or(768);
    let scale = goblin_ui_kit::scale_for(w, h);
    let mut theme = Theme::new(opts.mode, scale);
    theme.set_ui_size(opts.ui_size);
    let mut app = App::new(theme);
    // `GOBLIN_BENCH_SCREEN` picks another screen or scenario (e.g. `chat@media`).
    let screen = std::env::var("GOBLIN_BENCH_SCREEN").unwrap_or_else(|_| "chat".into());
    app.force_screen(&screen);
    let mut canvas = Canvas::new(w, h);

    // Warm the glyph/icon caches.
    app.render(&mut canvas, &mut ui, 0);

    // Scrolling: drive Down/Up repeats so the transcript scrolls, measuring each full render.
    let frames = 600u32;
    let mut times = Vec::with_capacity(frames as usize);
    let mut t = 100u64;
    let mut phases = [0.0f32; 4];
    for i in 0..frames {
        // Alternate scroll direction every 60 frames to keep content moving.
        let b = if (i / 60) % 2 == 0 {
            Button::Down
        } else {
            Button::Up
        };
        app.handle(
            goblin_ui_kit::InputEvent {
                button: b,
                phase: goblin_ui_kit::Phase::Press,
            },
            &mut ui,
        );
        t += 16;
        app.tick(&mut ui, t);
        let f0 = Instant::now();
        app.render(&mut canvas, &mut ui, t);
        times.push(f0.elapsed().as_secs_f64() * 1000.0);
        for (sum, ms) in phases.iter_mut().zip(app.phase_ms) {
            *sum += ms;
        }
    }
    times.sort_by(|a, b| a.total_cmp(b));
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let p50 = times[times.len() / 2];
    let p99 = times[times.len() * 99 / 100];
    let max = *times.last().unwrap();

    // Idle cost: no state change, render repeatedly (dirty=false fast path).
    let mut idle = Vec::with_capacity(120);
    for _ in 0..120 {
        let f0 = Instant::now();
        app.render(&mut canvas, &mut ui, t);
        idle.push(f0.elapsed().as_secs_f64() * 1000.0);
    }
    let idle_mean = idle.iter().sum::<f64>() / idle.len() as f64;

    println!("goblin-ui benchmark  {w}x{h}  scale {scale:.2}");
    println!("chat scroll render: mean {mean:.2} ms  p50 {p50:.2}  p99 {p99:.2}  max {max:.2}");
    let n = frames as f32;
    println!(
        "per frame ms: before body {:.2}, body {:.2}, header and hints {:.2}, overlays and ring {:.2}",
        phases[0] / n,
        phases[1] / n,
        phases[2] / n,
        phases[3] / n
    );
    println!("implied fps (mean): {:.0}", 1000.0 / mean.max(0.001));
    println!("idle render:        mean {idle_mean:.3} ms");
    println!("cached glyphs: {}", ui.text.cached_glyphs());
    Ok(())
}

fn run_sdl(opts: Options) -> Result<(), String> {
    let sdl = sdl2::init()?;
    let video = sdl.video()?;
    let joystick = sdl.joystick()?;
    // Poll the joystick on the event pump (not a background thread) so idle CPU stays near zero
    // when the event loop is blocked waiting for input.
    sdl2::hint::set("SDL_JOYSTICK_THREAD", "0");

    // Determine resolution: explicit, else the desktop mode.
    let (w, h) = match (opts.width, opts.height) {
        (Some(w), Some(h)) => (w, h),
        _ => {
            let dm = video.desktop_display_mode(0)?;
            (dm.w as u32, dm.h as u32)
        }
    };

    let mut window = video.window("Goblin Link", w, h);
    window.position_centered();
    if opts.width.is_none() {
        window.fullscreen_desktop();
    }
    let window = window.build().map_err(|e| e.to_string())?;
    let mut sdl_canvas = window
        .into_canvas()
        .present_vsync()
        .build()
        .map_err(|e| e.to_string())?;
    let texture_creator = sdl_canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture_streaming(sdl2::pixels::PixelFormatEnum::ABGR8888, w, h)
        .map_err(|e| e.to_string())?;

    let mut ui = make_ui(&opts.font_dir)?;
    let scale = goblin_ui_kit::scale_for(w, h);
    let mut theme = Theme::new(opts.mode, scale);
    theme.set_ui_size(opts.ui_size);
    let mut app = match &opts.live {
        Some(socket) => {
            let socket = socket
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(goblin_ui::live::LiveProvider::default_socket);
            App::with_provider(theme, Box::new(goblin_ui::live::LiveProvider::new(socket)))
        }
        None => App::new(theme),
    };
    app.show_fps = opts.fps;
    app.fixed_clock = false;
    app.watch_system_volume = true;
    app.decode_pictures_in_background();
    if opts.input_test {
        app.enter_input_test();
    }

    // Open the first joystick, if any.
    let _joy = (joystick.num_joysticks().unwrap_or(0) > 0)
        .then(|| joystick.open(0).ok())
        .flatten();
    let platform = goblin_platform::Platform::detect().ok();
    let map = input_map(platform.as_ref().and_then(|p| p.config().input.as_ref()));
    let mut input = InputState::new(Timing::default());

    let mut frame_canvas = goblin_ui_kit::Canvas::new(w, h);
    let start = Instant::now();
    let now_ms = || start.elapsed().as_millis() as u64;

    let mut event_pump = sdl.event_pump()?;
    let mut script = script_input::ScriptInput::from_env();
    let mut trigger_down = [false; 8];
    let mut hat_dirs = [false; 4];
    // GOBLIN_UI_INPUT_LOG=1: log raw controller events (to check a device's button mapping).
    let input_log = std::env::var_os("GOBLIN_UI_INPUT_LOG").is_some();
    // GOBLIN_UI_STATS=1: every 5 s, log loop iterations, SDL events and frames drawn (and why),
    // to diagnose idle CPU on devices.
    let stats_on = std::env::var_os("GOBLIN_UI_STATS").is_some();
    let (mut st_loops, mut st_events, mut st_dirty, mut st_anim) = (0u32, 0u32, 0u32, 0u32);
    let mut st_kinds: std::collections::BTreeMap<String, u32> = Default::default();
    let mut st_last = Instant::now();
    let (mut st_ms_sum, mut st_ms_max) = (0.0f32, 0.0f32);
    let mut st_phase = [0.0f32; 7];

    // Pieces of the screen the GPU holds as textures and moves (see `goblin_ui_kit::sprites`).
    // `GOBLIN_UI_NO_GPU=1` turns this off: every frame is then drawn whole on the CPU.
    let gpu_on = std::env::var_os("GOBLIN_UI_NO_GPU").is_none();
    let mut sprites: std::collections::HashMap<u64, SpriteTexture> = Default::default();
    let mut sprite_bytes = 0usize;
    let mut frame_no = 0u64;
    let (mut st_sprite_frames, mut st_fresh, mut st_fell_back) = (0u32, 0u32, 0u32);
    let mut warned_fallback = false;

    loop {
        // Drain events. When idle, block up to 250 ms so the CPU stays near zero; while
        // animating, poll so the vsync present paces us.
        let animating = app.is_animating();
        let evs = collect_events(&mut event_pump, animating);
        let t = now_ms();
        if stats_on {
            st_loops += 1;
            st_events += evs.len() as u32;
            for e in &evs {
                let k = format!("{e:?}");
                let k = k.split([' ', '{', '(']).next().unwrap_or("?").to_owned();
                *st_kinds.entry(k).or_default() += 1;
            }
            if st_last.elapsed().as_secs() >= 5 {
                eprintln!(
                    "stats: loops {st_loops} events {st_events} {st_kinds:?} frames dirty {st_dirty} animating {st_anim} frame ms avg {:.1} max {st_ms_max:.1}",
                    st_ms_sum / (st_dirty + st_anim).max(1) as f32
                );
                let n = (st_dirty + st_anim).max(1) as f32;
                eprintln!(
                    "stats: per frame ms: before body {:.1}, body {:.1}, header and hints {:.1}, overlays and ring {:.1}, upload {:.1}, copy {:.1}, present {:.1}",
                    st_phase[0] / n, st_phase[1] / n, st_phase[2] / n, st_phase[3] / n, st_phase[4] / n, st_phase[5] / n, st_phase[6] / n
                );
                eprintln!(
                    "stats: sprite frames {st_sprite_frames} (new sprites {st_fresh}, drawn plain instead {st_fell_back}), textures {} using {:.1} MB",
                    sprites.len(),
                    sprite_bytes as f32 / 1_048_576.0
                );
                (st_sprite_frames, st_fresh, st_fell_back) = (0, 0, 0);
                st_phase = [0.0; 7];
                (st_ms_sum, st_ms_max) = (0.0, 0.0);
                (st_loops, st_events, st_dirty, st_anim) = (0, 0, 0, 0);
                st_kinds.clear();
                st_last = Instant::now();
            }
        }
        for e in evs {
            if input_log {
                use sdl2::event::Event as E;
                match &e {
                    E::JoyButtonDown { button_idx, .. } => {
                        eprintln!("input: {t} button {button_idx} down")
                    }
                    E::JoyButtonUp { button_idx, .. } => {
                        eprintln!("input: {t} button {button_idx} up")
                    }
                    E::JoyHatMotion { hat_idx, state, .. } => {
                        eprintln!("input: {t} hat {hat_idx} {state:?}")
                    }
                    E::JoyAxisMotion {
                        axis_idx, value, ..
                    } if value.unsigned_abs() > 8000 => {
                        eprintln!("input: {t} axis {axis_idx} {value}")
                    }
                    E::KeyDown {
                        keycode,
                        repeat: false,
                        ..
                    } => eprintln!("input: {t} key {keycode:?} down"),
                    _ => {}
                }
            }
            // The D-pad arrives as hat motions: turn each change into releases and presses so
            // a direction is never left "held" (which auto-repeats forever).
            let actions = match e {
                sdl2::event::Event::JoyHatMotion { state, .. } => {
                    hat_transitions(&mut hat_dirs, hat_bits(state))
                }
                e => translate(e, &map, &mut trigger_down).into_iter().collect(),
            };
            for action in actions {
                match action {
                    Action::Quit => return restore_and_exit(),
                    Action::Down(b) => {
                        let iev = input.press(b, t);
                        app.handle(iev, &mut ui);
                    }
                    Action::Up(b) => {
                        if let Some(iev) = input.release(b, t) {
                            app.handle(iev, &mut ui);
                        }
                    }
                }
            }
        }
        if let Some(script) = script.as_mut() {
            for cmd in script.poll() {
                match cmd {
                    script_input::Scripted::Quit => return restore_and_exit(),
                    script_input::Scripted::Tap(b) => {
                        let iev = input.press(b, t);
                        app.handle(iev, &mut ui);
                        if let Some(iev) = input.release(b, t) {
                            app.handle(iev, &mut ui);
                        }
                    }
                }
            }
        }
        // Timed hold/repeat events.
        let mut timed = Vec::new();
        input.tick(t, &mut timed);
        for iev in timed {
            if input_log {
                // What the app made of a held button: a repeat here with no second press
                // above it is a step the owner did not ask for.
                eprintln!("input: {t} {:?} {:?}", iev.button, iev.phase);
            }
            app.handle(iev, &mut ui);
        }

        app.tick(&mut ui, t);
        if app.quit {
            return restore_and_exit();
        }

        if app.needs_redraw() || app.is_animating() {
            if stats_on {
                if app.needs_redraw() {
                    st_dirty += 1;
                } else {
                    st_anim += 1;
                }
            }
            let f0 = Instant::now();
            frame_no += 1;
            // A frame the app says can be made of sprites alone is asked for that way.
            let as_sprites = gpu_on && app.sprite_frame();
            ui.sprites.begin(as_sprites, app.sprite_salt());
            frame_canvas.take_written();
            app.render(&mut frame_canvas, &mut ui, t);
            let mut composed = false;
            let mut u0 = Instant::now();
            if as_sprites {
                // New sprites become textures whether or not this frame ends up using them.
                st_fresh += ui.sprites.fresh.len() as u32;
                let mut lost = false;
                let mut failed_sprites = Vec::new();
                for s in ui.sprites.fresh.drain(..) {
                    let made = texture_creator
                        .create_texture_static(sdl2::pixels::PixelFormatEnum::ABGR8888, s.w, s.h)
                        .map_err(|e| e.to_string())
                        .and_then(|mut tex| {
                            tex.update(None, &s.rgba, (s.w * 4) as usize)
                                .map_err(|e| e.to_string())?;
                            tex.set_blend_mode(sdl2::render::BlendMode::Blend);
                            Ok(tex)
                        });
                    match made {
                        Ok(texture) => {
                            sprite_bytes += s.rgba.len();
                            if let Some(old) = sprites.insert(
                                s.key,
                                SpriteTexture {
                                    texture,
                                    w: s.w,
                                    h: s.h,
                                    used: frame_no,
                                },
                            ) {
                                sprite_bytes -= (old.w * old.h * 4) as usize;
                            }
                        }
                        Err(e) => {
                            eprintln!("goblin-ui: no texture for a sprite ({e})");
                            failed_sprites.push(s.key);
                            lost = true;
                        }
                    }
                }
                // The kit must redraw a sprite whose upload failed on the next frame.
                ui.sprites.forget(failed_sprites);
                let complete = !lost
                    && !frame_canvas.take_written()
                    && ui
                        .sprites
                        .placed
                        .iter()
                        .all(|p| sprites.contains_key(&p.key));
                if let (true, Some(page)) = (complete, ui.sprites.clear) {
                    u0 = Instant::now();
                    sdl_canvas.set_draw_color((page.r, page.g, page.b));
                    sdl_canvas.clear();
                    let mut drawn = true;
                    for p in &ui.sprites.placed {
                        let Some(s) = sprites.get_mut(&p.key) else {
                            continue;
                        };
                        s.used = frame_no;
                        sdl_canvas.set_clip_rect(sdl2::rect::Rect::new(
                            p.clip.x,
                            p.clip.y,
                            p.clip.w.max(0) as u32,
                            p.clip.h.max(0) as u32,
                        ));
                        let dst = sdl2::rect::Rect::new(p.x, p.y, s.w, s.h);
                        drawn &= sdl_canvas.copy(&s.texture, None, dst).is_ok();
                    }
                    sdl_canvas.set_clip_rect(None);
                    composed = drawn;
                }
                if composed {
                    st_sprite_frames += 1;
                } else {
                    // Something was drawn outside a sprite (or a texture is missing): the
                    // frame is drawn again whole, as on a device without a GPU.
                    st_fell_back += 1;
                    if !warned_fallback {
                        warned_fallback = true;
                        eprintln!("goblin-ui: a sprite frame was drawn the plain way");
                    }
                    ui.sprites.begin(false, 0);
                    app.render(&mut frame_canvas, &mut ui, t);
                }
            }
            app.frame_ms = f0.elapsed().as_secs_f32() * 1000.0;
            if input_log && app.frame_ms > 60.0 {
                eprintln!("input: {t} slow frame {:.0} ms", app.frame_ms);
            }
            st_ms_sum += app.frame_ms;
            st_ms_max = st_ms_max.max(app.frame_ms);
            let (u1, u2);
            if composed {
                (u1, u2) = (u0, Instant::now());
            } else {
                app.draw_fps(&mut frame_canvas, &mut ui, t);
                u0 = Instant::now();
                texture
                    .update(None, frame_canvas.as_bytes(), (w * 4) as usize)
                    .map_err(|e| e.to_string())?;
                u1 = Instant::now();
                sdl_canvas
                    .copy(&texture, None, None)
                    .map_err(|e| e.to_string())?;
                u2 = Instant::now();
            }
            sdl_canvas.present();
            // Textures not placed for a while go first when there are too many.
            if sprite_bytes > SPRITE_BUDGET_BYTES || sprites.len() > SPRITE_BUDGET_COUNT {
                let mut by_age: Vec<(u64, u64)> =
                    sprites.iter().map(|(k, s)| (s.used, *k)).collect();
                by_age.sort_unstable();
                let mut dropped = Vec::new();
                for (used, key) in by_age {
                    if used == frame_no
                        || (sprite_bytes <= SPRITE_BUDGET_BYTES / 2
                            && sprites.len() <= SPRITE_BUDGET_COUNT / 2)
                    {
                        break;
                    }
                    if let Some(s) = sprites.remove(&key) {
                        sprite_bytes -= (s.w * s.h * 4) as usize;
                        dropped.push(key);
                    }
                }
                ui.sprites.forget(dropped);
            }
            for (sum, ms) in st_phase.iter_mut().zip(app.phase_ms) {
                *sum += ms;
            }
            st_phase[4] += (u1 - u0).as_secs_f32() * 1000.0;
            st_phase[5] += (u2 - u1).as_secs_f32() * 1000.0;
            st_phase[6] += u2.elapsed().as_secs_f32() * 1000.0;
        }
    }
}

/// A sprite as the GPU holds it.
struct SpriteTexture<'t> {
    texture: sdl2::render::Texture<'t>,
    w: u32,
    h: u32,
    /// The frame it was last placed in.
    used: u64,
}

/// Most texture memory sprites may take, and most sprites; past either, the ones unused
/// longest are dropped until half is free.
const SPRITE_BUDGET_BYTES: usize = 64 * 1024 * 1024;
const SPRITE_BUDGET_COUNT: usize = 400;

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Quit,
    Down(Button),
    Up(Button),
}

fn input_map(config: Option<&goblin_platform::config::InputConfig>) -> InputMap {
    let Some(c) = config else {
        return InputMap::default();
    };
    InputMap {
        a: c.a,
        b: c.b,
        x: c.x,
        y: c.y,
        l1: c.l1,
        r1: c.r1,
        select: c.select,
        start: c.start,
        menu: c.menu,
        f1: c.f1,
        f2: c.f2,
        l2_axis: c.l2_axis,
        r2_axis: c.r2_axis,
        layout: c.layout.clone().unwrap_or_else(|| "nintendo".into()),
    }
}

fn collect_events(pump: &mut sdl2::EventPump, animating: bool) -> Vec<sdl2::event::Event> {
    let mut v = Vec::new();
    if animating {
        for e in pump.poll_iter() {
            v.push(e);
        }
    } else {
        // Idle: poll and sleep in 40 ms steps for up to 250 ms. SDL's wait_event_timeout
        // busy-polls in 1 ms steps on backends without a native wait (the Brick's MALI one),
        // which cost ~5% of a core with nothing on screen changing.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
        loop {
            v.extend(pump.poll_iter());
            if !v.is_empty() || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(IDLE_POLL_MS));
        }
    }
    v
}

/// Idle input polling interval: input latency stays under one 25 fps frame.
const IDLE_POLL_MS: u64 = 40;

/// SDL trigger axis threshold (half of the i16 range).
const TRIGGER_ON: i16 = 12000;

fn translate(
    e: sdl2::event::Event,
    map: &InputMap,
    trigger_down: &mut [bool; 8],
) -> Option<Action> {
    use sdl2::event::Event;
    use sdl2::keyboard::Keycode;
    match e {
        Event::Quit { .. } => Some(Action::Quit),
        Event::JoyButtonDown { button_idx, .. } => map.button(button_idx).map(Action::Down),
        Event::JoyButtonUp { button_idx, .. } => map.button(button_idx).map(Action::Up),
        Event::JoyAxisMotion {
            axis_idx, value, ..
        } => {
            let b = map.axis(axis_idx)?;
            let idx = (axis_idx as usize).min(7);
            let on = value > TRIGGER_ON;
            if on != trigger_down[idx] {
                trigger_down[idx] = on;
                return Some(if on { Action::Down(b) } else { Action::Up(b) });
            }
            None
        }
        Event::KeyDown {
            keycode: Some(k),
            repeat: false,
            ..
        } => key_button(k)
            .map(Action::Down)
            .or_else(|| (k == Keycode::Escape).then_some(Action::Quit)),
        Event::KeyUp {
            keycode: Some(k), ..
        } => key_button(k).map(Action::Up),
        _ => None,
    }
}

/// Directions of the D-pad in [`HAT_BUTTONS`] order (up, down, left, right).
const HAT_BUTTONS: [Button; 4] = [Button::Up, Button::Down, Button::Left, Button::Right];

fn hat_bits(state: sdl2::joystick::HatState) -> [bool; 4] {
    use sdl2::joystick::HatState as H;
    match state {
        H::Centered => [false, false, false, false],
        H::Up => [true, false, false, false],
        H::Down => [false, true, false, false],
        H::Left => [false, false, true, false],
        H::Right => [false, false, false, true],
        H::LeftUp => [true, false, true, false],
        H::RightUp => [true, false, false, true],
        H::LeftDown => [false, true, true, false],
        H::RightDown => [false, true, false, true],
    }
}

/// Releases for directions that ended and presses for directions that began, updating `held`.
fn hat_transitions(held: &mut [bool; 4], now: [bool; 4]) -> Vec<Action> {
    let mut out = Vec::new();
    for i in 0..4 {
        if held[i] && !now[i] {
            out.push(Action::Up(HAT_BUTTONS[i]));
        }
    }
    for i in 0..4 {
        if !held[i] && now[i] {
            out.push(Action::Down(HAT_BUTTONS[i]));
        }
    }
    *held = now;
    out
}

fn key_button(k: sdl2::keyboard::Keycode) -> Option<Button> {
    use sdl2::keyboard::Keycode as K;
    Some(match k {
        K::Up => Button::Up,
        K::Down => Button::Down,
        K::Left => Button::Left,
        K::Right => Button::Right,
        K::Return => Button::A,
        K::Backspace => Button::B,
        K::A => Button::X,
        K::S => Button::Y,
        K::Q => Button::L1,
        K::W => Button::R1,
        K::E => Button::L2,
        K::R => Button::R2,
        K::Tab => Button::Select,
        K::M => Button::Menu,
        K::F => Button::F1,
        K::G => Button::F2,
        K::Space => Button::Start,
        _ => return None,
    })
}

/// The shell owns only the display and gamepad; it never pauses or resumes the launcher. That is
/// the deploy script's job (tools/run-device.sh), so a clean exit here just drops the window.
fn restore_and_exit() -> Result<(), String> {
    Ok(())
}

/// `--gpu-bench`: what the device's SDL renderer is and how fast it does the things a
/// GPU-drawn UI would ask of it, next to what the app does now (draw every pixel on the CPU
/// and upload the whole frame). Takes the screen for about ten seconds and prints times.
fn run_gpu_bench() -> Result<(), String> {
    use sdl2::pixels::PixelFormatEnum;
    use sdl2::rect::Rect as SdlRect;
    let sdl = sdl2::init()?;
    let video = sdl.video()?;
    let dm = video.desktop_display_mode(0)?;
    let (w, h) = (dm.w as u32, dm.h as u32);
    let window = video
        .window("gpu-bench", w, h)
        .fullscreen_desktop()
        .build()
        .map_err(|e| e.to_string())?;
    for (i, d) in sdl2::render::drivers().enumerate() {
        println!("driver {i}: {} flags {:#x}", d.name, d.flags);
    }
    let mut canvas = window
        .into_canvas()
        .present_vsync()
        .build()
        .map_err(|e| e.to_string())?;
    let info = canvas.info();
    println!(
        "renderer: {} flags {:#x} max texture {}x{} formats {:?}",
        info.name,
        info.flags,
        info.max_texture_width,
        info.max_texture_height,
        info.texture_formats
    );
    println!(
        "video driver: {}  screen {w}x{h}",
        video.current_video_driver()
    );
    let tc = canvas.texture_creator();
    let frames = 120u32;
    let ms = |t: Instant| t.elapsed().as_secs_f32() * 1000.0;

    // 1. Present alone (the vsync wait and the swap).
    let t = Instant::now();
    for i in 0..frames {
        canvas.set_draw_color((i as u8, 40, 60));
        canvas.clear();
        canvas.present();
    }
    println!(
        "clear + present:                      {:.2} ms/frame",
        ms(t) / frames as f32
    );

    // 2. What the app does now: upload a whole frame, draw it, present.
    let mut full = tc
        .create_texture_streaming(PixelFormatEnum::ABGR8888, w, h)
        .map_err(|e| e.to_string())?;
    let mut pixels = vec![0x80u8; (w * h * 4) as usize];
    let (mut up, mut total) = (0.0, 0.0);
    for i in 0..frames {
        pixels[(i as usize * 4) % 4096] = i as u8;
        let t = Instant::now();
        let u = Instant::now();
        full.update(None, &pixels, (w * 4) as usize)
            .map_err(|e| e.to_string())?;
        up += ms(u);
        canvas.copy(&full, None, None)?;
        canvas.present();
        total += ms(t);
    }
    println!(
        "full-frame upload + present (today):  {:.2} ms/frame, of which upload {:.2}",
        total / frames as f32,
        up / frames as f32
    );

    // 3. Uploading only a part that changed (a band a fifth of the screen high).
    let band = SdlRect::new(0, (h / 3) as i32, w, h / 5);
    let band_px = vec![0x60u8; (w * (h / 5) * 4) as usize];
    let t = Instant::now();
    for _ in 0..frames {
        full.update(band, &band_px, (w * 4) as usize)
            .map_err(|e| e.to_string())?;
    }
    println!(
        "upload of a 1/5-screen band:          {:.2} ms",
        ms(t) / frames as f32
    );
    let small = SdlRect::new(100, 100, 256, 64);
    let small_px = vec![0x60u8; 256 * 64 * 4];
    let t = Instant::now();
    for _ in 0..frames {
        full.update(small, &small_px, 256 * 4)
            .map_err(|e| e.to_string())?;
    }
    println!(
        "upload of a 256x64 patch:             {:.3} ms",
        ms(t) / frames as f32
    );

    // 4. A GPU-composed frame: a background, 12 bubble-sized blended textures, 400 glyph-sized
    // blended quads from one atlas, and two picture tiles; nothing uploaded per frame.
    let mut bubble = tc
        .create_texture_static(PixelFormatEnum::ABGR8888, 512, 128)
        .map_err(|e| e.to_string())?;
    bubble
        .update(None, &vec![0xC0u8; 512 * 128 * 4], 512 * 4)
        .map_err(|e| e.to_string())?;
    bubble.set_blend_mode(sdl2::render::BlendMode::Blend);
    let mut atlas = tc
        .create_texture_static(PixelFormatEnum::ABGR8888, 512, 512)
        .map_err(|e| e.to_string())?;
    atlas
        .update(None, &vec![0xA0u8; 512 * 512 * 4], 512 * 4)
        .map_err(|e| e.to_string())?;
    atlas.set_blend_mode(sdl2::render::BlendMode::Blend);
    let mut picture = tc
        .create_texture_static(PixelFormatEnum::ABGR8888, 1024, 768)
        .map_err(|e| e.to_string())?;
    picture
        .update(None, &vec![0x90u8; 1024 * 768 * 4], 1024 * 4)
        .map_err(|e| e.to_string())?;
    let (mut draw, mut total) = (0.0, 0.0);
    for f in 0..frames {
        let t = Instant::now();
        canvas.set_draw_color((18, 18, 20));
        canvas.clear();
        let scroll = (f % 60) as i32;
        for i in 0..12 {
            let y = i * 70 - scroll;
            canvas.copy(&bubble, None, SdlRect::new(20 + (i % 2) * 300, y, 600, 60))?;
        }
        for i in 0..400i32 {
            let (gx, gy) = ((i % 16) * 24, (i / 16) * 24);
            canvas.copy(
                &atlas,
                SdlRect::new(gx, gy, 14, 22),
                SdlRect::new(30 + (i % 40) * 15, 10 + (i / 40) * 70 - scroll, 14, 22),
            )?;
        }
        for i in 0..2 {
            canvas.copy(
                &picture,
                None,
                SdlRect::new(20, 200 + i * 260 - scroll, 400, 240),
            )?;
        }
        draw += ms(t);
        canvas.present();
        total += ms(t);
    }
    println!(
        "GPU-composed frame (414 quads):       {:.2} ms/frame, of which issuing draws {:.2}",
        total / frames as f32,
        draw / frames as f32
    );

    // 5. The same with a full-screen blended layer on top (a scrim or a fade).
    let t = Instant::now();
    for _ in 0..frames {
        canvas.set_draw_color((18, 18, 20));
        canvas.clear();
        for _ in 0..4 {
            canvas.copy(&picture, None, None)?;
        }
        canvas.copy(&bubble, None, None)?;
        canvas.present();
    }
    println!(
        "4 full-screen layers + 1 blended:     {:.2} ms/frame",
        ms(t) / frames as f32
    );
    Ok(())
}

#[cfg(test)]
mod hat_tests {
    use super::*;

    #[test]
    fn hat_press_then_center_releases() {
        let mut held = [false; 4];
        assert_eq!(
            hat_transitions(&mut held, [false, true, false, false]),
            vec![Action::Down(Button::Down)]
        );
        // Centering releases it, so nothing stays held and auto-repeats.
        assert_eq!(
            hat_transitions(&mut held, [false; 4]),
            vec![Action::Up(Button::Down)]
        );
        assert_eq!(hat_transitions(&mut held, [false; 4]), vec![]);
    }

    #[test]
    fn hat_direction_change_releases_old_first() {
        let mut held = [true, false, false, false];
        assert_eq!(
            hat_transitions(&mut held, [false, false, false, true]),
            vec![Action::Up(Button::Up), Action::Down(Button::Right)]
        );
    }
}

#[cfg(test)]
mod platform_input_tests {
    use super::*;

    #[test]
    fn platform_buttons_keep_missing_controls_unbound() {
        for (file, f1, f2) in [
            ("tg5040-brick-nextui", Some(9), Some(10)),
            ("tg5040-brickpro-nextui", Some(11), Some(12)),
            ("tg5040-smartpro-nextui", None, None),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../platforms")
                .join(file)
                .join("platform.toml");
            let config = goblin_platform::PlatformConfig::parse(
                &std::fs::read_to_string(&path).unwrap(),
                &path,
            )
            .unwrap();
            let map = input_map(config.input.as_ref());
            assert_eq!(map.f1, f1, "{file}");
            assert_eq!(map.f2, f2, "{file}");
            if file != "tg5040-brick-nextui" {
                assert_ne!(map.button(9), Some(Button::F1));
                assert_ne!(map.button(10), Some(Button::F2));
            }
        }
    }
}
