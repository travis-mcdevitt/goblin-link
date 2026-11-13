//! The Goblin Link front-end application, split from the SDL shell so it can be rendered
//! head-lessly (golden-image tests, `--screenshot`). The SDL `main` drives [`app::App`] with
//! real controller input; everything else here is platform-independent.

pub mod app;
pub mod data;
pub mod live;
pub mod media;

pub use app::App;

use goblin_ui_kit::{Canvas, Mode, Theme, Ui};

/// Renders a single named screen to a fresh [`Canvas`] at `w`×`h`, for screenshots and tests.
///
/// `mode` selects light/dark; `now_ms` drives animation so goldens are deterministic. Returns
/// the canvas, or `None` if the screen name is unknown.
pub fn render_screen(
    ui: &mut Ui,
    screen: &str,
    w: u32,
    h: u32,
    mode: Mode,
    now_ms: u64,
    size: goblin_ui_kit::UiSize,
) -> Option<Canvas> {
    let scale = goblin_ui_kit::scale_for(w, h);
    let mut theme = Theme::new(mode, scale);
    theme.set_ui_size(size);
    // Each screen starts from a resting mascot (the Ui may be shared between renders).
    ui.mascot.set_state(goblin_ui_kit::MascotState::Idle);
    let mut app = App::new(theme);
    if !app.force_screen(screen) {
        return None;
    }
    let mut canvas = Canvas::new(w, h);
    // Three frames, as the device settles: the first measures the content and picks the initial
    // focus, the second scrolls a pinned chat to its end, the third draws the result.
    // Time moves between them, so eased motion (scrolling, the focus ring) has settled.
    for i in 0..4u64 {
        let t = now_ms + i * 500;
        app.tick(ui, t);
        app.render(&mut canvas, ui, t);
    }
    Some(canvas)
}
