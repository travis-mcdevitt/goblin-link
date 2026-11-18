use std::process::Command;

#[test]
fn held_out_synthetic_audio() {
    let mut python = Command::new("python3");
    if let Some(case) = option_env!("GOBLIN_WAKE_CASE") {
        let case = std::path::Path::new(case);
        python.env("WAKE_RECIPE", case.join("phrases.json"));
        python.env("WAKE_MODELS", case.join("models"));
    }
    let status = python
        .arg("evaluate.py")
        .status()
        .expect("run in the Hey Goblin experiment image through tools/test-in-container.sh");
    assert!(status.success(), "synthetic wake-word evaluation failed");
}
