//! Device-local settings: typed values, validation, atomic persistence and change callbacks.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::fsutil::{quarantine, read_json, write_json_atomic};
use crate::StoreError;

macro_rules! choice {
    ($(#[$m:meta])* $name:ident { $($var:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $(
                #[doc = concat!("`", $s, "`")]
                #[serde(rename = $s)]
                $var,
            )+
        }
    };
}

choice! {
    /// `appearance.mode`.
    AppearanceMode { System = "system", Light = "light", Dark = "dark" }
}
choice! {
    /// `appearance.chat_theme`.
    ChatTheme {
        Avatar = "avatar", Default = "default", Monochrome = "monochrome", Beige = "beige",
        Blue = "blue", Purple = "purple", Pink = "pink", Orange = "orange", Green = "green",
    }
}
choice! {
    /// `appearance.avatar_size`.
    AvatarSize { Small = "small", Medium = "medium", Large = "large", Xl = "xl", Hidden = "hidden" }
}
choice! {
    /// `appearance.ui_size`.
    UiSize { Compact = "compact", Normal = "normal", Comfortable = "comfortable", Large = "large" }
}
/// `voice.speak_replies`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpeakReplies {
    /// Speak replies to voice notes. The former "always" choice had this same behavior.
    #[serde(rename = "voice_only", alias = "always")]
    VoiceOnly,
    /// Keep replies silent.
    #[serde(rename = "never")]
    Never,
}
choice! {
    /// `leds.mode`.
    LedMode { Off = "off", Presence = "presence", PresenceDim = "presence_dim" }
}

/// All settings with their defaults. Serialized with dotted key names, as on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `appearance.mode`.
    #[serde(rename = "appearance.mode")]
    pub appearance_mode: AppearanceMode,
    /// `appearance.chat_theme`.
    #[serde(rename = "appearance.chat_theme")]
    pub chat_theme: ChatTheme,
    /// `appearance.avatar_size`.
    #[serde(rename = "appearance.avatar_size")]
    pub avatar_size: AvatarSize,
    /// `appearance.ui_size`.
    #[serde(rename = "appearance.ui_size")]
    pub ui_size: UiSize,
    /// `voice.call_greeting`: Muse speaks first on a call.
    #[serde(rename = "voice.call_greeting")]
    pub call_greeting: bool,
    /// `appearance.reduce_motion`.
    #[serde(rename = "appearance.reduce_motion")]
    pub reduce_motion: bool,
    /// `voice.speak_replies`.
    #[serde(rename = "voice.speak_replies")]
    pub speak_replies: SpeakReplies,
    /// `leds.mode`.
    #[serde(rename = "leds.mode")]
    pub leds_mode: LedMode,
    /// `leds.never_touch`.
    #[serde(rename = "leds.never_touch")]
    pub leds_never_touch: bool,
    /// `haptics.enabled`.
    #[serde(rename = "haptics.enabled")]
    pub haptics: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance_mode: AppearanceMode::System,
            chat_theme: ChatTheme::Avatar,
            avatar_size: AvatarSize::Large,
            ui_size: UiSize::Normal,
            call_greeting: false,
            reduce_motion: false,
            // Voice notes are speech-to-text by default; voice-to-voice is opt-in.
            speak_replies: SpeakReplies::Never,
            leds_mode: LedMode::Presence,
            leds_never_touch: false,
            haptics: true,
        }
    }
}

fn invalid(key: &str, reason: &str) -> StoreError {
    StoreError::InvalidSetting {
        key: key.to_owned(),
        reason: reason.to_owned(),
    }
}

impl Settings {
    /// Every key, in documentation order.
    pub const KEYS: [&'static str; 10] = [
        "appearance.mode",
        "appearance.chat_theme",
        "appearance.avatar_size",
        "appearance.ui_size",
        "voice.call_greeting",
        "appearance.reduce_motion",
        "voice.speak_replies",
        "leds.mode",
        "leds.never_touch",
        "haptics.enabled",
    ];

    /// All settings as a key/value map.
    pub fn to_map(&self) -> Map<String, Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        }
    }

    /// Sets one key after validating its type and allowed values.
    pub fn set_key(&mut self, key: &str, value: &Value) -> Result<(), StoreError> {
        if !Self::KEYS.contains(&key) {
            return Err(StoreError::UnknownSetting(key.to_owned()));
        }
        let mut map = self.to_map();
        map.insert(key.to_owned(), value.clone());
        let next: Settings =
            serde_json::from_value(Value::Object(map)).map_err(|e| invalid(key, &e.to_string()))?;
        *self = next;
        Ok(())
    }

    /// Builds settings from stored values; unknown keys are ignored and invalid values fall
    /// back to the default of that key.
    pub fn from_map_lenient(map: &Map<String, Value>) -> Self {
        let mut s = Self::default();
        for (k, v) in map {
            let _ = s.set_key(k, v);
        }
        s
    }
}

type Listener = Box<dyn Fn(&Map<String, Value>) + Send + Sync>;

/// Persistent settings with change notification.
pub struct SettingsStore {
    path: PathBuf,
    current: Settings,
    listeners: Vec<Listener>,
    recovered: bool,
}

impl std::fmt::Debug for SettingsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsStore")
            .field("path", &self.path)
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

impl SettingsStore {
    /// Loads `path`, or defaults when it does not exist. A file that is not valid JSON is moved
    /// to `<path>.corrupt` and defaults are used ([`recovered`](Self::recovered) reports it).
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let mut recovered = false;
        let current = match read_json::<Value>(path) {
            Ok(Some(Value::Object(m))) => Settings::from_map_lenient(&m),
            Ok(Some(_)) | Err(StoreError::Corrupt { .. }) => {
                quarantine(path);
                recovered = true;
                Settings::default()
            }
            Ok(None) => Settings::default(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            path: path.to_owned(),
            current,
            listeners: Vec::new(),
            recovered,
        })
    }

    /// Whether the settings file was unusable and was replaced by defaults.
    pub fn recovered(&self) -> bool {
        self.recovered
    }

    /// The current settings.
    pub fn settings(&self) -> &Settings {
        &self.current
    }

    /// Registers a callback that receives the changed keys (with new values) after each
    /// successful [`set`](Self::set).
    pub fn on_change(&mut self, f: impl Fn(&Map<String, Value>) + Send + Sync + 'static) {
        self.listeners.push(Box::new(f));
    }

    /// Values for `keys`, or all of them when `None`. Unknown keys are an error.
    pub fn get(&self, keys: Option<&[String]>) -> Result<Map<String, Value>, StoreError> {
        let all = self.current.to_map();
        let Some(keys) = keys else { return Ok(all) };
        let mut out = Map::new();
        for k in keys {
            let v = all
                .get(k)
                .ok_or_else(|| StoreError::UnknownSetting(k.clone()))?;
            out.insert(k.clone(), v.clone());
        }
        Ok(out)
    }

    /// Validates and applies `values` all-or-nothing, persists atomically, then notifies
    /// listeners with the keys whose value actually changed. Returns those keys. Nothing is
    /// written or notified when nothing changed.
    pub fn set(&mut self, values: &Map<String, Value>) -> Result<Map<String, Value>, StoreError> {
        let mut next = self.current.clone();
        for (k, v) in values {
            next.set_key(k, v)?;
        }
        let before = self.current.to_map();
        let after = next.to_map();
        let changed: Map<String, Value> = after
            .iter()
            .filter(|(k, v)| before.get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if changed.is_empty() {
            return Ok(changed);
        }
        write_json_atomic(&self.path, &after, 0o644)?;
        self.current = next;
        for l in &self.listeners {
            l(&changed);
        }
        Ok(changed)
    }
}
