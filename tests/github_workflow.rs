use ai_dev_orchestrator::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_name(prefix: &str, suffix: &str) -> PathBuf {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}-{}-{id}{suffix}", std::process::id()))
}

#[cfg(unix)]
fn fake_gh(mode: &str) -> (PathBuf, PathBuf, PathBuf) {
    let script = temp_name("fake-gh", ".sh");
    // Publish the executable atomically. On macOS, spawning a path while it
    // is still being created or chmod'd can fail with ETXTBSY ("Text file
    // busy") when this integration test runs in parallel with other tests.
    let script_tmp = temp_name("fake-gh", ".sh.tmp");
    let log = temp_name("fake-gh", ".log");
    let created = temp_name("fake-gh", ".created");
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$2\" = list ]; then\n  if [ '{}' = existing ]; then printf '%s' '[{{\"url\":\"https://github/pr/99\"}}]'; elif [ '{}' = invalid ]; then printf '%s' '{{'; elif [ '{}' = nonzero ]; then printf '%s' 'list failed' >&2; exit 17; else printf '%s' '[]'; fi\nelse\n  touch '{}'\n  printf '%s' 'https://github/pr/new'\nfi\n",
        log.display(),
        mode,
        mode,
        mode,
        created.display()
    );
    fs::write(&script_tmp, body).unwrap();
    fs::set_permissions(&script_tmp, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&script_tmp, &script).unwrap();
    (script, log, created)
}

fn payload() -> PullRequestPayload {
    PullRequestPayload {
        repository: "acme/project".into(),
        head: "feature".into(),
        base: "main".into(),
        title: "Title".into(),
        body: "Body".into(),
    }
}

fn repo() -> PathBuf {
    let p = loop {
        let candidate = temp_name("ai-dev-gh", "");
        if fs::create_dir(&candidate).is_ok() {
            break candidate;
        }
    };
    for args in [
        &["init", "-b", "main"] as &[&str],
        &["config", "user.email", "test@example.invalid"],
        &["config", "user.name", "GH Test"],
        &["add", "."],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&p)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    fs::write(p.join("README"), "base\n").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "README"])
            .current_dir(&p)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&p)
            .output()
            .unwrap()
            .status
            .success()
    );
    p
}

#[derive(Clone, Debug)]
struct Provider {
    id: ProviderRef,
}
impl AgentProvider for Provider {
    fn provider_ref(&self) -> &ProviderRef {
        &self.id
    }
    fn check_availability(&self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn execute(&self, _: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
        Ok(ProviderResult::new("ok", "", Some(0), None, None))
    }
}
#[derive(Clone, Debug)]
struct ValidatorFake(bool);
impl Validator for ValidatorFake {
    fn validate(&self, _: &Path) -> Result<ValidationResult, ValidatorError> {
        Ok(ValidationResult::new("validation", self.0))
    }
}

fn publication(passed: bool) -> (PathBuf, Result<ValidatedPublication, WorkflowError>) {
    let root = repo();
    let manager = WorkspaceManager::new(&root).unwrap();
    let mut registry = ProviderRegistry::new();
    registry.register(Provider {
        id: ProviderRef::new("codex"),
    });
    let service = Orchestrator::new(manager, registry, ValidatorFake(passed));
    let mut task = Task::new(TaskId::new("task-7"), "implement", TaskRole::new("dev"));
    let report = service
        .execute_decision(
            &mut task,
            &PlannerDecision::execute(ProviderRef::new("codex"), "run"),
            AttemptId::new("a1"),
            Duration::from_secs(1),
            RetryPolicy::new(1),
        )
        .unwrap();
    let issue = IssueSnapshot {
        reference: IssueRef::new("acme/project", 7),
        title: "Fix bug".into(),
        body: "Details".into(),
    };
    (
        root,
        ValidatedPublication::prepare(issue, &report, "issue-7", "main"),
    )
}

#[derive(Clone, Debug)]
struct Effects {
    commits: Arc<Mutex<Vec<CommitRequest>>>,
    pushes: Arc<Mutex<Vec<PushRequest>>>,
    commit_error: bool,
    push_error: bool,
}
impl RepositoryEffects for Effects {
    fn commit(&self, r: &CommitRequest) -> Result<String, WorkflowError> {
        self.commits.lock().unwrap().push(r.clone());
        if self.commit_error {
            Err(WorkflowError::Repository("commit failed".into()))
        } else {
            Ok("abc123".into())
        }
    }
    fn push(&self, r: &PushRequest) -> Result<(), WorkflowError> {
        self.pushes.lock().unwrap().push(r.clone());
        if self.push_error {
            Err(WorkflowError::Repository("push failed".into()))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug)]
struct Gateway {
    calls: Arc<Mutex<Vec<PullRequestPayload>>>,
    error: bool,
}
impl PullRequestGateway for Gateway {
    fn create(&self, p: &PullRequestPayload) -> Result<String, WorkflowError> {
        self.calls.lock().unwrap().push(p.clone());
        if self.error {
            Err(WorkflowError::Publication("pr failed".into()))
        } else {
            Ok("https://github/pr/7".into())
        }
    }
}
#[derive(Clone, Debug, Default)]
struct Ledger(Arc<Mutex<HashMap<String, PublicationRecord>>>);
impl PublicationLedger for Ledger {
    fn load_publication(&self, key: &str) -> Result<Option<PublicationRecord>, WorkflowError> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn save_publication(&self, r: &PublicationRecord) -> Result<(), WorkflowError> {
        self.0
            .lock()
            .unwrap()
            .insert(r.idempotency_key().into(), r.clone());
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct FailPublishedSaveLedger {
    records: Arc<Mutex<HashMap<String, PublicationRecord>>>,
    failed: Arc<Mutex<bool>>,
}
impl PublicationLedger for FailPublishedSaveLedger {
    fn load_publication(&self, key: &str) -> Result<Option<PublicationRecord>, WorkflowError> {
        Ok(self.records.lock().unwrap().get(key).cloned())
    }
    fn save_publication(&self, record: &PublicationRecord) -> Result<(), WorkflowError> {
        if record.phase() == PublicationPhase::Published && !*self.failed.lock().unwrap() {
            *self.failed.lock().unwrap() = true;
            return Err(WorkflowError::Ledger("simulated save failure".into()));
        }
        self.records
            .lock()
            .unwrap()
            .insert(record.idempotency_key().into(), record.clone());
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct IdempotentGateway {
    calls: Arc<Mutex<u32>>,
}
impl PullRequestGateway for IdempotentGateway {
    fn create(&self, _: &PullRequestPayload) -> Result<String, WorkflowError> {
        *self.calls.lock().unwrap() += 1;
        Ok("https://github/pr/idempotent".into())
    }
}

#[derive(Clone, Debug)]
struct SourceFake {
    fail: bool,
}
impl IssueSource for SourceFake {
    fn fetch(&self, issue: &IssueRef) -> Result<IssueSnapshot, WorkflowError> {
        if self.fail {
            Err(WorkflowError::Source("fetch failed".into()))
        } else {
            Ok(IssueSnapshot {
                reference: issue.clone(),
                title: "Fix bug".into(),
                body: "Details".into(),
            })
        }
    }
}
#[derive(Clone, Debug)]
struct ExecutorFake;
impl IssueExecutor for ExecutorFake {
    fn execute_issue(
        &self,
        _: &mut Task,
        _: &IssueSnapshot,
    ) -> Result<OrchestrationReport, WorkflowError> {
        Err(WorkflowError::Repository("provider failed".into()))
    }
}

#[test]
fn publication_record_round_trips_in_the_execution_sqlite_ledger() {
    let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
    let record = PublicationRecord::new("acme/project#7", "acme/project", "issue-7");
    ledger.save_publication(&record).unwrap();
    let loaded = ledger.load_publication("acme/project#7").unwrap().unwrap();
    assert_eq!(loaded, record);
    assert_eq!(loaded.phase(), PublicationPhase::Prepared);
}

#[test]
fn validation_failure_is_rejected_before_any_effect() {
    let (root, result) = publication(false);
    assert!(matches!(result, Err(WorkflowError::Validation(_))));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn publish_gates_commit_push_and_pr_and_preserves_resume_phases() {
    let (root, p) = publication(true);
    let p = p.unwrap();
    let ledger = Ledger::default();
    let effects = Effects {
        commits: Arc::new(Mutex::new(Vec::new())),
        pushes: Arc::new(Mutex::new(Vec::new())),
        commit_error: false,
        push_error: false,
    };
    let gateway = Gateway {
        calls: Arc::new(Mutex::new(Vec::new())),
        error: false,
    };
    assert_eq!(
        GitHubWorkflow::publish(&p, &ledger, &effects, &gateway).unwrap(),
        PublishResult::Published("https://github/pr/7".into())
    );
    assert_eq!(effects.commits.lock().unwrap().len(), 1);
    assert_eq!(effects.pushes.lock().unwrap().len(), 1);
    assert_eq!(
        effects.commits.lock().unwrap()[0].branch,
        p.prepared().branch()
    );
    assert_eq!(
        effects.pushes.lock().unwrap()[0].branch,
        p.prepared().branch()
    );
    assert_eq!(gateway.calls.lock().unwrap().len(), 1);
    let payload = gateway.calls.lock().unwrap()[0].clone();
    assert_eq!(
        (
            &payload.repository,
            &payload.head,
            &payload.base,
            &payload.title
        ),
        (
            &"acme/project".into(),
            &p.prepared().branch().to_owned(),
            &"main".into(),
            &"Fix bug".into()
        )
    );
    assert_eq!(payload.body, "Details\n\nvalidation\n\nCloses #7");
    assert_eq!(
        GitHubWorkflow::publish(&p, &ledger, &effects, &gateway).unwrap(),
        PublishResult::AlreadyPublished("https://github/pr/7".into())
    );
    assert_eq!(effects.commits.lock().unwrap().len(), 1);
    assert_eq!(effects.pushes.lock().unwrap().len(), 1);
    assert_eq!(gateway.calls.lock().unwrap().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failures_stop_at_each_phase_and_resume_skips_completed_effects() {
    let (root, p) = publication(true);
    let p = p.unwrap();
    let ledger = Ledger::default();
    let commit_bad = Effects {
        commits: Arc::new(Mutex::new(Vec::new())),
        pushes: Arc::new(Mutex::new(Vec::new())),
        commit_error: true,
        push_error: false,
    };
    let gateway = Gateway {
        calls: Arc::new(Mutex::new(Vec::new())),
        error: false,
    };
    assert!(GitHubWorkflow::publish(&p, &ledger, &commit_bad, &gateway).is_err());
    assert!(commit_bad.pushes.lock().unwrap().is_empty());
    assert!(gateway.calls.lock().unwrap().is_empty());
    assert!(
        ledger
            .load_publication(p.prepared().idempotency_key())
            .unwrap()
            .is_none()
    );
    let push_bad = Effects {
        commits: Arc::new(Mutex::new(Vec::new())),
        pushes: Arc::new(Mutex::new(Vec::new())),
        commit_error: false,
        push_error: true,
    };
    assert!(GitHubWorkflow::publish(&p, &ledger, &push_bad, &gateway).is_err());
    assert_eq!(
        ledger
            .load_publication(p.prepared().idempotency_key())
            .unwrap()
            .unwrap()
            .phase(),
        PublicationPhase::Committed
    );
    assert!(push_bad.pushes.lock().unwrap().len() == 1);
    let pr_bad = Gateway {
        calls: Arc::new(Mutex::new(Vec::new())),
        error: true,
    };
    let good = Effects {
        commits: Arc::new(Mutex::new(Vec::new())),
        pushes: Arc::new(Mutex::new(Vec::new())),
        commit_error: false,
        push_error: false,
    };
    assert!(GitHubWorkflow::publish(&p, &ledger, &good, &pr_bad).is_err());
    assert_eq!(
        ledger
            .load_publication(p.prepared().idempotency_key())
            .unwrap()
            .unwrap()
            .phase(),
        PublicationPhase::Pushed
    );
    assert!(good.commits.lock().unwrap().is_empty());
    assert!(good.pushes.lock().unwrap().len() == 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn issue_snapshot_maps_to_task() {
    let issue = IssueSnapshot {
        reference: IssueRef::new("acme/project", 42),
        title: "Title".into(),
        body: "Body".into(),
    };
    let task = issue_to_task(&issue);
    assert_eq!(task.id().as_str(), "issue-42");
    assert!(task.description().contains("Title"));
    assert!(task.description().contains("Body"));
}

#[test]
fn sqlite_publication_survives_reopen_and_is_idempotent() {
    let path = loop {
        let candidate = temp_name("ai-dev-publication", ".sqlite");
        if fs::File::options()
            .write(true)
            .create_new(true)
            .open(&candidate)
            .is_ok()
        {
            break candidate;
        }
    };
    let record = PublicationRecord::new("acme/project#7", "acme/project", "issue-7");
    {
        let ledger = SqliteExecutionLedger::open(&path).unwrap();
        ledger.save_publication(&record).unwrap();
        ledger.save_publication(&record).unwrap();
    }
    let ledger = SqliteExecutionLedger::open(&path).unwrap();
    assert_eq!(
        ledger.load_publication(record.idempotency_key()).unwrap(),
        Some(record)
    );
    fs::remove_file(path).unwrap();
}

#[test]
fn issue_coordinator_maps_and_stops_before_effects_on_fetch_or_execution_error() {
    let (root, publication) = publication(true);
    let publication = publication.unwrap();
    let issue = IssueRef::new("acme/project", 7);
    let effects = Effects {
        commits: Arc::new(Mutex::new(Vec::new())),
        pushes: Arc::new(Mutex::new(Vec::new())),
        commit_error: false,
        push_error: false,
    };
    let source_error = SourceFake { fail: true };
    let executor = ExecutorFake;
    assert!(
        prepare_issue_publication(&source_error, &executor, &issue, "issue-7", "main").is_err()
    );
    assert!(effects.commits.lock().unwrap().is_empty());
    assert!(effects.pushes.lock().unwrap().is_empty());
    let source = SourceFake { fail: false };
    assert!(prepare_issue_publication(&source, &executor, &issue, "issue-7", "main").is_err());
    assert!(effects.commits.lock().unwrap().is_empty());
    assert!(effects.pushes.lock().unwrap().is_empty());
    let ledger = Ledger::default();
    let gateway = Gateway {
        calls: Arc::new(Mutex::new(Vec::new())),
        error: false,
    };
    let _ = GitHubWorkflow::publish(&publication, &ledger, &effects, &gateway);
    assert_eq!(
        gateway.calls.lock().unwrap()[0].body,
        "Details\n\nvalidation\n\nCloses #7"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sqlite_resume_skips_committed_pushed_and_published_effects() {
    let (root, p) = publication(true);
    let p = p.unwrap();
    let path = temp_name("ai-dev-resume", ".sqlite");
    let ledger = SqliteExecutionLedger::open(&path).unwrap();
    let issue = IssueSnapshot {
        reference: IssueRef::new("acme/project", 7),
        title: "Fix bug".into(),
        body: "Details".into(),
    };
    let committed_effects = Effects {
        commits: Arc::new(Mutex::new(Vec::new())),
        pushes: Arc::new(Mutex::new(Vec::new())),
        commit_error: false,
        push_error: true,
    };
    let gateway = Gateway {
        calls: Arc::new(Mutex::new(Vec::new())),
        error: false,
    };
    assert!(GitHubWorkflow::publish(&p, &ledger, &committed_effects, &gateway).is_err());
    let committed = ledger
        .load_publication(p.prepared().idempotency_key())
        .unwrap()
        .unwrap();
    let resumed = ValidatedPublication::resume(&committed, issue.clone()).unwrap();
    let effects = Effects {
        commits: Arc::new(Mutex::new(Vec::new())),
        pushes: Arc::new(Mutex::new(Vec::new())),
        commit_error: false,
        push_error: false,
    };
    // Resume the committed record and fail at PR creation to persist Pushed.
    let pr_bad = Gateway {
        calls: Arc::new(Mutex::new(Vec::new())),
        error: true,
    };
    let _ = GitHubWorkflow::publish(&resumed, &ledger, &effects, &pr_bad);
    let pushed = ledger
        .load_publication(p.prepared().idempotency_key())
        .unwrap()
        .unwrap();
    assert_eq!(pushed.phase(), PublicationPhase::Pushed);
    let resumed = ValidatedPublication::resume(&pushed, issue).unwrap();
    let effects = Effects {
        commits: Arc::new(Mutex::new(Vec::new())),
        pushes: Arc::new(Mutex::new(Vec::new())),
        commit_error: false,
        push_error: false,
    };
    GitHubWorkflow::publish(&resumed, &ledger, &effects, &gateway).unwrap();
    assert!(effects.commits.lock().unwrap().is_empty());
    assert!(effects.pushes.lock().unwrap().is_empty());
    let published = ledger
        .load_publication(p.prepared().idempotency_key())
        .unwrap()
        .unwrap();
    let resumed = ValidatedPublication::resume(
        &published,
        IssueSnapshot {
            reference: IssueRef::new("acme/project", 7),
            title: "Fix bug".into(),
            body: "Details".into(),
        },
    )
    .unwrap();
    let effects = Effects {
        commits: Arc::new(Mutex::new(Vec::new())),
        pushes: Arc::new(Mutex::new(Vec::new())),
        commit_error: false,
        push_error: false,
    };
    assert!(matches!(
        GitHubWorkflow::publish(&resumed, &ledger, &effects, &gateway),
        Ok(PublishResult::AlreadyPublished(_))
    ));
    assert!(effects.commits.lock().unwrap().is_empty());
    assert!(effects.pushes.lock().unwrap().is_empty());
    fs::remove_file(path).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn resume_requires_workspace_and_base_data() {
    let record = PublicationRecord::new("acme/project#7", "acme/project", "issue-7");
    let issue = IssueSnapshot {
        reference: IssueRef::new("acme/project", 7),
        title: "Fix bug".into(),
        body: "Details".into(),
    };
    assert!(matches!(
        ValidatedPublication::resume(&record, issue),
        Err(WorkflowError::RecoveryRequired(_))
    ));
}

#[cfg(unix)]
#[test]
fn gh_gateway_gets_existing_pr_without_create_and_creates_when_missing() {
    let (existing_script, existing_log, existing_created) = fake_gh("existing");
    let existing = GhPullRequestGateway::with_executable(&existing_script)
        .create(&payload())
        .unwrap();
    assert_eq!(existing, "https://github/pr/99");
    assert!(!existing_created.exists());
    assert!(
        fs::read_to_string(existing_log)
            .unwrap()
            .contains("pr list")
    );

    let (new_script, _, new_created) = fake_gh("new");
    let created = GhPullRequestGateway::with_executable(&new_script)
        .create(&payload())
        .unwrap();
    assert_eq!(created, "https://github/pr/new");
    assert!(new_created.exists());
}

#[cfg(unix)]
#[test]
fn gh_gateway_reports_invalid_json_and_nonzero_as_typed_errors() {
    let (invalid_script, _, _) = fake_gh("invalid");
    assert!(matches!(
        GhPullRequestGateway::with_executable(invalid_script).create(&payload()),
        Err(WorkflowError::PublicationCommand(_))
    ));
    let (failed_script, _, _) = fake_gh("nonzero");
    assert!(matches!(
        GhPullRequestGateway::with_executable(failed_script).create(&payload()),
        Err(WorkflowError::PublicationCommand(_))
    ));
}

#[test]
fn workflow_retry_after_published_save_failure_relies_on_gateway_idempotency() {
    let (root, publication) = publication(true);
    let publication = publication.unwrap();
    let ledger = FailPublishedSaveLedger {
        records: Arc::new(Mutex::new(HashMap::new())),
        failed: Arc::new(Mutex::new(false)),
    };
    let effects = Effects {
        commits: Arc::new(Mutex::new(Vec::new())),
        pushes: Arc::new(Mutex::new(Vec::new())),
        commit_error: false,
        push_error: false,
    };
    let gateway = IdempotentGateway::default();
    assert!(GitHubWorkflow::publish(&publication, &ledger, &effects, &gateway).is_err());
    assert!(matches!(
        GitHubWorkflow::publish(&publication, &ledger, &effects, &gateway),
        Ok(PublishResult::Published(url)) if url == "https://github/pr/idempotent"
    ));
    assert_eq!(*gateway.calls.lock().unwrap(), 2);
    assert_eq!(effects.commits.lock().unwrap().len(), 1);
    assert_eq!(effects.pushes.lock().unwrap().len(), 1);
    fs::remove_dir_all(root).unwrap();
}
