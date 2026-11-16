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
    ("controls", "controls", Mode::Dark),
    ("settings-device", "settings-device", Mode::Light),
    ("pairing-token", "pairing-token", Mode::Light),
    ("welcome", "welcome", Mode::Dark),
    ("chat-request", "chat@request", Mode::Light),
    ("chat-request-setting", "chat@request-setting", Mode::Dark),
    ("chat-approval", "chat@request-approval", Mode::Dark),
    ("chat-approval-long", "chat@request-long", Mode::Light),
    ("chat-approval-lines", "chat@request-lines", Mode::Dark),
    ("chat-question-long", "chat@request-question", Mode::Dark),
    ("profiles", "profiles", Mode::Light),
    ("profile-sheet", "profile-sheet", Mode::Dark),
    ("pairing", "pairing", Mode::Light),
    ("pairing-dark", "pairing", Mode::Dark),
    ("pairing-token-dark", "pairing-token", Mode::Dark),
    ("provider", "provider", Mode::Light),
    ("provider-dark", "provider", Mode::Dark),
    ("pairing-hermes", "pairing-hermes", Mode::Light),
    ("pairing-hermes-dark", "pairing-hermes", Mode::Dark),
    (
        "pairing-hermes-connecting",
        "pairing-hermes-connecting",
        Mode::Light,
    ),
    (
        "pairing-hermes-connecting-dark",
        "pairing-hermes-connecting",
        Mode::Dark,
    ),
    ("chat-hermes", "chat@hermes", Mode::Dark),
    ("chat-hermes-light", "chat@hermes", Mode::Light),
    ("commands", "commands", Mode::Dark),
    (
        "settings-alerts-hermes",
        "settings-alerts@hermes",
        Mode::Light,
    ),
    ("pairing-failed", "pairing-failed", Mode::Light),
    ("pairing-failed-dark", "pairing-failed", Mode::Dark),
    ("panel", "panel", Mode::Dark),
    ("markdown", "chat@markdown", Mode::Light),
    ("call", "call", Mode::Dark),
    ("keyboard", "keyboard", Mode::Light),
    ("voice", "voice", Mode::Light),
    ("recording", "recording", Mode::Dark),
    ("recording-half", "recording-half", Mode::Light),
    ("recording-late", "recording-late", Mode::Dark),
    ("media", "chat@media", Mode::Dark),
    ("viewer", "viewer@media", Mode::Dark),
    ("confirm", "confirm", Mode::Light),
    ("toast", "toast", Mode::Light),
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

/// How two images of one size differ: the mean per-channel difference (0..=255), and how
/// many pixels differ plainly (by more than [`PLAIN`] on some channel).
fn difference(a: &Image, b: &Image) -> (f64, usize) {
    assert_eq!((a.w, a.h), (b.w, b.h));
    let (mut sum, mut plain) = (0u64, 0usize);
    for (pa, pb) in a.pixels().iter().zip(b.pixels()) {
        let ca = goblin_ui_kit::Color::unpack(*pa);
        let cb = goblin_ui_kit::Color::unpack(*pb);
        let d = [
            (ca.r as i32 - cb.r as i32).unsigned_abs(),
            (ca.g as i32 - cb.g as i32).unsigned_abs(),
            (ca.b as i32 - cb.b as i32).unsigned_abs(),
        ];
        sum += d.iter().map(|v| u64::from(*v)).sum::<u64>();
        if d.iter().any(|v| *v > PLAIN) {
            plain += 1;
        }
    }
    (sum as f64 / (a.pixels().len() as f64 * 3.0), plain)
}

/// A channel difference no rounding at an edge explains.
const PLAIN: u32 = 40;

/// Why `img` is not the stored image, if it is not. Two tests, because they catch different
/// things. The mean catches a screen that shifted in colour or layout as a whole. The count
/// of plainly different pixels catches a small patch that changed outright (a label reworded,
/// an icon swapped), which hardly moves the mean of a whole screen: a handful of pixels may
/// differ at an edge between machines, a word's worth may not.
fn mismatch(img: &Image, golden: &Image) -> Option<String> {
    if (golden.w, golden.h) != (img.w, img.h) {
        return Some("golden size mismatch".into());
    }
    let (mean, plain) = difference(img, golden);
    let allowed = img.pixels().len() / 20_000;
    if mean > 1.2 {
        Some(format!("mean diff {mean:.3} (> 1.2)"))
    } else if plain > allowed {
        Some(format!(
            "{plain} pixels plainly differ (> {allowed}); mean diff {mean:.3}"
        ))
    } else {
        None
    }
}

/// The comparison itself: a reworded label fails, edge rounding does not.
#[test]
fn a_changed_label_is_a_mismatch_and_edge_noise_is_not() {
    let (w, h) = (640u32, 480u32);
    let page = goblin_ui_kit::Color::rgb(0xF2, 0xF3, 0xF5).pack();
    let ink = goblin_ui_kit::Color::rgb(0x11, 0x11, 0x12).pack();
    let blank = Image::from_packed(w, h, vec![page; (w * h) as usize]);
    assert_eq!(mismatch(&blank, &blank), None);
    // A few pixels at an edge, as another machine's rounding might leave.
    let mut px = vec![page; (w * h) as usize];
    for p in px.iter_mut().take(10) {
        *p = ink;
    }
    assert_eq!(mismatch(&Image::from_packed(w, h, px), &blank), None);
    // A short word: about forty by ten pixels of text. The old test (mean only) let this by.
    let mut px = vec![page; (w * h) as usize];
    for y in 100..110u32 {
        for x in 200..240u32 {
            if (x + y) % 2 == 0 {
                px[(y * w + x) as usize] = ink;
            }
        }
    }
    let word = Image::from_packed(w, h, px);
    assert!(difference(&word, &blank).0 < 1.2, "under the old threshold");
    let why = mismatch(&word, &blank).expect("a changed label is seen");
    assert!(why.contains("plainly differ"), "{why}");
    // Another size is never the same picture.
    let small = Image::from_packed(2, 2, vec![page; 4]);
    assert!(mismatch(&small, &blank).is_some());
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
            if let Some(why) = mismatch(&img, &golden) {
                let out = dir.join(format!("{stem}-{w}x{h}.actual.png"));
                img.save_png(&out).ok();
                failures.push(format!("{stem} {w}x{h}: {why}; wrote {out:?}"));
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
        if let Some(why) = mismatch(&img, &golden) {
            let out = dir.join(format!("{stem}-{w}x{h}.actual.png"));
            img.save_png(&out).ok();
            failures.push(format!("{stem} {w}x{h}: {why}; wrote {out:?}"));
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
