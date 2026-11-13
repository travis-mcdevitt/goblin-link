use std::fs;
use std::os::unix::fs::PermissionsExt;

use goblin_store::profiles::*;
use goblin_store::{Layout, StoreError};
use serde_json::json;

// 5 + 42 chars + a canonical last character ('A').
const TOKEN: &str = "mgst_abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPA";

fn store() -> (tempfile::TempDir, ProfileStore) {
    let t = tempfile::tempdir().unwrap();
    let s = ProfileStore::open(Layout::new(t.path())).unwrap();
    (t, s)
}

fn mode(p: &std::path::Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn profile_names_are_validated() {
    for ok in ["personal", "dev", "A_b-1", &"x".repeat(32)] {
        assert!(validate_profile_name(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        "a.b",
        "a b",
        "é",
        &"x".repeat(33),
        "a\n",
    ] {
        assert!(validate_profile_name(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn create_list_active_rename_delete() {
    let (t, s) = store();
    assert_eq!(mode(&t.path().join("profiles")), 0o700);
    let p = s.create("dev").unwrap();
    assert_eq!(mode(p.dir()), 0o700);
    s.create("personal").unwrap();
    assert!(matches!(s.create("dev"), Err(StoreError::AlreadyExists(_))));
    assert!(
        matches!(s.create("DEV"), Err(StoreError::AlreadyExists(_))),
        "FAT is case-insensitive"
    );
    assert!(matches!(s.create("../x"), Err(StoreError::InvalidName(_))));
    assert_eq!(s.active(), None);
    s.set_active("dev").unwrap();
    assert_eq!(s.active().as_deref(), Some("dev"));
    assert_eq!(
        fs::read_to_string(t.path().join("active")).unwrap(),
        "dev\n"
    );
    let names: Vec<_> = s.list().unwrap().into_iter().map(|p| p.name).collect();
    assert_eq!(names, ["dev", "personal"]);

    assert!(matches!(s.delete("dev"), Err(StoreError::ActiveProfile(_))));
    s.rename("dev", "work").unwrap();
    assert_eq!(
        s.active().as_deref(),
        Some("work"),
        "active follows a rename"
    );
    assert!(matches!(
        s.rename("work", "personal"),
        Err(StoreError::AlreadyExists(_))
    ));
    s.delete("personal").unwrap();
    assert!(matches!(
        s.profile("personal"),
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        s.set_active("personal"),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn dangling_or_hostile_active_file_is_ignored() {
    let (t, s) = store();
    fs::write(t.path().join("active"), "gone\n").unwrap();
    assert_eq!(s.active(), None);
    fs::write(t.path().join("active"), "../..\n").unwrap();
    assert_eq!(s.active(), None);
}

#[test]
fn identity_matches_the_python_sdk() {
    let (_t, s) = store();
    let p = s.create("dev").unwrap();
    let id = p.identity().unwrap();
    let again = p.identity().unwrap();
    assert_eq!(id, again, "identity is stable");
    let first = u8::from_str_radix(&id.mac[..2], 16).unwrap();
    assert_eq!(first & 0x03, 0x02, "unicast, locally administered");
    assert_eq!(id.suffix().len(), 6);
    assert_eq!(id.node_id(), format!("homelink-{}", id.suffix()));
    assert_eq!(id.device_id(), format!("hatch-link:{}", id.mac));
    assert_eq!(
        id.ble_name(),
        format!("MuseGadget{}", id.suffix().to_uppercase())
    );
    let file = p.dir().join("identity.json");
    assert_eq!(mode(&file), 0o600);
    let v: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(v, json!({"mac": id.mac}));
}

#[test]
fn identity_vectors() {
    let id = Identity::from_random([0xff, 0x01, 0x02, 0xa3, 0xb4, 0xc5]);
    assert_eq!(id.mac, "fe:01:02:a3:b4:c5");
    assert_eq!(id.node_id(), "homelink-a3b4c5");
    assert_eq!(id.ble_name(), "MuseGadgetA3B4C5");
}

#[test]
fn existing_python_identity_is_kept_and_garbage_is_replaced() {
    let (_t, s) = store();
    let p = s.create("dev").unwrap();
    fs::write(
        p.dir().join("identity.json"),
        r#"{"mac": "02:11:22:33:44:55"}"#,
    )
    .unwrap();
    assert_eq!(p.identity().unwrap().mac, "02:11:22:33:44:55");
    fs::write(p.dir().join("identity.json"), r#"{"mac": "NOT-A-MAC"}"#).unwrap();
    let fresh = p.identity().unwrap();
    assert_ne!(fresh.mac, "NOT-A-MAC");
    assert!(p.dir().join("identity.json.corrupt").exists());
}

#[test]
fn pairing_round_trip_preserves_python_fields_and_extras() {
    let (_t, s) = store();
    let p = s.create("dev").unwrap();
    assert!(p.pairing().unwrap().is_none());
    let record = json!({
        "access_token": "AT", "refresh_token": "RT", "token_type": "device",
        "username": "u", "api_url": "https://a", "api_url_v2": "https://b",
        "noise_host": "h", "access_token_saved_at": 1700000000,
        "sdk_token_hint": "mgst_abcdefg", "future_field": {"x": 1}
    });
    fs::write(p.dir().join("pairing.json"), record.to_string()).unwrap();
    let pairing = p.pairing().unwrap().unwrap();
    assert_eq!(pairing.access_token.expose(), "AT");
    assert_eq!(pairing.access_token_saved_at, 1_700_000_000);
    assert_eq!(pairing.extra["future_field"], json!({"x": 1}));
    p.save_pairing(&pairing).unwrap();
    let back: serde_json::Value =
        serde_json::from_slice(&fs::read(p.dir().join("pairing.json")).unwrap()).unwrap();
    assert_eq!(back, record);
    assert_eq!(mode(&p.dir().join("pairing.json")), 0o600);
    assert!(p.summary().paired);

    let rotated = p
        .rotate_tokens(Secret::new("AT2"), Secret::new("RT2"), 1_700_000_500)
        .unwrap();
    assert_eq!(rotated.username.as_deref(), Some("u"));
    let again = p.pairing().unwrap().unwrap();
    assert_eq!(again.refresh_token.expose(), "RT2");
    assert_eq!(again.access_token_saved_at, 1_700_000_500);
    p.delete_pairing().unwrap();
    p.delete_pairing().unwrap();
    assert!(p.pairing().unwrap().is_none());
}

#[test]
fn pairing_validation_mirrors_config_pairing_problem() {
    let bad = [
        json!({"refresh_token": "x"}),
        json!({"access_token": "x"}),
        json!({"access_token": "  ", "refresh_token": "x"}),
        json!({"access_token": 5, "refresh_token": "x"}),
        json!({"access_token": "a", "refresh_token": "x", "access_token_saved_at": "yesterday"}),
        json!({"access_token": "a", "refresh_token": "x", "access_token_saved_at": -1}),
        json!([1]),
    ];
    for b in bad {
        assert!(
            matches!(Pairing::from_value(&b), Err(StoreError::InvalidPairing(_))),
            "{b}"
        );
    }
    // Older records omit the timestamp; floats are accepted.
    let p = Pairing::from_value(&json!({"access_token": "a", "refresh_token": "b"})).unwrap();
    assert_eq!(p.access_token_saved_at, 0);
    let p = Pairing::from_value(
        &json!({"access_token": "a", "refresh_token": "b", "access_token_saved_at": 12.7}),
    )
    .unwrap();
    assert_eq!(p.access_token_saved_at, 12);
}

#[test]
fn invalid_pairing_file_is_reported_not_treated_as_unpaired() {
    let (_t, s) = store();
    let p = s.create("dev").unwrap();
    fs::write(p.dir().join("pairing.json"), "{broken").unwrap();
    assert!(matches!(p.pairing(), Err(StoreError::Corrupt { .. })));
    assert!(!p.summary().paired);
}

#[test]
fn secrets_never_appear_in_debug_or_display() {
    let secret = Secret::new("super-secret-value");
    assert!(!format!("{secret:?}").contains("super-secret"));
    assert!(!format!("{secret}").contains("super-secret"));
    let pairing = Pairing::new(Secret::new("AAA-access"), Secret::new("RRR-refresh"), 1);
    let dbg = format!("{pairing:?}");
    assert!(
        !dbg.contains("AAA-access") && !dbg.contains("RRR-refresh"),
        "{dbg}"
    );
    let tok = SdkToken::parse(TOKEN).unwrap();
    assert!(!format!("{tok:?}").contains("mgst_"));
    let err = SdkToken::parse("mgst_hunter2").unwrap_err().to_string();
    assert!(!err.contains("hunter2"));
}

#[test]
fn sdk_token_validation_and_storage() {
    assert!(SdkToken::parse(TOKEN).is_ok());
    assert_eq!(
        SdkToken::parse(&format!("  {TOKEN}\n")).unwrap().hint(),
        "mgst_abcdefg"
    );
    let wrong_last = format!("{}B", &TOKEN[..TOKEN.len() - 1]);
    for bad in [
        "",
        "mgst_",
        "xgst_abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPA",
        &wrong_last,
        &TOKEN[..40],
    ] {
        assert!(
            matches!(SdkToken::parse(bad), Err(StoreError::InvalidSdkToken)),
            "{bad}"
        );
    }

    let (t, s) = store();
    let p = s.create("dev").unwrap();
    assert!(p.sdk_token().unwrap().is_none());
    let tok = SdkToken::parse(TOKEN).unwrap();
    p.save_sdk_token(&tok).unwrap();
    assert_eq!(mode(&p.dir().join("sdk_token")), 0o600);
    assert_eq!(p.sdk_token().unwrap().unwrap(), tok);
    assert!(p.summary().has_sdk_token);
    p.delete_sdk_token().unwrap();
    assert!(!p.summary().has_sdk_token);

    // Importing consumes the source file, but only when it is valid.
    let src = t.path().join("drop.txt");
    fs::write(&src, format!("{TOKEN}\n")).unwrap();
    p.import_sdk_token_file(&src).unwrap();
    assert!(!src.exists());
    assert_eq!(p.sdk_token().unwrap().unwrap(), tok);
    fs::write(&src, "garbage").unwrap();
    assert!(p.import_sdk_token_file(&src).is_err());
    assert!(src.exists());
    // A hand-edited invalid token file is an error, a blank one means "none".
    fs::write(p.dir().join("sdk_token"), "nope").unwrap();
    assert!(matches!(p.sdk_token(), Err(StoreError::InvalidSdkToken)));
    fs::write(p.dir().join("sdk_token"), "  \n").unwrap();
    assert!(p.sdk_token().unwrap().is_none());
}
