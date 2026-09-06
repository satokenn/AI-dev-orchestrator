use ai_dev_orchestrator::{
    AgentProvider, AttemptFailureReason, AttemptId, AttemptState, Orchestrator, OrchestratorError,
    PlannerDecision, ProviderError, ProviderRef, ProviderRegistry, ProviderRequest, ProviderResult,
    RetryPolicy, SqliteExecutionLedger, Task, TaskId, TaskRole, ValidationResult, Validator,
    ValidatorError, WorkspaceManager,
};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn repository() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "ai-dev-orchestrator-retry-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    git(&path, &["init", "-b", "main"]);
    git(&path, &["config", "user.email", "test@example.invalid"]);
    git(&path, &["config", "user.name", "Retry Test"]);
    fs::write(path.join("README"), "base\n").unwrap();
    git(&path, &["add", "README"]);
    git(&path, &["commit", "-m", "initial"]);
    path
}

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn task() -> Task {
    Task::new(
        TaskId::new("task-1"),
        "implement change",
        TaskRole::new("developer"),
    )
}

#[derive(Clone, Debug)]
struct FakeProvider {
    reference: ProviderRef,
    calls: Arc<Mutex<Vec<PathBuf>>>,
    results: Arc<Mutex<VecDeque<Result<ProviderResult, ProviderError>>>>,
}

impl AgentProvider for FakeProvider {
    fn provider_ref(&self) -> &ProviderRef {
        &self.reference
    }
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
        self.calls
            .lock()
            .unwrap()
            .push(request.workspace().to_owned());
        self.results.lock().unwrap().pop_front().unwrap()
    }

    fn check_availability(&self) -> Result<(), ProviderError> {
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct FakeValidator {
    results: Arc<Mutex<VecDeque<Result<ValidationResult, ValidatorError>>>>,
}
impl Validator for FakeValidator {
    fn validate(&self, _: &Path) -> Result<ValidationResult, ValidatorError> {
        self.results.lock().unwrap().pop_front().unwrap()
    }
}

fn success() -> ProviderResult {
    ProviderResult::new("ok", "", Some(0), None, None)
}
fn service(
    root: &Path,
    providers: Vec<FakeProvider>,
    validation: Vec<Result<ValidationResult, ValidatorError>>,
) -> Orchestrator<WorkspaceManager, ProviderRegistry, FakeValidator> {
    let manager = WorkspaceManager::new(root).unwrap();
    let mut registry = ProviderRegistry::new();
    for provider in providers {
        registry.register(provider);
    }
    let validator = FakeValidator {
        results: Arc::new(Mutex::new(validation.into_iter().collect())),
    };
    Orchestrator::new(manager, registry, validator)
}

#[test]
fn validation_failure_retries_same_provider_in_new_workspace() {
    let root = repository();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider = FakeProvider {
        reference: ProviderRef::new("one"),
        calls: calls.clone(),
        results: Arc::new(Mutex::new(VecDeque::from([Ok(success()), Ok(success())]))),
    };
    let service = service(
        &root,
        vec![provider],
        vec![
            Ok(ValidationResult::new("bad", false)),
            Ok(ValidationResult::new("good", true)),
        ],
    );
    let mut task = task();
    let first = service
        .execute_decision(
            &mut task,
            &PlannerDecision::execute(ProviderRef::new("one"), "run"),
            AttemptId::new("a1"),
            Duration::from_secs(1),
            RetryPolicy::new(2),
        )
        .unwrap();
    assert_eq!(first.attempt().state(), AttemptState::Failed);
    assert_eq!(
        first.attempt().failure_reason(),
        Some(&AttemptFailureReason::Validation)
    );
    let second = service
        .retry_after(
            &mut task,
            &OrchestratorError::Validator {
                error: ValidatorError::NoChecksConfigured,
                attempt: Box::new(first.attempt().clone()),
                workspace: Box::new(first.workspace().clone()),
            },
            AttemptId::new("a2"),
            Duration::from_secs(1),
            RetryPolicy::new(2),
        )
        .unwrap();
    assert_eq!(second.attempt().state(), AttemptState::Succeeded);
    let paths = calls.lock().unwrap();
    assert_eq!(paths.len(), 2);
    assert_ne!(paths[0], paths[1]);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn generated_attempt_ids_are_unique_across_retries() {
    let root = repository();
    let provider = FakeProvider {
        reference: ProviderRef::new("one"),
        calls: Arc::new(Mutex::new(Vec::new())),
        results: Arc::new(Mutex::new(VecDeque::from([Ok(success()), Ok(success())]))),
    };
    let service = service(
        &root,
        vec![provider],
        vec![
            Ok(ValidationResult::new("bad", false)),
            Ok(ValidationResult::new("good", true)),
        ],
    );
    let mut task = task();
    let first = service
        .execute_decision_generated(
            &mut task,
            &PlannerDecision::execute(ProviderRef::new("one"), "run"),
            Duration::from_secs(1),
            RetryPolicy::new(2),
        )
        .unwrap();
    let second = service
        .execute_decision_generated(
            &mut task,
            &PlannerDecision::execute(ProviderRef::new("one"), "retry"),
            Duration::from_secs(1),
            RetryPolicy::new(2),
        )
        .unwrap();
    assert_ne!(first.attempt().id(), second.attempt().id());
    assert_eq!(task.attempts().len(), 2);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn provider_failure_escalates_to_different_provider() {
    let root = repository();
    let one_calls = Arc::new(Mutex::new(Vec::new()));
    let two_calls = Arc::new(Mutex::new(Vec::new()));
    let one = FakeProvider {
        reference: ProviderRef::new("one"),
        calls: one_calls.clone(),
        results: Arc::new(Mutex::new(VecDeque::from([Err(
            ProviderError::ExecutionFailed("down".into()),
        )]))),
    };
    let two = FakeProvider {
        reference: ProviderRef::new("two"),
        calls: two_calls.clone(),
        results: Arc::new(Mutex::new(VecDeque::from([Ok(success())]))),
    };
    let service = service(
        &root,
        vec![one, two],
        vec![Ok(ValidationResult::new("good", true))],
    );
    let mut task = task();
    let first = service
        .execute_decision(
            &mut task,
            &PlannerDecision::execute(ProviderRef::new("one"), "run"),
            AttemptId::new("a1"),
            Duration::from_secs(1),
            RetryPolicy::new(2),
        )
        .unwrap_err();
    let report = service
        .escalate(
            &mut task,
            ProviderRef::new("two"),
            AttemptId::new("a2"),
            Duration::from_secs(1),
            RetryPolicy::new(2),
        )
        .unwrap();
    assert_eq!(first.attempt().unwrap().provider().as_str(), "one");
    assert_eq!(report.attempt().provider().as_str(), "two");
    assert_eq!(one_calls.lock().unwrap().len(), 1);
    assert_eq!(two_calls.lock().unwrap().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn max_attempts_one_fails_task_without_new_calls() {
    let root = repository();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider = FakeProvider {
        reference: ProviderRef::new("one"),
        calls: calls.clone(),
        results: Arc::new(Mutex::new(VecDeque::from([Err(
            ProviderError::ExecutionFailed("down".into()),
        )]))),
    };
    let service = service(&root, vec![provider], vec![]);
    let mut task = task();
    let _ = service.execute_decision_generated(
        &mut task,
        &PlannerDecision::execute(ProviderRef::new("one"), "run"),
        Duration::from_secs(1),
        RetryPolicy::new(1),
    );
    let before = calls.lock().unwrap().len();
    let error = service
        .execute_decision_generated(
            &mut task,
            &PlannerDecision::execute(ProviderRef::new("one"), "retry"),
            Duration::from_secs(1),
            RetryPolicy::new(1),
        )
        .unwrap_err();
    assert!(matches!(error, OrchestratorError::MaxAttempts { .. }));
    assert_eq!(task.state(), ai_dev_orchestrator::TaskState::Failed);
    assert_eq!(calls.lock().unwrap().len(), before);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn timeout_and_cancel_policy_controls_retry() {
    for failure in [
        ProviderError::TimedOut {
            timeout: Duration::from_secs(1),
        },
        ProviderError::Cancelled,
    ] {
        let root = repository();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let provider = FakeProvider {
            reference: ProviderRef::new("one"),
            calls: calls.clone(),
            results: Arc::new(Mutex::new(VecDeque::from([
                Err(failure.clone()),
                Ok(success()),
            ]))),
        };
        let service = service(
            &root,
            vec![provider],
            vec![Ok(ValidationResult::new("good", true))],
        );
        let mut task = task();
        let first = service
            .execute_decision(
                &mut task,
                &PlannerDecision::execute(ProviderRef::new("one"), "run"),
                AttemptId::new("a1"),
                Duration::from_secs(1),
                RetryPolicy::new(2),
            )
            .unwrap_err();
        assert!(matches!(
            service.retry_after(
                &mut task,
                &first,
                AttemptId::new("a2"),
                Duration::from_secs(1),
                RetryPolicy::new(2)
            ),
            Err(OrchestratorError::RetryNotAllowed)
        ));
        assert_eq!(calls.lock().unwrap().len(), 1);
        let policy = match failure {
            ProviderError::TimedOut { .. } => RetryPolicy::new(2).with_timeout_retry(true),
            ProviderError::Cancelled => RetryPolicy::new(2).with_cancellation_retry(true),
            _ => unreachable!(),
        };
        assert!(
            service
                .retry_after(
                    &mut task,
                    &first,
                    AttemptId::new("a2"),
                    Duration::from_secs(1),
                    policy
                )
                .is_ok()
        );
        assert_eq!(calls.lock().unwrap().len(), 2);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn ledger_round_trips_two_attempts_and_failure_reason() {
    let root = repository();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider = FakeProvider {
        reference: ProviderRef::new("one"),
        calls,
        results: Arc::new(Mutex::new(VecDeque::from([Ok(success()), Ok(success())]))),
    };
    let service = service(
        &root,
        vec![provider],
        vec![
            Ok(ValidationResult::new("bad", false)),
            Ok(ValidationResult::new("good", true)),
        ],
    );
    let mut task = task();
    let first = service
        .execute_decision(
            &mut task,
            &PlannerDecision::execute(ProviderRef::new("one"), "run"),
            AttemptId::new("a1"),
            Duration::from_secs(1),
            RetryPolicy::new(2),
        )
        .unwrap();
    let first_snapshot = first.attempt().clone();
    let failure = OrchestratorError::Validator {
        error: ValidatorError::NoChecksConfigured,
        attempt: Box::new(first_snapshot.clone()),
        workspace: Box::new(first.workspace().clone()),
    };
    let second = service
        .retry_after(
            &mut task,
            &failure,
            AttemptId::new("a2"),
            Duration::from_secs(1),
            RetryPolicy::new(2),
        )
        .unwrap();
    let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
    ledger.save_task(&task).unwrap();
    ledger
        .save_attempt(task.id(), &first_snapshot, Some(1), Some(2))
        .unwrap();
    ledger
        .save_attempt(task.id(), second.attempt(), Some(3), Some(4))
        .unwrap();
    let records = ledger.list_attempts(task.id()).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0].attempt().failure_reason(),
        Some(&AttemptFailureReason::Validation)
    );
    assert_eq!(
        ledger
            .get_attempt(task.id(), &AttemptId::new("a1"))
            .unwrap()
            .unwrap()
            .attempt(),
        &first_snapshot
    );
    fs::remove_dir_all(root).unwrap();
}
