//! The settings Muse can see and ask the owner to change.
//!
//! Muse reads their state with `handheld.settings` and asks for a change with
//! `handheld.ask`. Asking changes nothing: the handheld puts the question to the owner in
//! the chat, and the owner answers there with a button: no or yes, and, where a permission
//! is being widened, yes for an hour (after which it goes back to what it was). The values that open the handheld widest
//! (the shell, files anywhere, installing without being asked) cannot be asked for at all;
//! the owner sets those in Settings.

use goblin_ipc::types::{BuildAccess, Consent, ConsentUpdate, FileScope};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How long a "for an hour" answer lasts, in seconds.
pub const TEMPORARY_S: u64 = 3600;
/// After a no, the same setting cannot be asked about again for this long, in seconds.
pub const ASK_AGAIN_AFTER_S: u64 = 24 * 3600;
/// The longest reason Muse may give.
pub const MAX_REASON: usize = 240;

/// Where a setting is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    /// A preference of the app, on or off, under this key.
    Switch(&'static str),
    /// A preference of the app with named values, under this key.
    Choice(&'static str),
    /// One of the profile's permissions.
    Access(AccessField),
}

/// A permission of the profile being served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessField {
    /// Reading files.
    Read,
    /// Writing files.
    Write,
    /// How far the file commands reach.
    Reach,
    /// Commands the owner wrote.
    OwnCommands,
    /// The shell.
    Shell,
    /// Installing what Muse builds.
    Builds,
}

/// One setting: what it is called, what it does, the values it can take.
#[derive(Debug, Clone, Copy)]
pub struct Setting {
    /// The name Muse uses.
    pub id: &'static str,
    /// The name the owner sees.
    pub label: &'static str,
    /// What it does, in a sentence.
    pub means: &'static str,
    /// Its values, each with the words the owner sees.
    pub choices: &'static [(&'static str, &'static str)],
    /// Values only the owner can choose, in Settings.
    pub owner_only: &'static [&'static str],
    /// Where it is kept.
    pub store: Store,
}

const ON_OFF: &[(&str, &str)] = &[("off", "Off"), ("on", "On")];

/// Every setting Muse can see.
pub const SETTINGS: &[Setting] = &[
    Setting {
        id: "text_size",
        label: "Text size",
        means: "How large the app's text and controls are.",
        choices: &[
            ("compact", "Compact"),
            ("normal", "Normal"),
            ("comfortable", "Comfortable"),
        ],
        owner_only: &[],
        store: Store::Choice("appearance.ui_size"),
    },
    Setting {
        id: "mode",
        label: "Light or dark",
        means: "Whether the app is drawn light or dark.",
        choices: &[("light", "Light"), ("dark", "Dark")],
        owner_only: &[],
        store: Store::Choice("appearance.mode"),
    },
    Setting {
        id: "avatar_size",
        label: "Avatar size",
        means: "How large your avatar is shown over the chat; hidden shows none.",
        choices: &[
            ("hidden", "Hidden"),
            ("small", "Small"),
            ("medium", "Medium"),
            ("large", "Large"),
            ("xl", "Extra large"),
        ],
        owner_only: &[],
        store: Store::Choice("appearance.avatar_size"),
    },
    Setting {
        id: "reduce_motion",
        label: "Reduce motion",
        means: "Fewer animations in the app.",
        choices: ON_OFF,
        owner_only: &[],
        store: Store::Switch("appearance.reduce_motion"),
    },
    Setting {
        id: "lights",
        label: "Blink lights",
        means: "The handheld's lights blink when a reply arrives and the app is closed.",
        choices: &[("off", "Off"), ("presence", "On")],
        owner_only: &[],
        store: Store::Choice("leds.mode"),
    },
    Setting {
        id: "vibration",
        label: "Vibrate",
        means: "The handheld vibrates when a reply arrives.",
        choices: ON_OFF,
        owner_only: &[],
        store: Store::Switch("haptics.enabled"),
    },
    Setting {
        id: "speak_replies",
        label: "Speak replies",
        means: "Whether your replies are read out loud: never, only to a voice note, or always.",
        choices: &[
            ("never", "Never"),
            ("voice_only", "To voice notes"),
            ("always", "Always"),
        ],
        owner_only: &[],
        store: Store::Choice("voice.speak_replies"),
    },
    Setting {
        id: "greet_on_calls",
        label: "Greet on calls",
        means: "Whether you speak first when the owner calls.",
        choices: ON_OFF,
        owner_only: &[],
        store: Store::Switch("voice.call_greeting"),
    },
    Setting {
        id: "stay_connected",
        label: "Stay connected when closed",
        means: "Whether the handheld stays connected to you with the app closed. Off, you reach it only while the app is open.",
        choices: ON_OFF,
        owner_only: &[],
        store: Store::Switch("background.keep_connected"),
    },
    Setting {
        id: "read_files",
        label: "Read files",
        means: "file.read and file.list work, as far as file_reach goes.",
        choices: ON_OFF,
        owner_only: &[],
        store: Store::Access(AccessField::Read),
    },
    Setting {
        id: "write_files",
        label: "Write files",
        means: "file.write works, as far as file_reach goes.",
        choices: ON_OFF,
        owner_only: &[],
        store: Store::Access(AccessField::Write),
    },
    Setting {
        id: "file_reach",
        label: "File reach",
        means: "How far the file commands go: your own folder, the storage card too (games, saves, the owner's files), or any path on the device.",
        choices: &[
            ("own_folder", "Own folder"),
            ("storage", "SD card"),
            ("everything", "Everything"),
        ],
        owner_only: &["everything"],
        store: Store::Access(AccessField::Reach),
    },
    Setting {
        id: "own_commands",
        label: "Your commands",
        means: "Commands the owner wrote and put on the device are callable.",
        choices: ON_OFF,
        owner_only: &[],
        store: Store::Access(AccessField::OwnCommands),
    },
    Setting {
        id: "install_builds",
        label: "Install its builds",
        means: "Whether build.install works: no, ask (the owner approves each install), or freely.",
        choices: &[("no", "No"), ("ask", "Ask first"), ("freely", "Freely")],
        owner_only: &["freely"],
        store: Store::Access(AccessField::Builds),
    },
    Setting {
        id: "shell",
        label: "Run shell commands",
        means: "system.run works: any command, as the device's administrator, on the whole device.",
        choices: ON_OFF,
        owner_only: &["on"],
        store: Store::Access(AccessField::Shell),
    },
];

/// The setting called `id`.
pub fn find(id: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|s| s.id == id)
}

impl Setting {
    /// The words the owner sees for `value`.
    pub fn choice_label(&self, value: &str) -> &'static str {
        self.choices
            .iter()
            .find(|(v, _)| *v == value)
            .map_or("?", |(_, label)| label)
    }

    /// Whether `value` is one of its values.
    pub fn has(&self, value: &str) -> bool {
        self.choices.iter().any(|(v, _)| *v == value)
    }

    /// Whether changing it from `from` to `to` opens the handheld further: a permission
    /// moving up its list of values. Only such a change can be given for a while; a
    /// preference, or a permission being narrowed, is simply yes or no.
    pub fn widens(&self, from: &str, to: &str) -> bool {
        let rank = |v: &str| self.choices.iter().position(|(c, _)| *c == v);
        matches!(self.store, Store::Access(_)) && rank(to) > rank(from)
    }

    /// The values Muse may ask for.
    pub fn askable(&self) -> Vec<&'static str> {
        self.choices
            .iter()
            .map(|(v, _)| *v)
            .filter(|v| !self.owner_only.contains(v))
            .collect()
    }

    /// A stored preference as one of this setting's values.
    pub fn from_stored(&self, stored: Option<&Value>) -> String {
        match (self.store, stored) {
            (Store::Switch(_), Some(Value::Bool(on))) => if *on { "on" } else { "off" }.into(),
            (Store::Choice(_), Some(Value::String(s))) => {
                // A stored value this list does not name (a dimmer light mode, say) reads
                // as the nearest named one.
                if self.has(s) {
                    s.clone()
                } else {
                    self.choices.last().map_or("", |(v, _)| v).to_owned()
                }
            }
            _ => self.choices.first().map_or("", |(v, _)| v).to_owned(),
        }
    }

    /// One of this setting's values as it is stored (preferences only).
    pub fn to_stored(&self, value: &str) -> Value {
        match self.store {
            Store::Switch(_) => Value::Bool(value == "on"),
            _ => Value::String(value.to_owned()),
        }
    }
}

/// A permission's value in the words Muse uses.
pub fn access_value(consent: &Consent, field: AccessField) -> &'static str {
    let on = |b: bool| if b { "on" } else { "off" };
    match field {
        AccessField::Read => on(consent.file_read),
        AccessField::Write => on(consent.file_write),
        AccessField::OwnCommands => on(consent.drop_ins),
        AccessField::Shell => on(consent.shell),
        AccessField::Reach => match consent.files {
            FileScope::Home => "own_folder",
            FileScope::Storage => "storage",
            FileScope::Device => "everything",
        },
        AccessField::Builds => match consent.builds {
            BuildAccess::Off => "no",
            BuildAccess::Ask => "ask",
            BuildAccess::Free => "freely",
        },
    }
}

/// The change that sets a permission to `value`.
pub fn access_update(field: AccessField, value: &str) -> ConsentUpdate {
    let on = Some(value == "on");
    let mut u = ConsentUpdate::default();
    match field {
        AccessField::Read => u.file_read = on,
        AccessField::Write => u.file_write = on,
        AccessField::OwnCommands => u.drop_ins = on,
        AccessField::Shell => u.shell = on,
        AccessField::Reach => {
            u.files = Some(match value {
                "storage" => FileScope::Storage,
                "everything" => FileScope::Device,
                _ => FileScope::Home,
            });
        }
        AccessField::Builds => {
            u.builds = Some(match value {
                "ask" => BuildAccess::Ask,
                "freely" => BuildAccess::Free,
                _ => BuildAccess::Off,
            });
        }
    }
    u
}

/// A question waiting for the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The setting.
    pub setting: &'static str,
    /// What it is now.
    pub from: String,
    /// What Muse asks for.
    pub to: String,
    /// Why, in Muse's words.
    pub reason: String,
    /// The chat it was asked in.
    pub chat: String,
    /// When, Unix seconds.
    pub at: u64,
}

/// A setting changed for a while: what to put back, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timed {
    /// The profile it was changed for.
    pub profile: String,
    /// The setting.
    pub setting: String,
    /// The value given for the while. If it is something else when the time comes, the
    /// owner has changed it since and it is left alone.
    pub value: String,
    /// The value to put back.
    pub revert_to: String,
    /// When to put it back, Unix seconds.
    pub until: u64,
}

/// Checks what Muse asks for, before anything is put to the owner. `current` is the
/// setting's value now and `declined_at` when the owner last said no to it.
pub fn check(
    setting: &str,
    value: &str,
    reason: &str,
    current: impl Fn(&Setting) -> String,
    declined_at: Option<u64>,
    now: u64,
) -> Result<&'static Setting, String> {
    let s = find(setting).ok_or_else(|| {
        format!("there is no setting called {setting}; handheld.settings lists them")
    })?;
    if !s.has(value) {
        let all: Vec<&str> = s.choices.iter().map(|(v, _)| *v).collect();
        return Err(format!("{setting} can be: {}", all.join(", ")));
    }
    if s.owner_only.contains(&value) {
        return Err(format!(
            "{setting} = {value} is the owner's alone to choose, in Settings on the handheld. \
             You cannot ask for it here; if you need it, tell them why in your own words."
        ));
    }
    if current(s) == value {
        return Err(format!("{setting} is already {value}"));
    }
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > MAX_REASON {
        return Err(format!(
            "reason is required: one sentence for the owner, at most {MAX_REASON} characters"
        ));
    }
    if let Some(at) = declined_at {
        if now < at + ASK_AGAIN_AFTER_S {
            let hours = (at + ASK_AGAIN_AFTER_S - now).div_ceil(3600);
            return Err(format!(
                "the owner said no to changing {setting}. You can ask again in about {hours} \
                 hour(s); until then, work without it."
            ));
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_widest_values_cannot_be_asked_for() {
        let now = |_: &Setting| "off".to_owned();
        let why = check("shell", "on", "I need it", now, None, 0).unwrap_err();
        assert!(why.contains("owner's alone"), "{why}");
        let reach = |_: &Setting| "own_folder".to_owned();
        assert!(check("file_reach", "everything", "r", reach, None, 0).is_err());
        assert!(check(
            "file_reach",
            "storage",
            "To read your saves",
            reach,
            None,
            0
        )
        .is_ok());
        let builds = |_: &Setting| "no".to_owned();
        assert!(check("install_builds", "freely", "r", builds, None, 0).is_err());
        assert!(check(
            "install_builds",
            "ask",
            "To install the tool",
            builds,
            None,
            0
        )
        .is_ok());
        // Narrowing is always askable.
        let on = |_: &Setting| "on".to_owned();
        assert!(check("shell", "off", "I do not need it", on, None, 0).is_ok());
    }

    #[test]
    fn a_no_holds_for_a_day_and_nonsense_is_refused() {
        let off = |_: &Setting| "off".to_owned();
        let why = check("vibration", "on", "To nudge you", off, Some(1000), 2000).unwrap_err();
        assert!(why.contains("said no"), "{why}");
        assert!(check(
            "vibration",
            "on",
            "To nudge you",
            off,
            Some(1000),
            1000 + ASK_AGAIN_AFTER_S
        )
        .is_ok());
        assert!(check("vibration", "on", "", off, None, 0)
            .unwrap_err()
            .contains("reason"));
        assert!(check("vibration", "loud", "r", off, None, 0)
            .unwrap_err()
            .contains("can be"));
        assert!(check("vibration", "off", "r", off, None, 0)
            .unwrap_err()
            .contains("already"));
        assert!(check("volume", "on", "r", off, None, 0)
            .unwrap_err()
            .contains("no setting"));
    }

    #[test]
    fn values_round_trip_through_the_stores() {
        let s = find("vibration").unwrap();
        assert_eq!(s.from_stored(Some(&Value::Bool(true))), "on");
        assert_eq!(s.to_stored("off"), Value::Bool(false));
        let lights = find("lights").unwrap();
        assert_eq!(
            lights.from_stored(Some(&Value::String("presence_dim".into()))),
            "presence"
        );
        let mut c = Consent::for_new_profile();
        for (field, value) in [
            (AccessField::Reach, "storage"),
            (AccessField::Builds, "ask"),
            (AccessField::Shell, "on"),
            (AccessField::Read, "off"),
        ] {
            c.apply(&access_update(field, value));
            assert_eq!(access_value(&c, field), value);
        }
        // Only a permission being widened can be given for a while.
        assert!(find("file_reach").unwrap().widens("own_folder", "storage"));
        assert!(!find("file_reach").unwrap().widens("storage", "own_folder"));
        assert!(find("read_files").unwrap().widens("off", "on"));
        assert!(
            !find("vibration").unwrap().widens("off", "on"),
            "a preference"
        );
        // Every setting has a first value and names all of its owner-only ones.
        for s in SETTINGS {
            assert!(!s.choices.is_empty(), "{}", s.id);
            assert!(s.owner_only.iter().all(|v| s.has(v)), "{}", s.id);
        }
    }
}
