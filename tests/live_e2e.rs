//! Opt-in live end-to-end test. Ignored by default; never run in ordinary CI.
use ai_dev_orchestrator::*;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const PROVIDER_INSTRUCTIONS: &str = "Work only in the supplied workspace. Do not commit, push, create, or update a pull request; the Rust workflow performs publication.";

struct PromptProvider(Box<dyn AgentProvider>);
impl AgentProvider for PromptProvider {
    fn provider_ref(&self) -> &ProviderRef {
        self.0.provider_ref()
    }
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
        self.0.execute(&ProviderRequest::new(
            request.workspace().to_owned(),
            format!("{}\n\n{}", request.prompt(), PROVIDER_INSTRUCTIONS),
            request.timeout(),
        ))
    }
    fn check_availability(&self) -> Result<(), ProviderError> {
        self.0.check_availability()
    }
}

struct CleanupGuard {
    manager: WorkspaceManager,
    workspace: Option<Workspace>,
}
impl CleanupGuard {
    fn new(manager: WorkspaceManager) -> Self {
        Self {
            manager,
            workspace: None,
        }
    }
    fn retain(&mut self, workspace: Workspace) {
        self.workspace = Some(workspace);
    }
    fn disarm(&mut self) -> Workspace {
        self.workspace.take().expect("workspace guard")
    }
}
impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if let Some(workspace) = self.workspace.take() {
            let _ = self.manager.cleanup_force(&workspace);
        }
    }
}

#[test]
#[ignore = "requires explicit publication, credentials, provider CLI, and an isolated repository"]
fn live_provider_e2e_requires_explicit_gate() {
    for (name, expected) in [
        ("LIVE_E2E", "1"),
        ("LIVE_PUBLISH", "1"),
        ("LIVE_CREDENTIALS", "1"),
    ] {
        assert_eq!(
            std::env::var(name).as_deref(),
            Ok(expected),
            "{name} must be {expected}"
        );
    }
    let provider_name = std::env::var("LIVE_PROVIDER").expect("LIVE_PROVIDER is required");
    assert!(matches!(
        provider_name.as_str(),
        "codex" | "copilot" | "antigravity"
    ));
    let root = std::env::var("LIVE_REPOSITORY_ROOT").expect("LIVE_REPOSITORY_ROOT is required");
    assert!(Path::new(&root).is_dir(), "repository root does not exist");
    let repository = std::env::var("LIVE_REPOSITORY").expect("LIVE_REPOSITORY is required");
    assert!(
        repository.split('/').count() == 2 && repository.split('/').all(|s| !s.is_empty()),
        "LIVE_REPOSITORY must be owner/name"
    );
    let issue_number: u64 = std::env::var("LIVE_ISSUE_NUMBER")
        .expect("LIVE_ISSUE_NUMBER is required")
        .parse()
        .expect("LIVE_ISSUE_NUMBER must be numeric");
    let ledger_path = PathBuf::from(std::env::var("LIVE_LEDGER").expect("LIVE_LEDGER is required"));
    let timeout = Duration::from_secs(
        std::env::var("LIVE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120),
    );
    assert!(
        !timeout.is_zero(),
        "LIVE_TIMEOUT_SECS must be greater than zero"
    );
    let main_head = git(&root, &["rev-parse", "HEAD"]);
    let main_status = git(&root, &["status", "--porcelain"]);
    let manager = WorkspaceManager::new(&root).expect("invalid repository root");
    let mut guard = CleanupGuard::new(manager.clone());
    let issue_ref = IssueRef::new(&repository, issue_number);
    let issue = GhIssueSource.fetch(&issue_ref).expect("issue fetch failed");
    let mut task = issue_to_task(&issue);
    let provider = match provider_name.as_str() {
        "codex" => PromptProvider(Box::new(CodexProvider::new())),
        "copilot" => PromptProvider(Box::new(CopilotProvider::new())),
        "antigravity" => PromptProvider(Box::new(AntigravityProvider::new())),
        _ => unreachable!(),
    };
    let provider_ref = provider.provider_ref().clone();
    let mut registry = ProviderRegistry::new();
    registry.register(provider);
    let request = PlannerRequest::from_task(
        &task,
        [ProviderAvailability::available(provider_ref.clone())],
    );
    let decision = request
        .validate_decision(PlannerDecision::execute(provider_ref.clone(), "live E2E"))
        .expect("planner decision validation failed");
    let policy = ExecutionPolicy::new(1)
        .expect("policy")
        .with_timeout(provider_ref, timeout)
        .expect("policy timeout");
    let orchestrator = Orchestrator::new(manager.clone(), registry, RustValidator::new());
    let report = match orchestrator.execute_validated_decision_with_policy(
        &mut task,
        &decision,
        AttemptId::new("live-attempt"),
        &policy,
    ) {
        Ok(report) => {
            guard.retain(report.workspace().clone());
            report
        }
        Err(error) => {
            if let Some(workspace) = error.workspace().cloned() {
                guard.retain(workspace);
            }
            panic!(
                "live execution failed; isolated worktree={}",
                error
                    .workspace()
                    .map(|w| w.path().display().to_string())
                    .unwrap_or_else(|| "none".into())
            );
        }
    };
    assert!(report.validation_result().passed());
    assert_eq!(report.provider_result().exit_status(), Some(0));
    assert!(report.provider_result().agent_result().is_some());
    let ledger = SqliteExecutionLedger::open(&ledger_path).expect("ledger open failed");
    ledger.save_task(&task).expect("task ledger save failed");
    ledger
        .save_attempt(task.id(), report.attempt(), None, None)
        .expect("attempt ledger save failed");
    drop(ledger);
    let ledger = SqliteExecutionLedger::open(&ledger_path).expect("ledger reopen failed");
    assert_eq!(
        ledger
            .get_attempt(task.id(), report.attempt().id())
            .expect("ledger read failed")
            .expect("attempt missing")
            .attempt()
            .state(),
        AttemptState::Succeeded
    );
    let publication =
        ValidatedPublication::prepare(issue.clone(), &report, report.workspace().branch(), "main")
            .expect("publication validation failed");
    let result = GitHubWorkflow::publish(
        &publication,
        &ledger,
        &GhRepositoryEffects,
        &GhPullRequestGateway,
    )
    .expect("publication failed");
    let url = match result {
        PublishResult::Published(url) | PublishResult::AlreadyPublished(url) => url,
    };
    assert!(
        url.starts_with("https://"),
        "published URL was not returned"
    );
    let reopened =
        SqliteExecutionLedger::open(&ledger_path).expect("publication ledger reopen failed");
    assert_eq!(
        reopened
            .load_publication(publication.prepared().idempotency_key())
            .expect("publication ledger read failed")
            .expect("publication missing")
            .phase(),
        PublicationPhase::Published
    );
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), main_head);
    assert_eq!(git(&root, &["status", "--porcelain"]), main_status);
    let workspace = guard.disarm();
    manager
        .cleanup_force(&workspace)
        .expect("worktree cleanup failed");
    assert!(!workspace.path().exists());
    let _ = fs::remove_file(ledger_path);
}

fn git(root: &str, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git diagnostic failed");
    assert!(output.status.success(), "git diagnostic failed");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
