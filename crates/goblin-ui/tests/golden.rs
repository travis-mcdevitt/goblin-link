//! Golden-image tests: every screen is rendered head-lessly in light and dark at 1024×768 and
//! 640×480 and compared against a committed PNG. Set `GOBLIN_REGEN=1` to (re)generate the goldens
//! after an intentional visual change; the comparison is tolerance-based so tiny anti-aliasing
//! differences between machines do not fail the build.

use goblin_ui::render_screen;
use goblin_ui_kit::{Image, Mode, Ui};
use std::path::PathBuf;

/// (file-stem, screen-name, mode)
const CASES: &[(&str, &str, Mode)] = &[
    ("chat-light", "chat", Mode::Light),
    ("chat-dark", "chat", Mode::Dark),
    ("settings-light", "settings", Mode::Light),
    ("settings-dark", "settings", Mode::Dark),
    ("settings-alerts", "settings-alerts", Mode::Light),
    ("settings-access", "settings-access", Mode::Dark),
    ("build-sheet", "build-sheet", Mode::Light),
    ("menu", "menu", Mode::Light),
    ("controls", "controls", Mode::Dark),
    ("settings-device", "settings-device", Mode::Light),
    ("pairing-token", "pairing-token", Mode::Light),
    ("welcome", "welcome", Mode::Dark),
    ("chat-request", "chat@request", Mode::Light),
    ("chat-request-setting", "chat@request-setting", Mode::Dark),
    ("profiles", "profiles", Mode::Light),
    ("profile-sheet", "profile-sheet", Mode::Dark),
    ("pairing", "pairing", Mode::Light),
    ("panel", "panel", Mode::Dark),
    ("markdown", "chat@markdown", Mode::Light),
    ("call", "call", Mode::Dark),
    ("keyboard", "keyboard", Mode::Light),
    ("voice", "voice", Mode::Light),
    ("recording", "recording", Mode::Dark),
    ("media", "chat@media", Mode::Dark),
    ("viewer", "viewer@media", Mode::Dark),
    ("confirm", "confirm", Mode::Light),
    ("toast", "toast", Mode::Light),
    ("inputtest", "inputtest", Mode::Dark),
];

const SIZES: &[(u32, u32)] = &[(1024, 768), (640, 480)];

/// A fixed clock so streaming/animated content is deterministic.
const NOW_MS: u64 = 1500;

/// Screens checked at the compact and comfortable sizes, in both modes between them.
const SIZED_CASES: &[(&str, &str, Mode, goblin_ui_kit::UiSize)] = &[
    (
        "chat-compact-dark",
        "chat",
        Mode::Dark,
        goblin_ui_kit::UiSize::Compact,
    ),
    (
        "chat-comfortable-light",
        "chat",
        Mode::Light,
        goblin_ui_kit::UiSize::Comfortable,
    ),
    (
        "settings-compact-light",
        "settings",
        Mode::Light,
        goblin_ui_kit::UiSize::Compact,
    ),
    (
        "settings-comfortable-dark",
        "settings",
        Mode::Dark,
        goblin_ui_kit::UiSize::Comfortable,
    ),
    (
        "panel-compact-light",
        "panel",
        Mode::Light,
        goblin_ui_kit::UiSize::Compact,
    ),
    (
        "panel-comfortable-dark",
        "panel",
        Mode::Dark,
        goblin_ui_kit::UiSize::Comfortable,
    ),
    (
        "keyboard-compact-dark",
        "keyboard",
        Mode::Dark,
        goblin_ui_kit::UiSize::Compact,
    ),
    (
        "keyboard-comfortable-light",
        "keyboard",
        Mode::Light,
        goblin_ui_kit::UiSize::Comfortable,
    ),
    (
        "call-comfortable-dark",
        "call",
        Mode::Dark,
        goblin_ui_kit::UiSize::Comfortable,
    ),
];

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Mean absolute per-channel difference between two equal-size images, 0..=255.
fn mean_diff(a: &Image, b: &Image) -> f64 {
    assert_eq!((a.w, a.h), (b.w, b.h));
    let mut sum = 0u64;
    for (pa, pb) in a.pixels().iter().zip(b.pixels()) {
        let ca = goblin_ui_kit::Color::unpack(*pa);
        let cb = goblin_ui_kit::Color::unpack(*pb);
        sum += (ca.r as i32 - cb.r as i32).unsigned_abs() as u64;
        sum += (ca.g as i32 - cb.g as i32).unsigned_abs() as u64;
        sum += (ca.b as i32 - cb.b as i32).unsigned_abs() as u64;
    }
    sum as f64 / (a.pixels().len() as f64 * 3.0)
}

#[test]
fn golden_screens() {
    let regen = std::env::var("GOBLIN_REGEN").is_ok();
    let dir = golden_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut ui = Ui::new().expect("ui");
    let mut failures = Vec::new();
    let mut generated = 0;

    for &(w, h) in SIZES {
        for &(stem, screen, mode) in CASES {
            let canvas = render_screen(&mut ui, screen, w, h, mode, NOW_MS, Default::default())
                .unwrap_or_else(|| panic!("unknown screen {screen}"));
            let img = canvas.to_image();
            let path = dir.join(format!("{stem}-{w}x{h}.png"));
            if regen || !path.exists() {
                img.save_png(&path).unwrap();
                generated += 1;
                continue;
            }
            let golden = Image::load_png(&path).unwrap_or_else(|e| panic!("load {path:?}: {e}"));
            if (golden.w, golden.h) != (w, h) {
                failures.push(format!("{stem} {w}x{h}: golden size mismatch"));
                continue;
            }
            let diff = mean_diff(&img, &golden);
            // ~1.2 of 255 per channel: generous enough for cross-machine AA, tight enough to
            // catch real layout/colour regressions.
            if diff > 1.2 {
                let out = dir.join(format!("{stem}-{w}x{h}.actual.png"));
                img.save_png(&out).ok();
                failures.push(format!(
                    "{stem} {w}x{h}: mean diff {diff:.3} (> 1.2); wrote {out:?}"
                ));
            }
        }
    }

    // The same layouts at the other two sizes (Brick resolution only).
    for &(stem, screen, mode, size) in SIZED_CASES {
        let (w, h) = (1024, 768);
        let canvas = render_screen(&mut ui, screen, w, h, mode, NOW_MS, size)
            .unwrap_or_else(|| panic!("unknown screen {screen}"));
        let img = canvas.to_image();
        let path = dir.join(format!("{stem}-{w}x{h}.png"));
        if regen || !path.exists() {
            img.save_png(&path).unwrap();
            generated += 1;
            continue;
        }
        let golden = Image::load_png(&path).unwrap_or_else(|e| panic!("load {path:?}: {e}"));
        let diff = mean_diff(&img, &golden);
        if (golden.w, golden.h) != (w, h) || diff > 1.2 {
            let out = dir.join(format!("{stem}-{w}x{h}.actual.png"));
            img.save_png(&out).ok();
            failures.push(format!(
                "{stem} {w}x{h}: mean diff {diff:.3}; wrote {out:?}"
            ));
        }
    }

    if generated > 0 {
        eprintln!("generated {generated} golden image(s); re-run to compare");
    }
    assert!(
        failures.is_empty(),
        "golden mismatches:\n{}",
        failures.join("\n")
    );
}
