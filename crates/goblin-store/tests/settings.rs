use std::sync::{Arc, Mutex};

use goblin_store::settings::*;
use goblin_store::StoreError;
use serde_json::{json, Map, Value};

fn map(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[test]
fn defaults_cover_every_documented_key() {
    let t = tempfile::tempdir().unwrap();
    let s = SettingsStore::open(&t.path().join("settings.json")).unwrap();
    let all = s.get(None).unwrap();
    let mut keys: Vec<&str> = all.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected = Settings::KEYS.to_vec();
    expected.sort_unstable();
    assert_eq!(keys, expected);
    assert_eq!(all["appearance.mode"], "system");
    assert_eq!(all["voice.speak_replies"], "never");
    assert_eq!(all["leds.mode"], "presence");
    assert_eq!(all["chat.quick_replies"][0], "Yes");
    assert!(!s.recovered());
}

#[test]
fn every_allowed_enum_value_is_accepted_and_others_rejected() {
    let cases: &[(&str, &[&str])] = &[
        ("appearance.mode", &["system", "light", "dark"]),
        (
            "appearance.chat_theme",
            &[
                "avatar",
                "default",
                "monochrome",
                "beige",
                "blue",
                "purple",
                "pink",
                "orange",
                "green",
            ],
        ),
        (
            "appearance.avatar_size",
            &["small", "medium", "large", "xl", "hidden"],
        ),
        ("voice.speak_replies", &["always", "voice_only", "never"]),
        ("leds.mode", &["off", "presence", "presence_dim"]),
    ];
    for (key, values) in cases {
        for v in *values {
            let mut s = Settings::default();
            s.set_key(key, &json!(v))
                .unwrap_or_else(|e| panic!("{key}={v}: {e}"));
            assert_eq!(s.to_map()[*key], json!(v));
        }
        let mut s = Settings::default();
        assert!(matches!(
            s.set_key(key, &json!("bogus")),
            Err(StoreError::InvalidSetting { .. })
        ));
        assert!(s.set_key(key, &json!(3)).is_err());
    }
}

#[test]
fn bools_numbers_and_lists_are_validated() {
    let mut s = Settings::default();
    assert!(s.set_key("audio.earcons", &json!(false)).is_ok());
    assert!(s.set_key("audio.earcons", &json!("yes")).is_err());
    assert!(s.set_key("audio.earcons", &Value::Null).is_err());
    assert!(s.set_key("audio.notification_volume", &json!(100)).is_ok());
    assert!(s.set_key("audio.notification_volume", &json!(101)).is_err());
    assert!(s.set_key("audio.notification_volume", &json!(-1)).is_err());
    assert!(s
        .set_key("audio.notification_volume", &json!(20.5))
        .is_err());
    assert!(s.set_key("voice.mic_threshold", &json!(0)).is_ok());
    assert!(s.set_key("voice.mic_threshold", &json!(0.5)).is_ok());
    assert!(s.set_key("voice.mic_threshold", &json!(1.5)).is_err());
    assert!(s.set_key("voice.mic_threshold", &json!(-0.1)).is_err());
    assert!(s
        .set_key("chat.quick_replies", &json!(["Ok", "Later"]))
        .is_ok());
    assert!(s.set_key("chat.quick_replies", &json!([])).is_ok());
    assert!(s.set_key("chat.quick_replies", &json!([""])).is_err());
    assert!(s.set_key("chat.quick_replies", &json!(["a\nb"])).is_err());
    assert!(s
        .set_key("chat.quick_replies", &json!(["x".repeat(41)]))
        .is_err());
    assert!(s
        .set_key("chat.quick_replies", &json!(vec!["x"; 13]))
        .is_err());
    assert!(s.set_key("chat.quick_replies", &json!("Ok")).is_err());
    assert!(matches!(
        s.set_key("nope.key", &json!(1)),
        Err(StoreError::UnknownSetting(_))
    ));
    // A rejected value leaves the previous one in place.
    assert_eq!(s.notification_volume, 100);
    assert_eq!(s.quick_replies, Vec::<String>::new());
}

#[test]
fn set_is_atomic_persistent_and_notifies_only_real_changes() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("settings.json");
    let mut s = SettingsStore::open(&path).unwrap();
    let seen = Arc::new(Mutex::new(Vec::<Map<String, Value>>::new()));
    let sink = seen.clone();
    s.on_change(move |c| sink.lock().unwrap().push(c.clone()));

    let changed = s
        .set(&map(
            json!({"appearance.mode": "dark", "audio.earcons": true}),
        ))
        .unwrap();
    assert_eq!(
        changed,
        map(json!({"appearance.mode": "dark"})),
        "earcons was already true"
    );
    assert_eq!(seen.lock().unwrap().len(), 1);

    // Same values again: no write, no notification.
    assert!(s
        .set(&map(json!({"appearance.mode": "dark"})))
        .unwrap()
        .is_empty());
    assert_eq!(seen.lock().unwrap().len(), 1);

    // One bad key rejects the whole request.
    let err = s.set(&map(
        json!({"appearance.mode": "light", "leds.mode": "disco"}),
    ));
    assert!(err.is_err());
    assert_eq!(s.settings().appearance_mode, AppearanceMode::Dark);
    assert_eq!(seen.lock().unwrap().len(), 1);

    drop(s);
    let reopened = SettingsStore::open(&path).unwrap();
    assert_eq!(reopened.settings().appearance_mode, AppearanceMode::Dark);
    let got = reopened.get(Some(&["appearance.mode".to_owned()])).unwrap();
    assert_eq!(got, map(json!({"appearance.mode": "dark"})));
    assert!(reopened.get(Some(&["zzz".to_owned()])).is_err());
}

#[test]
fn loading_tolerates_unknown_keys_bad_values_and_corruption() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"appearance.mode": "dark", "leds.mode": "disco", "future.key": 1, "audio.notification_volume": 300}"#,
    )
    .unwrap();
    let s = SettingsStore::open(&path).unwrap();
    assert_eq!(s.settings().appearance_mode, AppearanceMode::Dark);
    assert_eq!(
        s.settings().leds_mode,
        LedMode::Presence,
        "bad value -> default"
    );
    assert_eq!(s.settings().notification_volume, 50);
    assert!(!s.recovered());

    std::fs::write(&path, "{ not json").unwrap();
    let s = SettingsStore::open(&path).unwrap();
    assert!(s.recovered());
    assert_eq!(*s.settings(), Settings::default());
    assert!(t.path().join("settings.json.corrupt").exists());
}
