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
        .validator()
        .expect("validator")
        .validate(&root)
        .expect("validation starts");
    assert_eq!(result.config_id(), Some(REPOSITORY_CONFIG_PATH));
    let config_version = result.config_version().expect("config content version");
    assert_eq!(config_version.len(), 64);
    assert!(
        config_version
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    );
    assert_eq!(
        result.checks().iter().map(|c| c.name()).collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(result.checks()[0].passed());
    assert!(!result.checks()[1].passed());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn empty_validation_config_is_rejected_before_a_validator_can_be_created() {
    let root = repository("empty");
    init_repository(&root).expect("initialize");
    for source in [
        "schema_version = 1\n[validation]\nchecks = []\n",
        "schema_version = 1\n",
    ] {
        fs::write(root.join(REPOSITORY_CONFIG_PATH), source).expect("write empty config");
        assert!(matches!(
            load_repository_config(&root),
            Err(RepositoryConfigError::NoChecksConfigured)
        ));
    }
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

#[test]
fn absolute_and_parent_cwds_are_rejected_when_loading_config() {
    let root = repository("invalid-cwd-shape");
    init_repository(&root).expect("initialize");
    for cwd in ["/tmp", "../outside"] {
        fs::write(
            root.join(REPOSITORY_CONFIG_PATH),
            format!(
                "schema_version = 1\n[validation]\nchecks = [{{ name = 'bad', command = 'sh', cwd = '{cwd}', timeout_ms = 1000 }}]\n"
            ),
        )
        .expect("write config");
        assert!(matches!(
            load_repository_config(&root),
            Err(RepositoryConfigError::InvalidCheck {
                reason: "cwd must be a relative path without parent components",
                ..
            })
        ));
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unsupported_numeric_schema_version_precedes_unknown_field_validation() {
    let root = repository("unsupported-version");
    init_repository(&root).expect("initialize");
    fs::write(
        root.join(REPOSITORY_CONFIG_PATH),
        "schema_version = 2\nfuture_setting = true\n",
    )
    .expect("write future config");

    assert!(matches!(
        load_repository_config(&root),
        Err(RepositoryConfigError::UnsupportedVersion(2))
    ));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn malformed_or_out_of_range_schema_version_remains_a_parse_error() {
    let root = repository("invalid-version");
    init_repository(&root).expect("initialize");
    for source in [
        "validation = {}\n", // missing version
        "schema_version = '2'\n",
        "schema_version = -1\n",
        "schema_version = 4294967296\n",
    ] {
        fs::write(root.join(REPOSITORY_CONFIG_PATH), source).expect("write invalid config");
        assert!(matches!(
            load_repository_config(&root),
            Err(RepositoryConfigError::Parse(_))
        ));
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn version_one_still_rejects_unknown_fields() {
    let root = repository("version-one-unknown");
    init_repository(&root).expect("initialize");
    fs::write(
        root.join(REPOSITORY_CONFIG_PATH),
        "schema_version = 1\nfuture_setting = true\n",
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
    let error = validator
        .validate_with_cancellation(&root, token)
        .expect_err("cancelled validation must not become failed validation");
    assert_eq!(error, ai_dev_orchestrator::ValidatorError::Cancelled);
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn repository_configured_validator_passes_cancellation_to_process_runner() {
    let root = repository("configured-cancel-run");
    init_repository(&root).expect("initialize");
    fs::write(
        root.join(REPOSITORY_CONFIG_PATH),
        r#"schema_version = 1
[validation]
checks = [
  { name = "cancel", command = "sh", args = ["-c", "touch should-not-run"], timeout_ms = 1000 },
]
"#,
    )
    .expect("write config");

    let validator = load_repository_config(&root)
        .expect("load config")
        .validator()
        .expect("configured validator");
    let validator_api: &dyn Validator = &validator;
    let token = CancellationToken::new();
    token.cancel();
    let error = validator_api
        .validate_with_cancellation(&root, token)
        .expect_err("cancellation must not be recorded as validation failure");
    assert_eq!(error, ai_dev_orchestrator::ValidatorError::Cancelled);
    assert!(!root.join("should-not-run").exists());

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

#[cfg(unix)]
#[test]
fn load_rejects_config_directory_symlink_escaping_repository() {
    let root = repository("load-directory-symlink-root");
    let outside = repository("load-directory-symlink-outside");
    fs::write(
        outside.join("config.toml"),
        "schema_version = 1\n[validation]\nchecks = []\n",
    )
    .expect("write external config");
    std::os::unix::fs::symlink(&outside, root.join(".ai-dev-orchestrator"))
        .expect("create config directory symlink");

    assert!(matches!(
        load_repository_config(&root),
        Err(RepositoryConfigError::Io(error))
            if error.kind() == std::io::ErrorKind::InvalidInput
    ));

    let _ = fs::remove_file(root.join(".ai-dev-orchestrator"));
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
}

#[cfg(unix)]
#[test]
fn load_rejects_config_file_symlink_escaping_repository() {
    let root = repository("load-file-symlink-root");
    let outside = repository("load-file-symlink-outside");
    init_repository(&root).expect("initialize repository");
    let external_config = outside.join("external-config.toml");
    fs::write(
        &external_config,
        "schema_version = 1\n[validation]\nchecks = []\n",
    )
    .expect("write external config");
    let repository_config = root.join(REPOSITORY_CONFIG_PATH);
    fs::remove_file(&repository_config).expect("remove template");
    std::os::unix::fs::symlink(&external_config, &repository_config)
        .expect("create config file symlink");

    assert!(matches!(
        load_repository_config(&root),
        Err(RepositoryConfigError::Io(error))
            if error.kind() == std::io::ErrorKind::InvalidInput
    ));

    let _ = fs::remove_file(repository_config);
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
}
