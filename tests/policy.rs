use ai_dev_orchestrator::{
    AgentProvider, Attempt, ExecutionIntent, ExecutionPolicy, Orchestrator, OrchestratorError,
    PlannerDecision, PlannerRequest, PolicyError, ProviderAvailability, ProviderError, ProviderRef,
    ProviderRegistry, ProviderRequest, ProviderResult, Task, TaskId, TaskRole, ValidationResult,
    Validator, ValidatorError, Workspace, WorkspaceError, WorkspaceManager, WorkspaceManagerPort,
};
use std::collections::VecDeque;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_repository_path() -> PathBuf {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ai-dev-policy-{}-{id}", std::process::id()))
}

fn repository() -> PathBuf {
    let path = loop {
        let candidate = temp_repository_path();
        if fs::create_dir(&candidate).is_ok() {
            break candidate;
        }
    };
    for args in [
        ["init", "-b", "main"],
        ["config", "user.email", "test@example.invalid"],
        ["config", "user.name", "Policy Test"],
    ] {
        let out = Command::new("git")
            .args(args)
            .current_dir(&path)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    fs::write(path.join("README"), "base\n").unwrap();
    for args in [&["add", "README"] as &[&str], &["commit", "-m", "initial"]] {
        let out = Command::new("git")
            .args(args)
            .current_dir(&path)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    path
}

#[derive(Clone, Debug)]
struct FakeProvider {
    id: ProviderRef,
    calls: Arc<Mutex<Vec<ProviderRequest>>>,
    availability: Result<(), ProviderError>,
    results: Arc<Mutex<VecDeque<Result<ProviderResult, ProviderError>>>>,
}
impl AgentProvider for FakeProvider {
    fn provider_ref(&self) -> &ProviderRef {
        &self.id
    }
    fn check_availability(&self) -> Result<(), ProviderError> {
        self.availability.clone()
    }
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
        self.calls.lock().unwrap().push(request.clone());
        self.results.lock().unwrap().pop_front().unwrap()
    }
}

#[derive(Clone, Debug)]
struct FakeValidator;
impl Validator for FakeValidator {
    fn validate(&self, _: &Path) -> Result<ValidationResult, ValidatorError> {
        Ok(ValidationResult::new("ok", true))
    }
}

#[derive(Clone, Debug)]
struct RejectingWorkspace(WorkspaceManager);
impl WorkspaceManagerPort for RejectingWorkspace {
    fn create_for_task_attempt(
        &self,
        task: &Task,
        attempt: &Attempt,
    ) -> Result<Workspace, WorkspaceError> {
        self.0.create_for_task_attempt(task, attempt)
    }
    fn validate_provider_workspace(&self, path: &Path) -> Result<(), WorkspaceError> {
        Err(WorkspaceError::WorkspaceNotManaged {
            path: path.to_owned(),
        })
    }
}

fn provider(id: &str, calls: Arc<Mutex<Vec<ProviderRequest>>>) -> FakeProvider {
    FakeProvider {
        id: ProviderRef::new(id),
        calls,
        availability: Ok(()),
        results: Arc::new(Mutex::new(VecDeque::from([
            Ok(ProviderResult::new("ok", "", Some(0), None, None)),
            Ok(ProviderResult::new("ok", "", Some(0), None, None)),
        ]))),
    }
}

fn service(
    root: &Path,
    providers: Vec<FakeProvider>,
) -> Orchestrator<WorkspaceManager, ProviderRegistry, FakeValidator> {
    let manager = WorkspaceManager::new(root).unwrap();
    let mut registry = ProviderRegistry::new();
    for p in providers {
        registry.register(p);
    }
    Orchestrator::new(manager, registry, FakeValidator)
}

#[test]
fn policy_builders_reject_zero_values_without_an_invalid_policy() {
    assert_eq!(ExecutionPolicy::new(0), Err(PolicyError::MaxAttemptsZero));

    let policy = ExecutionPolicy::new(2).expect("positive policy");
    assert_eq!(
        policy.with_timeout(ProviderRef::new("codex"), Duration::ZERO),
        Err(PolicyError::TimeoutZero {
            provider: ProviderRef::new("codex")
        })
    );
}

#[test]
fn policy_keeps_provider_specific_timeout() {
    let codex = ProviderRef::new("codex");
    let copilot = ProviderRef::new("copilot");
    let policy = ExecutionPolicy::new(2)
        .unwrap()
        .with_timeout(codex.clone(), Duration::from_secs(7))
        .unwrap()
        .with_timeout(copilot.clone(), Duration::from_secs(11))
        .unwrap();

    assert_eq!(policy.timeout_for(&codex), Ok(Duration::from_secs(7)));
    assert_eq!(policy.timeout_for(&copilot), Ok(Duration::from_secs(11)));
    assert!(matches!(
        policy.timeout_for(&ProviderRef::new("unknown")),
        Err(PolicyError::TimeoutMissing { .. })
    ));
}

#[test]
fn planner_service_output_is_the_validated_decision_boundary() {
    let task = Task::new(
        TaskId::new("task-1"),
        "implement",
        TaskRole::new("developer"),
    );
    let request = PlannerRequest::from_task(
        &task,
        [ProviderAvailability::available(ProviderRef::new("codex"))],
    );
    let decision = PlannerDecision::new(
        ProviderRef::new("codex"),
        "use the configured provider",
        ExecutionIntent::Execute,
    );
    let validated = decision.validate(&request).expect("valid planner output");
    assert_eq!(validated.provider().as_str(), "codex");
}

#[test]
fn policy_error_is_preserved_as_error_source() {
    let error = OrchestratorError::Policy(PolicyError::MaxAttemptsZero);
    let source = Error::source(&error).expect("policy source");
    assert!(source.downcast_ref::<PolicyError>().is_some());
}

#[test]
fn hard_policy_rejects_preflight_errors_without_mutation_or_provider_execution() {
    let root = repository();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let service = service(&root, vec![provider("codex", calls.clone())]);
    let decision = PlannerDecision::execute(ProviderRef::new("codex"), "run");
    let policy = ExecutionPolicy::new(1)
        .unwrap()
        .with_timeout("codex", Duration::from_secs(5))
        .unwrap();
    let mut task = Task::new(TaskId::new("task-1"), "implement", TaskRole::new("dev"));

    let before = task.clone();
    assert!(matches!(
        service.execute_decision_with_policy(
            &mut task,
            &PlannerDecision::execute(ProviderRef::new("missing"), "run"),
            ai_dev_orchestrator::AttemptId::new("a1"),
            &policy
        ),
        Err(OrchestratorError::Policy(
            PolicyError::UnknownProvider { .. }
        ))
    ));
    assert_eq!(task, before);
    assert!(calls.lock().unwrap().is_empty());

    let before = task.clone();
    let missing_timeout = ExecutionPolicy::new(1).unwrap();
    assert!(matches!(
        service.execute_decision_with_policy(
            &mut task,
            &decision,
            ai_dev_orchestrator::AttemptId::new("a1"),
            &missing_timeout
        ),
        Err(OrchestratorError::Policy(
            PolicyError::TimeoutMissing { .. }
        ))
    ));
    assert_eq!(task, before);
    assert!(calls.lock().unwrap().is_empty());

    let before = task.clone();
    let zero_max = ExecutionPolicy::new(0).unwrap_err();
    assert_eq!(zero_max, PolicyError::MaxAttemptsZero);
    assert_eq!(task, before);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn terminal_and_duplicate_attempts_are_hard_rejected() {
    let root = repository();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let service = service(&root, vec![provider("codex", calls.clone())]);
    let policy = ExecutionPolicy::new(2)
        .unwrap()
        .with_timeout("codex", Duration::from_secs(5))
        .unwrap();
    let decision = PlannerDecision::execute(ProviderRef::new("codex"), "run");
    let mut task = Task::new(TaskId::new("task-1"), "implement", TaskRole::new("dev"));
    task.start().unwrap();
    let _ = service
        .execute_decision_with_policy(
            &mut task,
            &decision,
            ai_dev_orchestrator::AttemptId::new("a1"),
            &policy,
        )
        .unwrap();
    let before = task.clone();
    assert!(matches!(
        service.execute_decision_with_policy(
            &mut task,
            &decision,
            ai_dev_orchestrator::AttemptId::new("a1"),
            &policy
        ),
        Err(OrchestratorError::Policy(
            PolicyError::DuplicateAttempt { .. }
        ))
    ));
    assert_eq!(task, before);
    task.fail().unwrap();
    let before = task.clone();
    assert!(matches!(
        service.execute_decision_with_policy(
            &mut task,
            &decision,
            ai_dev_orchestrator::AttemptId::new("a2"),
            &policy
        ),
        Err(OrchestratorError::Policy(PolicyError::TaskClosed { .. }))
    ));
    assert_eq!(task, before);
    assert_eq!(calls.lock().unwrap().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn availability_is_checked_before_workspace_and_timeout_reaches_provider() {
    let root = repository();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut unavailable = provider("codex", calls.clone());
    unavailable.availability = Err(ProviderError::Unavailable("not installed".into()));
    let manager = WorkspaceManager::new(&root).unwrap();
    let collision = manager.worktree_path(
        &TaskId::new("task-1"),
        &ai_dev_orchestrator::AttemptId::new("a1"),
    );
    fs::create_dir_all(&collision).unwrap();
    let unavailable_service = Orchestrator::new(
        manager,
        {
            let mut registry = ProviderRegistry::new();
            registry.register(unavailable);
            registry
        },
        FakeValidator,
    );
    let policy = ExecutionPolicy::new(1)
        .unwrap()
        .with_timeout("codex", Duration::from_secs(7))
        .unwrap();
    let decision = PlannerDecision::execute(ProviderRef::new("codex"), "run");
    let mut task = Task::new(TaskId::new("task-1"), "implement", TaskRole::new("dev"));
    assert!(matches!(
        unavailable_service.execute_decision_with_policy(
            &mut task,
            &decision,
            ai_dev_orchestrator::AttemptId::new("a1"),
            &policy
        ),
        Err(OrchestratorError::Policy(
            PolicyError::ProviderUnavailable { .. }
        ))
    ));
    assert!(calls.lock().unwrap().is_empty());
    fs::remove_dir_all(&collision).unwrap();

    let calls = Arc::new(Mutex::new(Vec::new()));
    let service = service(&root, vec![provider("codex", calls.clone())]);
    let mut task = Task::new(TaskId::new("task-2"), "implement", TaskRole::new("dev"));
    let policy = ExecutionPolicy::new(1)
        .unwrap()
        .with_timeout("codex", Duration::from_secs(7))
        .unwrap();
    let report = service
        .execute_decision_with_policy(
            &mut task,
            &decision,
            ai_dev_orchestrator::AttemptId::new("a1"),
            &policy,
        )
        .unwrap();
    assert_eq!(calls.lock().unwrap()[0].timeout(), Duration::from_secs(7));
    assert_eq!(report.attempt().provider().as_str(), "codex");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn policy_entrypoint_supports_pending_start_and_active_provider_switch() {
    let root = repository();
    let first_calls = Arc::new(Mutex::new(Vec::new()));
    let second_calls = Arc::new(Mutex::new(Vec::new()));
    let service = service(
        &root,
        vec![
            provider("codex", first_calls.clone()),
            provider("copilot", second_calls.clone()),
        ],
    );
    let policy = ExecutionPolicy::new(2)
        .unwrap()
        .with_timeout("codex", Duration::from_secs(3))
        .unwrap()
        .with_timeout("copilot", Duration::from_secs(4))
        .unwrap();
    let mut task = Task::new(TaskId::new("task-1"), "implement", TaskRole::new("dev"));
    let first_decision = PlannerDecision::execute(ProviderRef::new("codex"), "run");
    let validated = first_decision
        .validate(&PlannerRequest::from_task(
            &task,
            [ProviderAvailability::available(ProviderRef::new("codex"))],
        ))
        .unwrap();
    let first = service
        .execute_validated_decision_with_policy(
            &mut task,
            &validated,
            ai_dev_orchestrator::AttemptId::new("a1"),
            &policy,
        )
        .unwrap();
    assert_eq!(first.attempt().provider().as_str(), "codex");
    assert_eq!(task.state(), ai_dev_orchestrator::TaskState::Active);
    let second = service
        .execute_decision_with_policy(
            &mut task,
            &PlannerDecision::execute(ProviderRef::new("copilot"), "escalate"),
            ai_dev_orchestrator::AttemptId::new("a2"),
            &policy,
        )
        .unwrap();
    assert_eq!(second.attempt().provider().as_str(), "copilot");
    assert_eq!(first_calls.lock().unwrap().len(), 1);
    assert_eq!(second_calls.lock().unwrap().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn workspace_validation_failure_does_not_execute_provider() {
    let root = repository();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let manager = WorkspaceManager::new(&root).unwrap();
    let mut registry = ProviderRegistry::new();
    registry.register(provider("codex", calls.clone()));
    let service = Orchestrator::new(RejectingWorkspace(manager), registry, FakeValidator);
    let policy = ExecutionPolicy::new(1)
        .unwrap()
        .with_timeout("codex", Duration::from_secs(1))
        .unwrap();
    let mut task = Task::new(TaskId::new("task-1"), "implement", TaskRole::new("dev"));
    let error = service
        .execute_decision_with_policy(
            &mut task,
            &PlannerDecision::execute(ProviderRef::new("codex"), "run"),
            ai_dev_orchestrator::AttemptId::new("a1"),
            &policy,
        )
        .unwrap_err();
    assert!(matches!(error, OrchestratorError::Workspace { .. }));
    assert!(calls.lock().unwrap().is_empty());
    if let Some(workspace) = error.workspace() {
        let _ = WorkspaceManager::new(&root).unwrap().cleanup(workspace);
    }
    fs::remove_dir_all(root).unwrap();
}
