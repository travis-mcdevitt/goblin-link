use std::process::Command;

fn run_args(script: &str, args: &[&str]) {
    let status = Command::new("python3")
        .arg(script)
        .args(args)
        .env("PYTHONPATH", "..:/opt/sdk/linux/src")
        .status()
        .expect("run through tools/test-in-container.sh using the IPA experiment image");
    assert!(status.success(), "{script} failed");
}

fn run(script: &str) {
    run_args(script, &[]);
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

#[test]
#[ignore = "ten new phrase heads and eight additional synthetic voices"]
fn transfer_inventory() {
    run("transfer.py");
}

#[test]
#[ignore = "continuous mixed speech with the portable encoder"]
fn mixed_events() {
    run("stream_eval.py");
}

macro_rules! longform {
    ($name:ident, $seed:literal) => {
        #[test]
        #[ignore = "five minutes of continuous synthetic negatives"]
        fn $name() {
            run_args("stream_eval.py", &[$seed]);
        }
    };
}

longform!(longform_0, "0");
longform!(longform_1, "1");
longform!(longform_2, "2");
longform!(longform_3, "3");
longform!(longform_4, "4");
longform!(longform_5, "5");

#[test]
fn specialized_checkpoint() {
    run("verify_bank.py");
}

#[test]
#[ignore = "fresh synthetic speakers for the first six inventory phrases"]
fn confirm_inventory_a() {
    run_args("transfer.py", &["confirm-a"]);
}

#[test]
#[ignore = "fresh synthetic speakers for the other six inventory phrases"]
fn confirm_inventory_b() {
    run_args("transfer.py", &["confirm-b"]);
}

#[test]
#[ignore = "IPA-only synthetic pseudowords"]
fn nonce_phrases() {
    run("nonce.py");
}

#[test]
#[ignore = "explicit IPA competitors on development audio"]
fn development_confusers() {
    run("guard_dev.py");
}

#[test]
#[ignore = "additional independent voices for the optimized two-phrase recipe"]
fn focused_a() {
    run_args("focused_eval.py", &["0"]);
}

#[test]
#[ignore = "second half of the optimized two-phrase confirmation"]
fn focused_b() {
    run_args("focused_eval.py", &["1"]);
}
