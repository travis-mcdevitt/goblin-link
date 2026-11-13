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
    assert_eq!(all["haptics.enabled"], true);
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
fn switches_take_only_true_or_false() {
    let mut s = Settings::default();
    assert!(s.set_key("haptics.enabled", &json!(false)).is_ok());
    assert!(s.set_key("haptics.enabled", &json!("yes")).is_err());
    assert!(s.set_key("haptics.enabled", &json!(1)).is_err());
    assert!(s.set_key("haptics.enabled", &Value::Null).is_err());
    assert!(matches!(
        s.set_key("nope.key", &json!(1)),
        Err(StoreError::UnknownSetting(_))
    ));
    // A rejected value leaves the previous one in place.
    assert!(!s.haptics);
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
            json!({"appearance.mode": "dark", "haptics.enabled": true}),
        ))
        .unwrap();
    assert_eq!(
        changed,
        map(json!({"appearance.mode": "dark"})),
        "vibration was already on"
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
        r#"{"appearance.mode": "dark", "leds.mode": "disco", "future.key": 1, "haptics.enabled": 300}"#,
    )
    .unwrap();
    let s = SettingsStore::open(&path).unwrap();
    assert_eq!(s.settings().appearance_mode, AppearanceMode::Dark);
    assert_eq!(
        s.settings().leds_mode,
        LedMode::Presence,
        "bad value -> default"
    );
    assert!(s.settings().haptics, "bad value -> default");
    assert!(!s.recovered());

    std::fs::write(&path, "{ not json").unwrap();
    let s = SettingsStore::open(&path).unwrap();
    assert!(s.recovered());
    assert_eq!(*s.settings(), Settings::default());
    assert!(t.path().join("settings.json.corrupt").exists());
}
