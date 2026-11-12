//! Theme tokens: colour roles, chat themes, the layout scale, and the type/spacing scale derived
//! from it. Light and dark palettes load from `assets/theme/*.toml`; chat themes are a list of
//! user-bubble swatches. See `docs/design.md` for the rationale behind each value.

use crate::color::Color;
use serde::Deserialize;

/// Appearance mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Light palette.
    Light,
    /// Dark palette.
    Dark,
}

/// Avatar size (mirrors the app's choice list).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvatarSize {
    /// Hidden entirely.
    Hidden,
    /// Small.
    Small,
    /// Medium.
    Medium,
    /// Large.
    Large,
    /// Extra large.
    ExtraLarge,
}

impl AvatarSize {
    /// Diameter in logical units (multiplied by scale at use).
    pub fn diameter(self) -> f32 {
        match self {
            AvatarSize::Hidden => 0.0,
            AvatarSize::Small => 26.0,
            AvatarSize::Medium => 34.0,
            AvatarSize::Large => 44.0,
            AvatarSize::ExtraLarge => 56.0,
        }
    }

    /// All sizes in menu order.
    pub const ALL: &'static [AvatarSize] = &[
        AvatarSize::Small,
        AvatarSize::Medium,
        AvatarSize::Large,
        AvatarSize::ExtraLarge,
        AvatarSize::Hidden,
    ];

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            AvatarSize::Hidden => "Hidden",
            AvatarSize::Small => "Small",
            AvatarSize::Medium => "Medium",
            AvatarSize::Large => "Large",
            AvatarSize::ExtraLarge => "Extra large",
        }
    }
}

/// The colour roles used across the kit (resolved for one mode).
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Palette {
    /// Page background.
    pub page: Color,
    /// Card/surface background.
    pub surface: Color,
    /// Subtle fill (agent bubble, composer, secondary button).
    pub subtle: Color,
    /// A slightly stronger subtle fill (pressed, hover).
    pub subtle_strong: Color,
    /// Primary text.
    pub text: Color,
    /// Secondary text.
    pub text_muted: Color,
    /// Accent (links, primary buttons, focus).
    pub accent: Color,
    /// Text drawn on the accent colour.
    pub on_accent: Color,
    /// Hairline divider.
    pub divider: Color,
    /// Error/danger.
    pub danger: Color,
    /// Warning.
    pub warn: Color,
    /// Success/ok.
    pub ok: Color,
    /// Default text colour on a user bubble (overridden per chat theme if needed).
    pub on_bubble: Color,
}

/// A named chat theme (user bubble swatch).
#[derive(Clone, Copy, Debug)]
pub struct ChatTheme {
    /// Identifier used in settings and TOML.
    pub id: &'static str,
    /// Display label.
    pub label: &'static str,
    /// Swatch colour in light mode.
    pub light: Color,
    /// Swatch colour in dark mode.
    pub dark: Color,
}

/// The eight chat themes plus the avatar-colours default, in the app's order.
pub const CHAT_THEMES: &[ChatTheme] = &[
    ChatTheme {
        id: "avatar",
        label: "Avatar",
        light: Color::rgb(0xD9, 0xCE, 0xBE),
        dark: Color::rgb(0x4A, 0x44, 0x3C),
    },
    ChatTheme {
        id: "default",
        label: "Default",
        light: Color::rgb(0xCB, 0xE5, 0xFF),
        // Dark mode uses the Muse app's saturated blue with light text.
        dark: Color::rgb(0x2B, 0x68, 0xC8),
    },
    ChatTheme {
        id: "monochrome",
        label: "Monochrome",
        light: Color::rgb(0x00, 0x00, 0x00),
        dark: Color::rgb(0xE8, 0xE8, 0xEA),
    },
    ChatTheme {
        id: "beige",
        label: "Beige",
        light: Color::rgb(0xE4, 0xDB, 0xD2),
        dark: Color::rgb(0x4A, 0x40, 0x36),
    },
    ChatTheme {
        id: "blue",
        label: "Blue",
        light: Color::rgb(0xB8, 0xDB, 0xFE),
        dark: Color::rgb(0x2A, 0x57, 0xA6),
    },
    ChatTheme {
        id: "purple",
        label: "Purple",
        light: Color::rgb(0xDB, 0xC6, 0xF1),
        dark: Color::rgb(0x40, 0x30, 0x58),
    },
    ChatTheme {
        id: "pink",
        label: "Pink",
        light: Color::rgb(0xFD, 0xBA, 0xDF),
        dark: Color::rgb(0x5A, 0x2E, 0x48),
    },
    ChatTheme {
        id: "orange",
        label: "Orange",
        light: Color::rgb(0xFF, 0xD9, 0xB8),
        dark: Color::rgb(0x5A, 0x3C, 0x22),
    },
    ChatTheme {
        id: "green",
        label: "Green",
        light: Color::rgb(0xE0, 0xF0, 0xC7),
        dark: Color::rgb(0x34, 0x45, 0x26),
    },
];

/// Looks up a chat theme by id, falling back to the default.
pub fn chat_theme(id: &str) -> &'static ChatTheme {
    CHAT_THEMES
        .iter()
        .find(|t| t.id == id)
        .unwrap_or(&CHAT_THEMES[1])
}

#[derive(Deserialize)]
struct PaletteFile {
    palette: Palette,
}

/// A resolved theme: palette, scale and the derived metric scale.
#[derive(Clone)]
pub struct Theme {
    /// The active mode.
    pub mode: Mode,
    /// Resolved colour roles.
    pub pal: Palette,
    /// The chat theme id.
    pub chat_theme: &'static ChatTheme,
    /// Avatar size.
    pub avatar: AvatarSize,
    /// Layout scale in use: the screen's base scale times the size choice.
    pub scale: f32,
    /// The screen's own scale (2.0 at 1024×768, ~1.25 at 640×480).
    pub base_scale: f32,
    /// The user's size choice.
    pub ui_size: UiSize,
    /// Derived metrics.
    pub metrics: Metrics,
    /// Whether animations are reduced.
    pub reduce_motion: bool,
}

/// The user's size choice: scales every metric (text, icons, rows, bars) together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UiSize {
    /// More on screen.
    Compact,
    /// The default.
    #[default]
    Normal,
    /// Larger text and targets.
    Comfortable,
}

impl UiSize {
    /// All sizes in menu order.
    pub const ALL: [UiSize; 3] = [UiSize::Compact, UiSize::Normal, UiSize::Comfortable];

    /// Multiplier on the layout scale.
    pub fn factor(self) -> f32 {
        match self {
            UiSize::Compact => 0.88,
            UiSize::Normal => 1.0,
            UiSize::Comfortable => 1.14,
        }
    }

    /// Label for settings.
    pub fn label(self) -> &'static str {
        match self {
            UiSize::Compact => "Compact",
            UiSize::Normal => "Normal",
            UiSize::Comfortable => "Comfortable",
        }
    }

    /// Settings value (`appearance.ui_size`).
    pub fn id(self) -> &'static str {
        match self {
            UiSize::Compact => "compact",
            UiSize::Normal => "normal",
            UiSize::Comfortable => "comfortable",
        }
    }

    /// Parses a settings value.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }
}

/// Pixel metrics derived from the scale. All values are physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    /// Base 8-unit grid step.
    pub grid: f32,
    /// Title text size.
    pub title: f32,
    /// Body text size.
    pub body: f32,
    /// Caption text size.
    pub caption: f32,
    /// Small label size.
    pub small: f32,
    /// Standard corner radius for cards.
    pub radius_card: f32,
    /// Pill/button radius (also icon-button radius when square).
    pub radius_pill: f32,
    /// Hairline thickness.
    pub hairline: f32,
    /// Focus ring thickness.
    pub focus: f32,
    /// Top bar height.
    pub topbar: f32,
    /// Tab rail height (0: the tabs live in the top bar).
    pub tabbar: f32,
    /// Hint bar height.
    pub hintbar: f32,
    /// Standard touch/focus target height for rows and buttons.
    pub row: f32,
    /// Default content padding.
    pub pad: f32,
}

impl Metrics {
    fn derive(scale: f32) -> Self {
        let r = |u: f32| (u * scale).round();
        Metrics {
            grid: r(8.0),
            title: r(14.0),
            body: r(11.0),
            // Floors keep the smallest text readable at the Compact size (about 400 ppi).
            caption: r(9.0).max(16.0),
            small: r(8.0).max(15.0),
            radius_card: r(12.0),
            radius_pill: r(999.0).min(1000.0),
            hairline: (scale * 0.75).max(1.0).round(),
            focus: r(1.6).max(2.0),
            topbar: r(21.0),
            tabbar: 0.0,
            hintbar: r(20.0),
            row: r(27.0),
            pad: r(10.0),
        }
    }
}

/// Errors while loading a palette.
#[derive(Debug)]
pub struct ThemeError(pub String);

impl std::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "theme: {}", self.0)
    }
}

impl std::error::Error for ThemeError {}

impl Palette {
    /// Parses a palette from a `[palette]` TOML document.
    pub fn from_toml(src: &str) -> Result<Palette, ThemeError> {
        toml::from_str::<PaletteFile>(src)
            .map(|f| f.palette)
            .map_err(|e| ThemeError(e.to_string()))
    }

    /// The built-in light palette (also mirrored in `assets/theme/light.toml`).
    pub fn light() -> Palette {
        Palette {
            page: Color::rgb(0xF3, 0xF4, 0xF5),
            surface: Color::WHITE,
            subtle: Color::rgb(0xE9, 0xEA, 0xEB),
            subtle_strong: Color::rgb(0xDD, 0xDE, 0xE1),
            text: Color::rgb(0x11, 0x11, 0x12),
            text_muted: Color::rgb(0x6B, 0x6F, 0x76),
            accent: Color::rgb(0x00, 0x64, 0xD4),
            on_accent: Color::WHITE,
            divider: Color::rgb(0xDD, 0xDE, 0xE1),
            danger: Color::rgb(0xD9, 0x30, 0x25),
            warn: Color::rgb(0xB5, 0x7A, 0x00),
            ok: Color::rgb(0x1F, 0x9D, 0x55),
            on_bubble: Color::rgb(0x11, 0x11, 0x12),
        }
    }

    /// The built-in dark palette (also mirrored in `assets/theme/dark.toml`).
    pub fn dark() -> Palette {
        Palette {
            page: Color::rgb(0x0F, 0x10, 0x11),
            surface: Color::rgb(0x1B, 0x1C, 0x1E),
            subtle: Color::rgb(0x2A, 0x2B, 0x2E),
            subtle_strong: Color::rgb(0x37, 0x38, 0x3C),
            text: Color::rgb(0xF3, 0xF4, 0xF5),
            text_muted: Color::rgb(0xA0, 0xA4, 0xAB),
            accent: Color::rgb(0x4C, 0x9B, 0xFF),
            on_accent: Color::rgb(0x08, 0x0C, 0x12),
            divider: Color::rgb(0x34, 0x35, 0x39),
            danger: Color::rgb(0xFF, 0x6B, 0x5E),
            warn: Color::rgb(0xF0, 0xB8, 0x3A),
            ok: Color::rgb(0x55, 0xD6, 0x8A),
            on_bubble: Color::rgb(0xF3, 0xF4, 0xF5),
        }
    }
}

impl Theme {
    /// Builds a theme from built-in palettes.
    pub fn new(mode: Mode, scale: f32) -> Self {
        let pal = match mode {
            Mode::Light => Palette::light(),
            Mode::Dark => Palette::dark(),
        };
        Theme {
            mode,
            pal,
            chat_theme: &CHAT_THEMES[1],
            avatar: AvatarSize::Large,
            scale,
            base_scale: scale,
            ui_size: UiSize::Normal,
            metrics: Metrics::derive(scale),
            reduce_motion: false,
        }
    }

    /// Applies a size choice, rescaling every metric.
    pub fn set_ui_size(&mut self, size: UiSize) {
        self.ui_size = size;
        self.scale = self.base_scale * size.factor();
        self.metrics = Metrics::derive(self.scale);
    }

    /// The user-bubble colour for the active chat theme and mode.
    pub fn user_bubble(&self) -> Color {
        match self.mode {
            Mode::Light => self.chat_theme.light,
            Mode::Dark => self.chat_theme.dark,
        }
    }

    /// Text colour that reads on the user bubble (picks black/white by contrast).
    pub fn on_user_bubble(&self) -> Color {
        let b = self.user_bubble();
        let dark = Color::rgb(0x11, 0x11, 0x12);
        let light = Color::rgb(0xFA, 0xFA, 0xFB);
        if b.contrast(dark) >= b.contrast(light) {
            dark
        } else {
            light
        }
    }

    /// Sets the chat theme by id.
    pub fn set_chat_theme(&mut self, id: &str) {
        self.chat_theme = chat_theme(id);
    }

    /// Switches mode, keeping scale and other settings.
    pub fn set_mode(&mut self, mode: Mode) {
        self.pal = match mode {
            Mode::Light => Palette::light(),
            Mode::Dark => Palette::dark(),
        };
        self.mode = mode;
    }

    /// Replaces the palette (e.g. loaded from TOML) for the current mode.
    pub fn with_palette(mut self, pal: Palette) -> Self {
        self.pal = pal;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_palettes_match_toml() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/theme");
        let light =
            Palette::from_toml(&std::fs::read_to_string(dir.join("light.toml")).unwrap()).unwrap();
        assert_eq!(light.accent, Palette::light().accent);
        assert_eq!(light.page, Palette::light().page);
        let dark =
            Palette::from_toml(&std::fs::read_to_string(dir.join("dark.toml")).unwrap()).unwrap();
        assert_eq!(dark.page, Palette::dark().page);
    }

    #[test]
    fn bubble_text_has_contrast() {
        for mode in [Mode::Light, Mode::Dark] {
            let mut t = Theme::new(mode, 2.0);
            for ct in CHAT_THEMES {
                t.set_chat_theme(ct.id);
                let c = t.on_user_bubble().contrast(t.user_bubble());
                assert!(c > 3.0, "{} {:?} contrast {c}", ct.id, mode);
            }
        }
    }

    #[test]
    fn metrics_scale() {
        let big = Metrics::derive(2.0);
        let small = Metrics::derive(1.25);
        assert!(big.body > small.body);
        assert!(big.topbar > small.topbar);
    }
}
