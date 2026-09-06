//! Full, deterministic orchestration flow.  This test deliberately uses no provider CLI
//! or network: the provider and publication effects are fakes, while the workspace and
//! Rust validator exercise their real implementations.
use ai_dev_orchestrator::*;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}
fn repository() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "ai-dev-e2e-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.email", "e2e@example.invalid"]);
    git(&root, &["config", "user.name", "E2E"]);
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"e2e-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn answer() -> u32 {\n    42\n}\n",
    )
    .unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "fixture"]);
    root
}

#[derive(Clone)]
struct FakeProvider {
    id: ProviderRef,
    calls: Arc<Mutex<usize>>,
}
impl AgentProvider for FakeProvider {
    fn provider_ref(&self) -> &ProviderRef {
        &self.id
    }
    fn check_availability(&self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
        *self.calls.lock().unwrap() += 1;
        fs::create_dir_all(request.workspace().join("src/bin")).unwrap();
        fs::write(
            request.workspace().join("src/bin/agent_fixture.rs"),
            "fn main() {\n    println!(\"agent ok\");\n}\n",
        )
        .unwrap();
        Ok(ProviderResult::new(
            "fake provider edited fixture",
            "",
            Some(0),
            Some(AgentResult::new("implemented", true)),
            None,
        ))
    }
}
#[derive(Clone)]
struct FakePlanner;
impl Planner for FakePlanner {
    fn plan(&self, request: &PlannerRequest) -> Result<PlannerDecision, PlannerError> {
        Ok(PlannerDecision::execute(
            request
                .available_providers()
                .next()
                .unwrap()
                .provider()
                .clone(),
            "fake e2e",
        ))
    }
}

#[derive(Default)]
struct Effects {
    commits: Mutex<Vec<CommitRequest>>,
    pushes: Mutex<Vec<PushRequest>>,
    prs: Mutex<Vec<PullRequestPayload>>,
}
impl RepositoryEffects for Effects {
    fn commit(&self, request: &CommitRequest) -> Result<String, WorkflowError> {
        self.commits.lock().unwrap().push(request.clone());
        Ok("fake-commit-sha".into())
    }
    fn push(&self, request: &PushRequest) -> Result<(), WorkflowError> {
        self.pushes.lock().unwrap().push(request.clone());
        Ok(())
    }
}
impl PullRequestGateway for Effects {
    fn create(&self, payload: &PullRequestPayload) -> Result<String, WorkflowError> {
        self.prs.lock().unwrap().push(payload.clone());
        Ok("https://example.invalid/pr/37".into())
    }
}

#[test]
fn issue_to_publication_completes_without_touching_main() {
    let root = repository();
    let main_head = git(&root, &["rev-parse", "HEAD"]);
    let main_status = git(&root, &["status", "--porcelain"]);
    let source = IssueSnapshot {
        reference: IssueRef::new("example/project", 37),
        title: "Add fixture".into(),
        body: "Create a valid Rust fixture".into(),
    };
    let mut task = issue_to_task(&source);
    let provider_calls = Arc::new(Mutex::new(0));
    let mut registry = ProviderRegistry::new();
    registry.register(FakeProvider {
        id: ProviderRef::new("fake"),
        calls: provider_calls.clone(),
    });
    let planner_request = PlannerRequest::from_task(
        &task,
        [ProviderAvailability::available(ProviderRef::new("fake"))],
    );
    let validated = PlannerService::new(FakePlanner)
        .plan_request(&planner_request)
        .unwrap();
    let policy = ExecutionPolicy::new(1)
        .unwrap()
        .with_timeout("fake", Duration::from_secs(30))
        .unwrap();
    let manager = WorkspaceManager::new(&root).unwrap();
    let orchestrator = Orchestrator::new(manager.clone(), registry, RustValidator::new());
    let report = orchestrator
        .execute_validated_decision_with_policy(
            &mut task,
            &validated,
            AttemptId::new("attempt-1"),
            &policy,
        )
        .unwrap();
    assert_eq!(
        report.attempt().state(),
        AttemptState::Succeeded,
        "validation: {:?}",
        report.validation_result().checks()
    );
    assert_eq!(*provider_calls.lock().unwrap(), 1);
    assert!(
        report
            .workspace()
            .path()
            .join("src/bin/agent_fixture.rs")
            .exists()
    );

    let ledger_path = std::env::temp_dir().join(format!(
        "ai-dev-e2e-ledger-{}.sqlite",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    {
        let ledger = SqliteExecutionLedger::open(&ledger_path).unwrap();
        ledger.save_task(&task).unwrap();
        ledger
            .save_attempt(task.id(), report.attempt(), Some(1), Some(2))
            .unwrap();
    }
    let reopened = SqliteExecutionLedger::open(&ledger_path).unwrap();
    let restored = reopened
        .get_attempt(task.id(), report.attempt().id())
        .unwrap()
        .unwrap();
    assert_eq!(restored.attempt().state(), AttemptState::Succeeded);
    let publication = ValidatedPublication::prepare(source, &report, "issue-37", "main").unwrap();
    let effects = Effects::default();
    assert_eq!(
        GitHubWorkflow::publish(&publication, &reopened, &effects, &effects).unwrap(),
        PublishResult::Published("https://example.invalid/pr/37".into())
    );
    assert_eq!(effects.commits.lock().unwrap().len(), 1);
    assert_eq!(effects.pushes.lock().unwrap().len(), 1);
    assert_eq!(effects.prs.lock().unwrap().len(), 1);
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), main_head);
    assert_eq!(git(&root, &["status", "--porcelain"]), main_status);
    manager.cleanup_force(report.workspace()).unwrap();
    assert!(!report.workspace().path().exists());
    fs::remove_file(&ledger_path).unwrap();
    fs::remove_dir_all(root).unwrap();
}
