//! Runs the built `goblinctl` against the shipped platforms on the development host.

use std::path::Path;
use std::process::{Command, Output};

fn goblinctl(args: &[&str]) -> Output {
    let platforms = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../platforms");
    Command::new(env!("CARGO_BIN_EXE_goblinctl"))
        .args(["platform", "--platforms-dir"])
        .arg(platforms)
        .args(["--platform", "generic-sdl"])
        .args(args)
        .output()
        .expect("goblinctl runs")
}

#[test]
fn info_lists_capabilities_as_unavailable() {
    let out = goblinctl(&["info"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("platform: generic-sdl"), "{text}");
    assert!(
        text.contains("rumble                 unavailable"),
        "{text}"
    );
}

#[test]
fn unavailable_capability_exits_3() {
    for args in [
        &["rumble", "10"][..],
        &["battery"],
        &["leds", "list"],
        &["volume", "get"],
    ] {
        let out = goblinctl(args);
        assert_eq!(out.status.code(), Some(3), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("unavailable"));
    }
}

#[test]
fn health_prints_json_with_version() {
    let out = goblinctl(&["health"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("\"version\""), "{text}");
}

#[test]
fn bare_command_prints_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_goblinctl"))
        .output()
        .expect("runs");
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("goblinctl "));
}
