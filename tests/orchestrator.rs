use ai_dev_orchestrator::{
    AgentProvider, AgentResult, AttemptId, AttemptState, CancellationToken, CommandValidator,
    OperationLedger, Orchestrator, OrchestratorError, ProviderError, ProviderRef, ProviderRequest,
    ProviderResult, SqliteExecutionLedger, SqliteOperationLedger, Task, TaskId, TaskRole,
    UsageCost, ValidationCheck, ValidationResult, Validator, ValidatorError, WorkspaceManager,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn temporary_repository() -> PathBuf {
    static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_nanos();
    let repository = std::env::temp_dir().join(format!(
        "ai-dev-orchestrator-orchestrator-{suffix}-{}",
        NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&repository).expect("create repository directory");
    git(&repository, &["init", "-b", "main"]);
    git(
        &repository,
        &["config", "user.email", "test@example.invalid"],
    );
    git(&repository, &["config", "user.name", "Orchestrator Test"]);
    fs::write(repository.join("README"), "base\n").expect("write initial file");
    git(&repository, &["add", "README"]);
    git(&repository, &["commit", "-m", "initial"]);
    repository
}

fn git(directory: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .expect("git is installed");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[derive(Clone)]
struct FakeProvider {
    reference: ProviderRef,
    calls: Arc<Mutex<Vec<ProviderRequest>>>,
    result: Result<ProviderResult, ProviderError>,
}

impl AgentProvider for FakeProvider {
    fn provider_ref(&self) -> &ProviderRef {
        &self.reference
    }

    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
        self.calls
            .lock()
            .expect("provider calls lock")
            .push(request.clone());
        self.result.clone()
    }

    fn check_availability(&self) -> Result<(), ProviderError> {
        Ok(())
    }
}

#[derive(Clone)]
struct FakeValidator {
    calls: Arc<Mutex<Vec<PathBuf>>>,
    result: Result<ValidationResult, ValidatorError>,
}

impl Validator for FakeValidator {
    fn validate(&self, workspace: &Path) -> Result<ValidationResult, ValidatorError> {
        self.calls
            .lock()
            .expect("validator calls lock")
            .push(workspace.to_owned());
        self.result.clone()
    }
}

fn task() -> Task {
    Task::new(
        TaskId::new("task-1"),
        "implement the requested change",
        TaskRole::new("developer"),
    )
}

#[test]
fn successful_attempt_records_agent_result_and_validation_once() {
    let repository = temporary_repository();
    let manager = WorkspaceManager::new(&repository).expect("resolve repository");
    let provider_calls = Arc::new(Mutex::new(Vec::new()));
    let validator_calls = Arc::new(Mutex::new(Vec::new()));
    let provider = FakeProvider {
        reference: ProviderRef::new("fake-provider"),
        calls: provider_calls.clone(),
        result: Ok(ProviderResult::new(
            "provider stdout",
            "",
            Some(0),
            Some(AgentResult::new("implemented", true)),
            Some(UsageCost::default()),
        )),
    };
    let validator = FakeValidator {
        calls: validator_calls.clone(),
        result: Ok(ValidationResult::new("checks passed", true)),
    };
    let operation_ledger = Arc::new(SqliteOperationLedger::open_in_memory().unwrap());
    let service = Orchestrator::new(manager.clone(), provider, validator)
        .with_operation_ledger(operation_ledger.clone());
    let mut task = task();

    let report = service
        .execute(
            &mut task,
            AttemptId::new("attempt-1"),
            Duration::from_secs(30),
        )
        .expect("orchestration succeeds");

    assert_eq!(task.state(), ai_dev_orchestrator::TaskState::Active);
    assert_eq!(task.attempts().len(), 1);
    assert_eq!(
        task.attempts()[0].state(),
        ai_dev_orchestrator::AttemptState::Succeeded
    );
    assert_eq!(task.attempts()[0].validation_results().len(), 1);
    assert_eq!(
        task.attempts()[0].agent_result().unwrap().summary(),
        "implemented"
    );
    assert_eq!(
        report.attempt().state(),
        ai_dev_orchestrator::AttemptState::Succeeded
    );
    assert_eq!(provider_calls.lock().unwrap().len(), 1);
    assert_eq!(validator_calls.lock().unwrap().len(), 1);
    assert_eq!(
        validator_calls.lock().unwrap()[0],
        report.workspace().path()
    );
    assert!(report.workspace().path().exists());
    let operation = operation_ledger
        .accept_operation(&ai_dev_orchestrator::OperationRequest::new(
            "attempt:attempt-1",
            task.id().clone(),
            1,
            task.description(),
            task.description(),
            ProviderRef::new("fake-provider"),
            ai_dev_orchestrator::ModelChoice::ProviderDefault,
        ))
        .expect("operation is persisted");
    assert_eq!(
        operation_ledger
            .get_operation(operation.id())
            .unwrap()
            .unwrap()
            .status(),
        ai_dev_orchestrator::OperationStatus::Succeeded
    );
    assert_eq!(operation_ledger.events(operation.id()).unwrap().len(), 4);
    manager
        .cleanup(report.workspace())
        .expect("clean workspace");
    fs::remove_dir_all(repository).expect("remove repository");
}

#[test]
fn orchestrator_passes_caller_cancellation_to_validator() {
    let repository = temporary_repository();
    let manager = WorkspaceManager::new(&repository).expect("resolve repository");
    let provider = FakeProvider {
        reference: ProviderRef::new("fake-provider"),
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Ok(ProviderResult::new(
            "provider stdout",
            "",
            Some(0),
            Some(AgentResult::new("implemented", true)),
            Some(UsageCost::default()),
        )),
    };
    let validator = CommandValidator::new([ValidationCheck::new("cancelled-check", "true")]);
    let service = Orchestrator::new(manager, provider, validator);
    let mut task = task();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = service
        .execute_with_cancellation(
            &mut task,
            AttemptId::new("attempt-cancelled-validation"),
            Duration::from_secs(1),
            cancellation,
        )
        .expect_err("cancellation must remain distinct from invalid output");
    match error {
        OrchestratorError::Validator { error, attempt, .. } => {
            assert_eq!(error, ValidatorError::Cancelled);
            assert_eq!(attempt.state(), AttemptState::Succeeded);
            assert!(attempt.validation_results().is_empty());
        }
        other => panic!("expected validator cancellation, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn command_output_is_not_returned_or_persisted_in_validation_diagnostics() {
    const SENTINEL: &str = "VALIDATOR_SENTINEL_SECRET";
    let repository = temporary_repository();
    let manager = WorkspaceManager::new(&repository).expect("resolve repository");
    let provider = FakeProvider {
        reference: ProviderRef::new("fake-provider"),
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Ok(ProviderResult::new(
            "provider stdout",
            "",
            Some(0),
            Some(AgentResult::new("implemented", true)),
            Some(UsageCost::default()),
        )),
    };
    let validator = CommandValidator::new([ValidationCheck::new("secret-output", "sh").args([
        "-c",
        &format!("printf '{SENTINEL}'; printf '{SENTINEL}' >&2; exit 17"),
    ])]);
    let service = Orchestrator::new(manager.clone(), provider, validator);
    let mut task = task();
    let report = service
        .execute(
            &mut task,
            AttemptId::new("attempt-secret-output"),
            Duration::from_secs(1),
        )
        .expect("non-zero validation result is recorded safely");
    let validation = report.validation_result();
    assert!(!validation.passed());
    assert_eq!(validation.checks()[0].exit_status(), Some(17));
    assert_eq!(
        validation.checks()[0].diagnostics(),
        "process exited with a non-zero status"
    );
    assert!(!validation.summary().contains(SENTINEL));
    assert!(!validation.checks()[0].diagnostics().contains(SENTINEL));
    assert!(
        !report.attempt().validation_results()[0].checks()[0]
            .diagnostics()
            .contains(SENTINEL)
    );

    let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
    ledger.save_task(&task).unwrap();
    ledger
        .save_attempt(task.id(), report.attempt(), None, None)
        .unwrap();
    let loaded = ledger
        .get_attempt(task.id(), report.attempt().id())
        .unwrap()
        .unwrap();
    let persisted = &loaded.attempt().validation_results()[0];
    assert!(!persisted.summary().contains(SENTINEL));
    assert!(!persisted.checks()[0].diagnostics().contains(SENTINEL));
    manager
        .cleanup(report.workspace())
        .expect("cleanup workspace");
    fs::remove_dir_all(repository).expect("cleanup repository");
}

#[test]
fn provider_failure_keeps_failed_attempt_and_workspace_diagnostics() {
    let repository = temporary_repository();
    let manager = WorkspaceManager::new(&repository).expect("resolve repository");
    let provider = FakeProvider {
        reference: ProviderRef::new("fake-provider"),
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Err(ProviderError::ExecutionFailed("agent crashed".to_owned())),
    };
    let validator_calls = Arc::new(Mutex::new(Vec::new()));
    let validator = FakeValidator {
        calls: validator_calls.clone(),
        result: Ok(ValidationResult::new("unused", true)),
    };
    let service = Orchestrator::new(manager.clone(), provider, validator);
    let mut task = task();

    let error = service
        .execute(
            &mut task,
            AttemptId::new("attempt-1"),
            Duration::from_secs(30),
        )
        .expect_err("provider failure must be returned");

    assert!(matches!(error, OrchestratorError::Provider { .. }));
    assert_eq!(
        error.provider_error().unwrap().to_string(),
        "provider execution failed: agent crashed"
    );
    assert_eq!(
        error.attempt().unwrap().state(),
        ai_dev_orchestrator::AttemptState::Failed
    );
    let workspace = error.workspace().expect("workspace is retained in error");
    assert!(workspace.path().exists());
    assert_eq!(task.state(), ai_dev_orchestrator::TaskState::Active);
    assert_eq!(
        task.attempts()[0].state(),
        ai_dev_orchestrator::AttemptState::Failed
    );
    assert!(validator_calls.lock().unwrap().is_empty());
    manager.cleanup(workspace).expect("clean workspace");
    fs::remove_dir_all(repository).expect("remove repository");
}

#[test]
fn workspace_failure_keeps_one_failed_attempt_in_task() {
    let repository = temporary_repository();
    let manager = WorkspaceManager::new(&repository).expect("resolve repository");
    let path = manager.worktree_path(&TaskId::new("task-1"), &AttemptId::new("attempt-1"));
    fs::create_dir_all(&path).expect("create colliding workspace path");
    let provider = FakeProvider {
        reference: ProviderRef::new("fake-provider"),
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Ok(ProviderResult::new("unused", "", Some(0), None, None)),
    };
    let validator = FakeValidator {
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Ok(ValidationResult::new("unused", true)),
    };
    let service = Orchestrator::new(manager, provider, validator);
    let mut task = task();

    let error = service
        .execute(
            &mut task,
            AttemptId::new("attempt-1"),
            Duration::from_secs(30),
        )
        .expect_err("workspace collision must fail orchestration");

    assert!(matches!(error, OrchestratorError::Workspace { .. }));
    assert!(error.workspace().is_none());
    assert_eq!(task.state(), ai_dev_orchestrator::TaskState::Active);
    assert_eq!(task.attempts().len(), 1);
    assert_eq!(
        task.attempts()[0].state(),
        ai_dev_orchestrator::AttemptState::Failed
    );
    fs::remove_dir_all(path).expect("remove colliding path");
    fs::remove_dir_all(repository).expect("remove repository");
}

#[test]
fn validation_failure_fails_attempt_but_keeps_task_active() {
    let repository = temporary_repository();
    let manager = WorkspaceManager::new(&repository).expect("resolve repository");
    let provider = FakeProvider {
        reference: ProviderRef::new("fake-provider"),
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Ok(ProviderResult::new(
            "provider stdout",
            "provider stderr",
            Some(0),
            Some(AgentResult::new("reported success", true)),
            None,
        )),
    };
    let validator_calls = Arc::new(Mutex::new(Vec::new()));
    let validator = FakeValidator {
        calls: validator_calls.clone(),
        result: Ok(ValidationResult::new("checks failed", false)),
    };
    let service = Orchestrator::new(manager.clone(), provider, validator);
    let mut task = task();

    let report = service
        .execute(
            &mut task,
            AttemptId::new("attempt-1"),
            Duration::from_secs(30),
        )
        .expect("validation completed with a failed result");

    assert!(!report.validation_result().passed());
    assert_eq!(
        report.attempt().state(),
        ai_dev_orchestrator::AttemptState::Failed
    );
    assert_eq!(report.attempt().validation_results().len(), 1);
    assert_eq!(task.state(), ai_dev_orchestrator::TaskState::Active);
    assert_eq!(
        task.attempts()[0].state(),
        ai_dev_orchestrator::AttemptState::Failed
    );
    assert_eq!(validator_calls.lock().unwrap().len(), 1);
    manager
        .cleanup(report.workspace())
        .expect("clean workspace");
    fs::remove_dir_all(repository).expect("remove repository");
}

#[test]
fn validator_execution_error_keeps_one_failed_attempt_and_workspace() {
    let repository = temporary_repository();
    let manager = WorkspaceManager::new(&repository).expect("resolve repository");
    let provider = FakeProvider {
        reference: ProviderRef::new("fake-provider"),
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Ok(ProviderResult::new(
            "provider stdout",
            "",
            Some(0),
            Some(AgentResult::new("reported success", true)),
            None,
        )),
    };
    let validator = FakeValidator {
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Err(ValidatorError::NoChecksConfigured),
    };
    let service = Orchestrator::new(manager.clone(), provider, validator);
    let mut task = task();

    let error = service
        .execute(
            &mut task,
            AttemptId::new("attempt-1"),
            Duration::from_secs(30),
        )
        .expect_err("validator execution error must be returned");

    assert!(matches!(error, OrchestratorError::Validator { .. }));
    assert_eq!(task.state(), ai_dev_orchestrator::TaskState::Active);
    assert_eq!(task.attempts().len(), 1);
    assert_eq!(
        task.attempts()[0].state(),
        ai_dev_orchestrator::AttemptState::Failed
    );
    let workspace = error.workspace().expect("workspace is retained in error");
    assert!(workspace.path().exists());
    manager.cleanup(workspace).expect("clean workspace");
    fs::remove_dir_all(repository).expect("remove repository");
}
