//! A frame made of sprites must look like the frame drawn directly.
//!
//! The app draws the chat either straight onto the frame (screenshots, the goldens, devices
//! without a GPU) or as sprites the GPU places. Here the sprites are composed on the CPU the
//! way the GPU would, at several scroll positions with the sprites kept between frames (as
//! on the device), and compared with the same frames drawn directly.

use std::collections::HashMap;

use goblin_ui::App;
use goblin_ui_kit::sprites::{compose, Pixels};
use goblin_ui_kit::{Button, Canvas, InputEvent, Mode, Phase, Theme, Ui};

fn press(button: Button) -> InputEvent {
    InputEvent {
        button,
        phase: Phase::Press,
    }
}

/// Draws the app's next frame directly.
fn direct(app: &mut App, ui: &mut Ui, canvas: &mut Canvas, t: u64) -> Vec<u32> {
    ui.sprites.begin(false, 0);
    app.tick(ui, t);
    app.render(canvas, ui, t);
    canvas.pixels().to_vec()
}

/// Draws the app's next frame as sprites and composes them; `None` if something was drawn
/// outside a sprite.
fn from_sprites(
    app: &mut App,
    ui: &mut Ui,
    canvas: &mut Canvas,
    kept: &mut HashMap<u64, Pixels>,
    t: u64,
) -> Option<(Vec<u32>, usize)> {
    ui.sprites.begin(true, app.sprite_salt());
    canvas.take_written();
    app.tick(ui, t);
    app.render(canvas, ui, t);
    let fresh = ui.sprites.fresh.len();
    for s in ui.sprites.fresh.drain(..) {
        kept.insert(s.key, s);
    }
    if canvas.take_written() {
        return None;
    }
    let page = ui
        .sprites
        .clear
        .expect("the frame starts from the page colour");
    Some((
        compose(
            canvas.width(),
            canvas.height(),
            page,
            &ui.sprites.placed,
            kept,
        ),
        fresh,
    ))
}

fn worst(a: &[u32], b: &[u32]) -> u32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            (0..3)
                .map(|i| ((x >> (i * 8)) & 0xFF).abs_diff((y >> (i * 8)) & 0xFF))
                .max()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
}

fn check(screen: &str, mode: Mode) {
    let (w, h) = (1024, 768);
    let scale = goblin_ui_kit::scale_for(w, h);
    let new_app = || {
        let mut app = App::new(Theme::new(mode, scale));
        // The stand-in avatar blinks by the clock; hold it still so both drawings of a
        // frame show the same face. (On a device the avatar is a picture.)
        app.reduce_motion = true;
        app.force_screen(screen);
        app
    };
    let (mut a, mut b) = (new_app(), new_app());
    let (mut ui_a, mut ui_b) = (Ui::new().unwrap(), Ui::new().unwrap());
    let (mut ca, mut cb) = (Canvas::new(w, h), Canvas::new(w, h));
    let mut kept = HashMap::new();
    // Settle both (the first frames place focus and scroll to the newest message).
    let mut t = 1000;
    for _ in 0..3 {
        direct(&mut a, &mut ui_a, &mut ca, t);
        direct(&mut b, &mut ui_b, &mut cb, t);
        t += 500;
    }
    let mut frames = 0;
    let mut reused = 0;
    for step in 0..14 {
        // Read back through the chat, then forward again; let each scroll finish.
        let button = if step < 8 { Button::L2 } else { Button::R2 };
        a.handle(press(button), &mut ui_a);
        b.handle(press(button), &mut ui_b);
        for _ in 0..3 {
            t += 400;
            let want = direct(&mut a, &mut ui_a, &mut ca, t);
            let (got, fresh) = from_sprites(&mut b, &mut ui_b, &mut cb, &mut kept, t)
                .unwrap_or_else(|| panic!("{screen}: step {step} drew outside a sprite"));
            let diff = worst(&want, &got);
            if diff > 3 {
                // `SPRITE_DEBUG_DIR` gets both pictures, to look at where they part.
                if let Some(dir) = std::env::var_os("SPRITE_DEBUG_DIR") {
                    let dir = std::path::Path::new(&dir);
                    let name = screen.replace('@', "-");
                    for (kind, px) in [("direct", &want), ("sprites", &got)] {
                        let img = goblin_ui_kit::Image::from_packed(w, h, px.clone());
                        let _ = img.save_png(&dir.join(format!("{name}-{step}-{kind}.png")));
                    }
                }
            }
            assert!(diff <= 3, "{screen}: step {step} differs by {diff}");
            frames += 1;
            if fresh == 0 {
                reused += 1;
            }
        }
    }
    // Most frames while reading need nothing new drawn: that is the point.
    assert!(
        reused * 2 > frames,
        "{screen}: only {reused} of {frames} frames reused every sprite"
    );
}

#[test]
fn the_chat_is_the_same_from_sprites() {
    check("chat", Mode::Light);
    check("chat", Mode::Dark);
}

#[test]
fn a_chat_with_markdown_and_one_with_pictures_are_the_same_from_sprites() {
    check("chat@markdown", Mode::Light);
    check("chat@media", Mode::Dark);
}
