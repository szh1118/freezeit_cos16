use freezeit_daemon::app::command_runner::run_command;

#[test]
fn drains_large_stdout_and_stderr_without_deadlock() {
    let script = "i=0; while [ $i -lt 2048 ]; do printf '%04096d' 0; printf '%04096d' 0 >&2; i=$((i+1)); done";
    let output = run_command("sh", &["-c", script]).expect("command completes");

    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 2048 * 4096);
    assert_eq!(output.stderr.len(), 2048 * 4096);
}
