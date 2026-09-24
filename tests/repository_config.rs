use ai_dev_orchestrator::{
    CancellationToken, REPOSITORY_CONFIG_PATH, RepositoryConfigError, ValidationCheck, Validator,
    init_repository, load_repository_config,
};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn repository(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "ai-dev-orchestrator-config-{name}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create repository");
    path
}

#[test]
fn init_creates_canonical_template_without_replacing_existing_config() {
    let root = repository("init");
    let path = init_repository(&root).expect("initialize repository config");
    assert_eq!(
        path,
        fs::canonicalize(&root)
            .expect("canonical repository root")
            .join(REPOSITORY_CONFIG_PATH)
    );
    let template = fs::read_to_string(&path).expect("read config template");
    assert!(template.contains("schema_version = 1"));
    assert!(template.contains("checks = []"));
    assert!(matches!(
        init_repository(&root),
        Err(RepositoryConfigError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists
    ));
    assert_eq!(
        fs::read_to_string(&path).expect("existing config remains"),
        template
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn config_parser_preserves_structured_args_and_declared_order() {
    let root = repository("ordered");
    init_repository(&root).expect("initialize");
    fs::write(
        root.join(REPOSITORY_CONFIG_PATH),
        r#"schema_version = 1
[validation]
checks = [
  { name = "first", command = "sh", args = ["-c", "exit 0"], cwd = ".", timeout_ms = 1000 },
  { name = "second", command = "sh", args = ["-c", "exit 3"], timeout_ms = 2000 },
]
"#,
    )
    .expect("write config");

    let config = load_repository_config(&root).expect("parse config");
    let checks = config.validation.as_ref().expect("validation config");
    assert_eq!(checks.checks[0].name, "first");
    assert_eq!(checks.checks[0].args, ["-c", "exit 0"]);
    assert_eq!(checks.checks[0].timeout_ms, 1000);
    let result = config
        .command_validator()
        .expect("validator")
        .validate(&root)
        .expect("validation starts");
    assert_eq!(
        result.checks().iter().map(|c| c.name()).collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(result.checks()[0].passed());
    assert!(!result.checks()[1].passed());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn empty_validation_config_fails_closed_and_missing_checks_are_not_a_pass() {
    let root = repository("empty");
    init_repository(&root).expect("initialize");
    let config = load_repository_config(&root).expect("empty template parses");
    let error = config
        .command_validator()
        .expect("empty validator can be represented")
        .validate(&root)
        .expect_err("no configured checks must fail closed");
    assert_eq!(error.to_string(), "no validation checks configured");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn invalid_timeout_is_rejected_before_validator_can_run() {
    let root = repository("timeout");
    init_repository(&root).expect("initialize");
    fs::write(
        root.join(REPOSITORY_CONFIG_PATH),
        "schema_version = 1\n[validation]\nchecks = [{ name = 'bad', command = 'sh', timeout_ms = 0 }]\n",
    )
    .expect("write config");
    assert!(matches!(
        load_repository_config(&root),
        Err(RepositoryConfigError::InvalidCheck {
            reason: "timeout_ms must be greater than zero",
            ..
        })
    ));
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn configured_timeout_is_passed_to_process_runner() {
    let root = repository("timeout-run");
    let validator =
        ai_dev_orchestrator::CommandValidator::new([ValidationCheck::new("sleep", "sh")
            .args(["-c", "sleep 2"])
            .timeout(Duration::from_millis(20))]);
    let result = validator.validate(&root).expect("validator starts");
    assert!(!result.passed());
    assert!(result.checks()[0].diagnostics().contains("timed out"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cancellation_token_is_passed_to_process_runner() {
    let root = repository("cancel-run");
    let token = CancellationToken::new();
    token.cancel();
    let validator =
        ai_dev_orchestrator::CommandValidator::new([
            ValidationCheck::new("cancel", "sh").args(["-c", "exit 0"])
        ]);
    let result = validator
        .validate_with_cancellation(&root, token)
        .expect("validator records cancellation as a failed check");
    assert!(!result.passed());
    assert!(
        result.checks()[0]
            .diagnostics()
            .contains("cancelled before start")
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn secret_fields_are_not_part_of_the_schema() {
    let root = repository("secret-field");
    init_repository(&root).expect("initialize");
    fs::write(
        root.join(REPOSITORY_CONFIG_PATH),
        "schema_version = 1\napi_token = 'must-not-be-configurable'\n",
    )
    .expect("write config");
    assert!(matches!(
        load_repository_config(&root),
        Err(RepositoryConfigError::Parse(_))
    ));
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn init_rejects_config_directory_symlink_escaping_repository() {
    let root = repository("symlink-root");
    let outside = repository("symlink-outside");
    std::os::unix::fs::symlink(&outside, root.join(".ai-dev-orchestrator"))
        .expect("create config directory symlink");

    assert!(matches!(
        init_repository(&root),
        Err(RepositoryConfigError::Io(error))
            if error.kind() == std::io::ErrorKind::InvalidInput
    ));
    assert!(!outside.join("config.toml").exists());

    let _ = fs::remove_file(root.join(".ai-dev-orchestrator"));
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
}
