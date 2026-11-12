//! Outline icons from Lucide (ISC licence, see `assets/icons/lucide/LICENSE`).
//!
//! The SVG sources are embedded (about 30 KB) and rasterised on first use at the exact pixel
//! size requested, then cached as coverage masks. That keeps icons sharp at every display scale
//! without shipping pre-baked atlases per scale.

use crate::image::Mask;
use crate::raster::{fill_coverage, stroke_coverage, to_mask, Path};
use std::collections::HashMap;

macro_rules! icons {
    ($($variant:ident => $file:literal,)*) => {
        /// An icon from the bundled set.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[allow(missing_docs)]
        pub enum Icon { $($variant,)* }

        impl Icon {
            /// Every icon, for previews and tests.
            pub const ALL: &'static [Icon] = &[$(Icon::$variant,)*];

            fn source(self) -> &'static str {
                match self {
                    $(Icon::$variant => include_str!(concat!("../../../assets/icons/lucide/", $file, ".svg")),)*
                }
            }
        }
    };
}

icons! {
    Chat => "message-circle",
    Workshop => "gamepad-2",
    Feed => "newspaper",
    Ideas => "lightbulb",
    Goals => "square-check",
    Library => "shapes",
    Bell => "bell",
    Settings => "settings",
    Activity => "activity",
    Battery => "battery",
    BatteryLow => "battery-low",
    BatteryMedium => "battery-medium",
    BatteryFull => "battery-full",
    BatteryCharging => "battery-charging",
    Wifi => "wifi",
    WifiOff => "wifi-off",
    Link => "link",
    Unlink => "unlink",
    User => "user",
    Shield => "shield",
    Mic => "mic",
    MicOff => "mic-off",
    Phone => "phone",
    PhoneOff => "phone-off",
    Volume => "volume-2",
    Headphones => "headphones",
    Plus => "plus",
    ChevronRight => "chevron-right",
    ChevronLeft => "chevron-left",
    Check => "check",
    X => "x",
    Send => "send",
    Keyboard => "keyboard",
    Delete => "delete",
    Alert => "circle-exclamation-point",
    CircleX => "circle-x",
    Info => "info",
    Refresh => "refresh-cw",
    Clock => "clock",
    Cpu => "cpu",
    Storage => "hard-drive",
    Thermometer => "thermometer",
    Terminal => "terminal",
    Compose => "square-pen",
    Search => "search",
    Sun => "sun",
    Moon => "moon",
    Palette => "palette",
    Device => "smartphone",
    File => "file-text",
    Image => "image",
    CloudOff => "cloud-off",
    Inbox => "inbox",
    Spinner => "loader-circle",
    Enter => "corner-down-left",
    Bluetooth => "bluetooth",
    Menu => "menu",
    SideChats => "messages-square",
    LogOut => "log-out",
    ArrowUp => "arrow-up",
    CircleCheck => "circle-check",
    Sparkles => "sparkles",
    More => "ellipsis-vertical",
    ThumbsUp => "thumbs-up",
    ThumbsDown => "thumbs-down",
}

/// How an icon is rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconStyle {
    /// Outline strokes (the default).
    Outline,
    /// Closed shapes filled; inner detail strokes are knocked out (used for the active tab).
    Filled,
}

/// Stroke width in 24-unit icon space. Lucide's default is 2; 1.75 reads closer to the app's
/// lighter outline icons at handheld sizes.
const STROKE: f32 = 1.75;

/// Rasterises icons on demand and caches the masks.
#[derive(Default)]
pub struct IconCache {
    paths: HashMap<Icon, Path>,
    masks: HashMap<(Icon, u16, IconStyle), Mask>,
}

impl IconCache {
    /// Returns the mask for `icon` at `size` pixels square.
    pub fn get(&mut self, icon: Icon, size: u16, style: IconStyle) -> &Mask {
        let size = size.max(4);
        let paths = &mut self.paths;
        self.masks.entry((icon, size, style)).or_insert_with(|| {
            let path = paths
                .entry(icon)
                .or_insert_with(|| crate::svg::parse_icon(icon.source()).unwrap_or_default());
            rasterize(path, size as u32, style)
        })
    }
}

fn rasterize(path: &Path, size: u32, style: IconStyle) -> Mask {
    let scale = size as f32 / 24.0;
    let width = STROKE * scale;
    match style {
        IconStyle::Outline => to_mask(
            &stroke_coverage(path, size, size, scale, (0.0, 0.0), width, |_| true),
            size,
            size,
        ),
        IconStyle::Filled => {
            let fill = fill_coverage(path, size, size, scale, (0.0, 0.0));
            let closed = stroke_coverage(path, size, size, scale, (0.0, 0.0), width, |s| s.closed);
            let open = stroke_coverage(path, size, size, scale, (0.0, 0.0), width, |s| !s.closed);
            let cov: Vec<f32> = (0..fill.len())
                .map(|i| {
                    let base = fill[i].max(closed[i]);
                    if fill[i] > 0.5 {
                        base * (1.0 - open[i])
                    } else {
                        base.max(open[i])
                    }
                })
                .collect();
            to_mask(&cov, size, size)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_parses_and_has_ink() {
        let mut cache = IconCache::default();
        for &icon in Icon::ALL {
            crate::svg::parse_icon(icon.source()).unwrap_or_else(|e| panic!("{icon:?}: {e}"));
            let m = cache.get(icon, 32, IconStyle::Outline);
            let ink: u32 = m.data.iter().map(|&v| v as u32).sum();
            assert!(ink > 255 * 20, "{icon:?} looks empty");
        }
    }

    #[test]
    fn filled_goal_icon_knocks_out_the_check() {
        let mut cache = IconCache::default();
        let m = cache.get(Icon::Goals, 48, IconStyle::Filled).clone();
        // Box interior away from the check mark is solid.
        assert!(m.data[(10 * 48 + 10) as usize] > 200);
        let o = cache.get(Icon::Goals, 48, IconStyle::Outline);
        assert!(o.data[(10 * 48 + 10) as usize] < 30);
    }
}
