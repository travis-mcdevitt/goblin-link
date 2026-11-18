use std::process::Command;

fn run(script: &str) {
    let status = Command::new("python3")
        .arg(script)
        .env("PYTHONPATH", "..:/opt/sdk/linux/src")
        .status()
        .expect("run through tools/test-in-container.sh using the IPA experiment image");
    assert!(status.success(), "{script} failed");
}

#[test]
#[ignore = "development sweep"]
fn tune_development() {
    run("tune.py");
}

#[test]
fn compiled_weights() {
    run("verify.py");
}

#[test]
fn held_out_audio() {
    run("evaluate.py");
}

#[test]
#[ignore = "CPU benchmark on development audio"]
fn cpu_benchmark() {
    run("benchmark.py");
}
