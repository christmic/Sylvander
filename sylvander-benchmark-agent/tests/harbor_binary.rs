use std::process::Command;

#[test]
fn self_check_proves_the_runner_can_start() {
    let output = Command::new(env!("CARGO_BIN_EXE_sylvander-harbor-agent"))
        .arg("--self-check")
        .output()
        .expect("self-check process must start");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"sylvander-harbor-agent ready\n");
    assert!(output.stderr.is_empty());
}
