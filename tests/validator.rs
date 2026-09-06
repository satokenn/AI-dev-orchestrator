use ai_dev_orchestrator::{
    Attempt, AttemptId, AttemptState, CommandValidator, ProviderRef, RustValidator,
    ValidationCheck, ValidationResult, Validator, ValidatorError, default_rust_checks,
};
use std::path::{Path, PathBuf};

fn test_workspace(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "ai-dev-orchestrator-validator-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create test workspace");
    path
}

#[cfg(unix)]
fn shell() -> &'static str {
    "sh"
}

#[cfg(windows)]
fn shell() -> &'static str {
    "cmd"
}

#[cfg(unix)]
fn shell_args(script: &str) -> [String; 2] {
    ["-c".to_owned(), script.to_owned()]
}

#[cfg(windows)]
fn shell_args(script: &str) -> [String; 2] {
    ["/C".to_owned(), script.to_owned()]
}

#[test]
fn checks_run_in_order_in_the_requested_workspace_and_map_output() {
    let workspace = std::env::temp_dir();
    let checks = [
        ValidationCheck::new("first", shell()).args(shell_args("printf first; exit 0")),
        ValidationCheck::new("second", shell()).args(shell_args("printf diagnostic >&2; exit 9")),
    ];

    let results = CommandValidator::new(checks)
        .validate(&workspace)
        .expect("workspace is valid");

    assert!(!results.passed());
    assert_eq!(results.checks().len(), 2);
    assert_eq!(results.checks()[0].name(), "first");
    assert!(results.checks()[0].passed());
    assert_eq!(results.checks()[0].exit_status(), Some(0));
    assert_eq!(results.checks()[0].diagnostics(), "first");
    assert_eq!(results.checks()[1].name(), "second");
    assert!(!results.checks()[1].passed());
    assert_eq!(results.checks()[1].exit_status(), Some(9));
    assert_eq!(results.checks()[1].diagnostics(), "diagnostic");
}

#[test]
fn a_check_uses_workspace_as_its_default_cwd() {
    let workspace = std::env::temp_dir();
    let check = ValidationCheck::new("cwd", shell()).args(shell_args(if cfg!(unix) {
        "pwd"
    } else {
        "cd"
    }));
    let result = CommandValidator::new([check])
        .validate(&workspace)
        .expect("workspace is valid")
        .checks()[0]
        .clone();

    assert!(result.passed());
    if cfg!(unix) {
        let cwd = std::fs::canonicalize(&workspace).expect("canonical workspace");
        assert_eq!(result.diagnostics().trim(), cwd.to_string_lossy());
    }
}

#[test]
fn invalid_workspace_is_rejected_before_any_command_runs() {
    let workspace = Path::new("/definitely/missing/validator-workspace");
    let error = CommandValidator::new([ValidationCheck::new("check", shell())])
        .validate(workspace)
        .expect_err("missing workspace must be rejected");

    assert!(matches!(error, ValidatorError::InvalidWorkspace { .. }));
}

#[test]
fn cwd_inside_workspace_is_allowed() {
    let workspace = test_workspace("inside");
    let subdir = workspace.join("nested");
    std::fs::create_dir(&subdir).expect("create nested directory");
    let result = CommandValidator::new([ValidationCheck::new("check", shell()).cwd(&subdir)])
        .validate(&workspace)
        .expect("nested cwd is inside workspace");
    assert!(result.passed());
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn missing_cwd_is_rejected_before_any_command_runs() {
    let workspace = test_workspace("missing-cwd");
    let error = CommandValidator::new([
        ValidationCheck::new("check", shell()).cwd(workspace.join("does-not-exist"))
    ])
    .validate(&workspace)
    .expect_err("missing cwd must be rejected");
    assert!(matches!(
        error,
        ValidatorError::WorkspaceOutsideBoundary { .. }
    ));
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn cwd_with_parent_traversal_is_rejected() {
    let workspace = test_workspace("parent-cwd");
    let error = CommandValidator::new([ValidationCheck::new("check", shell()).cwd("nested/..")])
        .validate(&workspace)
        .expect_err("parent traversal must be rejected");
    assert!(matches!(
        error,
        ValidatorError::WorkspaceOutsideBoundary { .. }
    ));
    let _ = std::fs::remove_dir_all(workspace);
}

#[cfg(unix)]
#[test]
fn cwd_symlink_resolving_outside_workspace_is_rejected() {
    let workspace = test_workspace("symlink-cwd");
    let outside = test_workspace("outside-cwd");
    std::os::unix::fs::symlink(&outside, workspace.join("escape")).expect("create symlink");
    let error = CommandValidator::new([ValidationCheck::new("check", shell()).cwd("escape")])
        .validate(&workspace)
        .expect_err("outside symlink must be rejected");
    assert!(matches!(
        error,
        ValidatorError::WorkspaceOutsideBoundary { .. }
    ));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn no_checks_are_rejected_instead_of_passing() {
    let error = CommandValidator::new([])
        .validate(&std::env::temp_dir())
        .expect_err("an empty check list must be rejected");

    assert_eq!(error, ValidatorError::NoChecksConfigured);
    assert_eq!(error.to_string(), "no validation checks configured");
    assert!(!ValidationResult::from_checks("empty", []).passed());
}

#[test]
fn rust_validator_contains_the_standard_checks_without_running_them() {
    let validator = RustValidator::new();
    assert_eq!(validator.checks(), default_rust_checks());
}

#[test]
fn aggregate_result_is_applied_to_an_attempt_once() {
    let checks = [
        ValidationCheck::new("pass", shell()).args(shell_args("exit 0")),
        ValidationCheck::new("fail", shell()).args(shell_args("exit 3")),
    ];
    let result = CommandValidator::new(checks)
        .validate(&std::env::temp_dir())
        .expect("workspace is valid");
    let mut attempt = Attempt::new(AttemptId::new("attempt"), ProviderRef::new("fake"));
    attempt.start().expect("start attempt");
    attempt.finish().expect("finish attempt");
    attempt
        .apply_validation(result)
        .expect("apply aggregate validation");

    assert_eq!(attempt.state(), AttemptState::Failed);
    assert_eq!(attempt.validation_results().len(), 1);
    assert_eq!(attempt.validation_results()[0].checks().len(), 2);
}
