use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use ai_dev_orchestrator::cli::{
    self, CliRuntime, FakeRuntime, OPERATION_ERROR, SUCCESS, UNAVAILABLE, USAGE_ERROR,
};
use ai_dev_orchestrator::{PublicationRecord, SqliteExecutionLedger, TaskId};

fn binary() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_ai-dev-orchestrator"))
}

#[test]
fn installed_binary_supports_help_and_version() {
    let help = binary().arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("USAGE:"));

    let version = binary().arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(
        String::from_utf8_lossy(&version.stdout)
            .contains(concat!("ai-dev-orchestrator ", env!("CARGO_PKG_VERSION")))
    );
}

#[test]
fn installed_binary_rejects_unknown_command() {
    let output = binary().arg("unknown-command").output().unwrap();
    assert_eq!(output.status.code(), Some(USAGE_ERROR));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown command"));
}

#[derive(Clone, Default)]
struct RecordingRuntime(Arc<Mutex<Vec<&'static str>>>);
impl CliRuntime for RecordingRuntime {
    fn doctor(&self, _: bool) -> i32 {
        self.0.lock().unwrap().push("doctor");
        SUCCESS
    }
    fn run_issue(&self, _: &cli::RunRequest) -> i32 {
        self.0.lock().unwrap().push("run");
        OPERATION_ERROR
    }
    fn status(&self, _: &str, _: &Path, _: bool) -> i32 {
        self.0.lock().unwrap().push("status");
        SUCCESS
    }
}

struct UnavailableRuntime;
impl CliRuntime for UnavailableRuntime {
    fn doctor(&self, _: bool) -> i32 {
        UNAVAILABLE
    }
    fn run_issue(&self, _: &cli::RunRequest) -> i32 {
        OPERATION_ERROR
    }
    fn status(&self, _: &str, _: &Path, _: bool) -> i32 {
        OPERATION_ERROR
    }
}

#[test]
fn parser_rejects_unknown_and_incomplete_options() {
    let runtime = RecordingRuntime::default();
    assert_eq!(cli::run_with_runtime(["wat".into()], &runtime), USAGE_ERROR);
    assert_eq!(
        cli::run_with_runtime(["doctor".into(), "--wat".into()], &runtime),
        USAGE_ERROR
    );
    assert_eq!(
        cli::run_with_runtime(["doctor".into(), "--json".into(), "extra".into()], &runtime),
        USAGE_ERROR
    );
    assert_eq!(
        cli::run_with_runtime(["status".into(), "--ledger".into()], &runtime),
        USAGE_ERROR
    );
}

#[test]
fn help_version_and_unavailable_exit_codes_are_stable() {
    let runtime = RecordingRuntime::default();
    assert_eq!(cli::run_with_runtime(["--help".into()], &runtime), SUCCESS);
    assert_eq!(
        cli::run_with_runtime(["--version".into()], &runtime),
        SUCCESS
    );
    assert_eq!(
        cli::run_with_runtime(["doctor".into()], &UnavailableRuntime),
        UNAVAILABLE
    );
}

#[test]
fn dispatches_commands_through_injected_runtime_in_order() {
    let runtime = RecordingRuntime::default();
    assert_eq!(cli::run_with_runtime(["doctor".into()], &runtime), SUCCESS);
    assert_eq!(
        cli::run_with_runtime(
            [
                "status".into(),
                "--task-id".into(),
                "x".into(),
                "--ledger".into(),
                "/tmp/x".into()
            ],
            &runtime
        ),
        SUCCESS
    );
    assert_eq!(
        cli::run_with_runtime(
            [
                "run".into(),
                "--repo".into(),
                "o/r".into(),
                "--issue".into(),
                "1".into(),
                "--repository-root".into(),
                "/tmp".into()
            ],
            &runtime
        ),
        OPERATION_ERROR
    );
    assert_eq!(&*runtime.0.lock().unwrap(), &["doctor", "status", "run"]);
}

#[test]
fn fake_runtime_run_and_status_are_local_only() {
    let root = std::env::temp_dir().join(format!(
        "ai-dev-orchestrator-cli-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = root.join("nested/ledger.sqlite3");
    let args = [
        "run",
        "--repo",
        "o/r",
        "--issue",
        "1",
        "--repository-root",
        "/tmp",
        "--ledger",
        path.to_str().unwrap(),
    ];
    assert_eq!(
        cli::run_with_runtime(args.into_iter().map(String::from), &FakeRuntime),
        SUCCESS
    );
    assert_eq!(
        cli::run_with_runtime(
            [
                "status",
                "--task-id",
                "issue-1",
                "--ledger",
                path.to_str().unwrap(),
                "--json"
            ]
            .into_iter()
            .map(String::from),
            &FakeRuntime
        ),
        SUCCESS
    );
    assert_eq!(
        cli::run_with_runtime(
            [
                "status",
                "--task-id",
                "missing",
                "--ledger",
                path.to_str().unwrap()
            ]
            .into_iter()
            .map(String::from),
            &FakeRuntime
        ),
        OPERATION_ERROR
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn status_does_not_create_a_missing_ledger_parent() {
    let root = std::env::temp_dir().join(format!(
        "ai-dev-orchestrator-cli-status-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = root.join("nested/ledger.sqlite3");
    assert_eq!(
        cli::run_with_runtime(
            [
                "status".into(),
                "--task-id".into(),
                "missing".into(),
                "--ledger".into(),
                path.to_str().unwrap().into(),
            ],
            &FakeRuntime,
        ),
        OPERATION_ERROR
    );
    assert!(!root.exists());
}

#[cfg(unix)]
#[test]
fn invalid_repository_config_stops_production_before_planner_provider_or_workspace() {
    use std::os::unix::fs::PermissionsExt;

    let root = std::env::temp_dir().join(format!(
        "ai-dev-orchestrator-cli-config-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let repository_config = root.join(".ai-dev-orchestrator/config.toml");
    fs::create_dir_all(repository_config.parent().unwrap()).unwrap();
    fs::write(
        &repository_config,
        "schema_version = 1\n[validation]\nchecks = [{ name = 'unsafe-cwd', command = 'sh', cwd = '../outside', timeout_ms = 1000 }]\n",
    )
    .unwrap();

    let bin = root.join("fake-bin");
    fs::create_dir(&bin).unwrap();
    let process_marker = root.join("unexpected-process");
    let marker = process_marker.to_string_lossy();
    for command in ["codex", "copilot", "agy", "git"] {
        let path = bin.join(command);
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' '{command} $*' >> '{marker}'\nexit 0\n"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let gh = bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\nprintf '%s\\n' '{\"title\":\"fixture\",\"body\":\"fixture body\"}'\n",
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();

    let ledger = root.join("ledger.sqlite3");
    let output = binary()
        .args([
            "run",
            "--repo",
            "example/project",
            "--issue",
            "7",
            "--repository-root",
            root.to_str().unwrap(),
            "--ledger",
            ledger.to_str().unwrap(),
        ])
        .env("PATH", &bin)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(OPERATION_ERROR));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("cwd must be a relative path without parent components"),
        "unexpected production failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !process_marker.exists(),
        "Planner, Provider, or git process ran"
    );
    assert!(
        !root.join(".ai-dev-orchestrator/worktrees").exists(),
        "workspace manager created a worktree directory"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn publication_lookup_uses_exact_task_id_not_issue_number_suffix() {
    let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
    ledger
        .save_publication(
            &PublicationRecord::new("one/repo#7", "one/repo", "b1").with_task_id("issue-7-one"),
        )
        .unwrap();
    ledger
        .save_publication(
            &PublicationRecord::new("two/repo#7", "two/repo", "b2").with_task_id("issue-7-two"),
        )
        .unwrap();
    assert_eq!(
        ledger
            .load_publication_for_task(&TaskId::new("issue-7-one"))
            .unwrap()
            .unwrap()
            .repository(),
        "one/repo"
    );
    assert_eq!(
        ledger
            .load_publication_for_task(&TaskId::new("issue-7-two"))
            .unwrap()
            .unwrap()
            .repository(),
        "two/repo"
    );
    assert!(
        ledger
            .load_publication_for_task(&TaskId::new("issue-7"))
            .unwrap()
            .is_none()
    );
}
