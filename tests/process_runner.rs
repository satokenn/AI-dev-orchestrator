use ai_dev_orchestrator::{ProcessError, ProcessRequest, ProcessRunner};
use std::time::Duration;

#[cfg(unix)]
#[test]
fn uses_cwd_and_environment_without_shell_interpolation() {
    let output = ProcessRunner
        .run(
            ProcessRequest::new("sh")
                .args(["-c", "printf '%s:%s' \"$PWD\" \"$PROCESS_RUNNER_TEST\""])
                .cwd("/tmp")
                .env("PROCESS_RUNNER_TEST", "value with spaces"),
        )
        .expect("process succeeds");

    let text = String::from_utf8(output.stdout).expect("utf8 output");
    assert!(text.starts_with("/tmp:"), "unexpected cwd: {text}");
    assert!(text.ends_with("value with spaces"), "unexpected environment: {text}");
}

#[cfg(unix)]
#[test]
fn timeout_preserves_captured_output() {
    let result = ProcessRunner.run(
        ProcessRequest::new("sh")
            .args(["-c", "printf before; sleep 10"])
            .timeout(Duration::from_millis(20)),
    );

    match result {
        Err(ProcessError::TimedOut(output)) => assert_eq!(output.stdout, b"before"),
        other => panic!("unexpected result: {other:?}"),
    }
}
