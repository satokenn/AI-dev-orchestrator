//! Small, dependency-free command line interface.
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    AgentResult, AntigravityProvider, Attempt, AttemptId, CodexPlanner, CodexProvider,
    CopilotProvider, ExecutionPolicy, GhIssueSource, GhPullRequestGateway, GhRepositoryEffects,
    GitHubWorkflow, IssueExecutor, IssueRef, IssueSnapshot, IssueSource, Orchestrator,
    PlannerRequest, PlannerService, ProviderAvailability, ProviderRef, ProviderRegistry,
    ProviderResolver, PublicationRecord, PublishResult, RustValidator, SqliteExecutionLedger,
    SqliteOperationLedger, Task, TaskId, TaskRole, ValidationResult, WorkflowError,
    WorkspaceManager, prepare_issue_publication,
};

pub const SUCCESS: i32 = 0;
pub const OPERATION_ERROR: i32 = 1;
pub const USAGE_ERROR: i32 = 2;
pub const UNAVAILABLE: i32 = 3;
const DEFAULT_LEDGER: &str = ".ai-dev-orchestrator/ledger.sqlite3";

fn open_ledger_for_run(path: &Path) -> Result<SqliteExecutionLedger, String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "cannot create ledger directory {}: {error}",
                parent.display()
            )
        })?;
    }
    SqliteExecutionLedger::open(path)
        .map_err(|error| format!("cannot open ledger {}: {error}", path.display()))
}

fn open_operation_ledger_for_run(path: &Path) -> Result<Arc<SqliteOperationLedger>, String> {
    let ledger = SqliteOperationLedger::open(path)
        .map_err(|error| format!("cannot open operation ledger: {error}"))?;
    let recovered = ledger
        .recover()
        .map_err(|error| format!("cannot recover operation ledger: {error}"))?;
    for record in recovered {
        eprintln!(
            "operation {} requires recovery: {}",
            record.operation.as_str(),
            record.reason
        );
    }
    Ok(Arc::new(ledger))
}

fn help() -> String {
    format!(
        "ai-dev-orchestrator {}\n\nUSAGE:\n  ai-dev-orchestrator doctor [--json]\n  ai-dev-orchestrator run --repo OWNER/NAME --issue N --repository-root PATH --ledger PATH\n  ai-dev-orchestrator status --task-id ID --ledger PATH [--json]\n\nEXIT CODES: 0 success, 1 operation failure, 2 usage error, 3 provider unavailable\n",
        env!("CARGO_PKG_VERSION")
    )
}

fn value(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

fn doctor(json: bool) -> i32 {
    let providers: Vec<(&str, Result<(), String>)> = vec![
        (
            "codex",
            CodexProvider::new()
                .check_availability()
                .map_err(|e| e.to_string()),
        ),
        (
            "copilot",
            CopilotProvider::new()
                .check_availability()
                .map_err(|e| e.to_string()),
        ),
        (
            "antigravity",
            AntigravityProvider::new()
                .check_availability()
                .map_err(|e| e.to_string()),
        ),
    ];
    if json {
        let entries: Vec<String> = providers
            .iter()
            .map(|(name, result)| {
                format!(
                    r#"{{"provider":"{name}","launchable":{},"authenticated":"unknown"}}"#,
                    result.is_ok()
                )
            })
            .collect();
        println!("{{\"providers\":[{}]}}", entries.join(","));
    } else {
        for (name, result) in &providers {
            match result {
                Ok(()) => println!("{name}: launchable=yes authenticated=unknown"),
                Err(error) => println!("{name}: launchable=no authenticated=unknown ({error})"),
            }
        }
    }
    if providers.iter().all(|(_, result)| result.is_ok()) {
        SUCCESS
    } else {
        UNAVAILABLE
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunRequest {
    pub repository: String,
    pub issue: u64,
    pub repository_root: PathBuf,
    pub ledger: PathBuf,
}

pub trait CliRuntime {
    fn doctor(&self, json: bool) -> i32;
    fn run_issue(&self, request: &RunRequest) -> i32;
    fn status(&self, task_id: &str, ledger: &Path, json: bool) -> i32;
}

pub struct ProductionRuntime;

struct ProductionIssueExecutor {
    root: PathBuf,
    ledger: SqliteExecutionLedger,
    operation_ledger: Arc<SqliteOperationLedger>,
}
impl IssueExecutor for ProductionIssueExecutor {
    fn execute_issue(
        &self,
        task: &mut Task,
        _issue: &IssueSnapshot,
    ) -> Result<crate::OrchestrationReport, WorkflowError> {
        let mut registry = ProviderRegistry::new();
        registry.register(CodexProvider::new());
        registry.register(CopilotProvider::new());
        registry.register(AntigravityProvider::new());
        let providers = ["codex", "copilot", "antigravity"]
            .into_iter()
            .map(|name| {
                let reference = ProviderRef::new(name);
                let available = registry
                    .resolve(&reference)
                    .map(|provider| provider.check_availability().is_ok())
                    .unwrap_or(false);
                ProviderAvailability::new(reference, available)
            })
            .collect::<Vec<_>>();
        let request = PlannerRequest::from_task(task, providers);
        let decision = PlannerService::new(CodexPlanner::new())
            .plan_request(&request)
            .map_err(|e| WorkflowError::Validation(e.to_string()))?;
        let policy = ExecutionPolicy::new(1)
            .and_then(|p| {
                p.with_timeout("codex", Duration::from_secs(3600))
                    .and_then(|p| p.with_timeout("copilot", Duration::from_secs(3600)))
                    .and_then(|p| p.with_timeout("antigravity", Duration::from_secs(3600)))
            })
            .map_err(|e| WorkflowError::Validation(e.to_string()))?;
        let manager = WorkspaceManager::new(&self.root)
            .map_err(|e| WorkflowError::Repository(e.to_string()))?;
        let orchestrator = Orchestrator::new(manager, registry, RustValidator::new())
            .with_operation_ledger(self.operation_ledger.clone());
        let report = orchestrator
            .execute_validated_decision_with_policy(
                task,
                &decision,
                AttemptId::new("attempt-1"),
                &policy,
            )
            .map_err(|e| WorkflowError::Validation(e.to_string()))?;
        self.ledger
            .save_task(task)
            .map_err(|e| WorkflowError::Ledger(e.to_string()))?;
        self.ledger
            .save_attempt(task.id(), report.attempt(), None, None)
            .map_err(|e| WorkflowError::Ledger(e.to_string()))?;
        Ok(report)
    }
}

impl CliRuntime for ProductionRuntime {
    fn doctor(&self, json: bool) -> i32 {
        doctor(json)
    }
    fn run_issue(&self, request: &RunRequest) -> i32 {
        let ledger = match open_ledger_for_run(&request.ledger) {
            Ok(ledger) => ledger,
            Err(error) => {
                eprintln!("{error}");
                return OPERATION_ERROR;
            }
        };
        let operation_ledger = match open_operation_ledger_for_run(&request.ledger) {
            Ok(ledger) => ledger,
            Err(error) => {
                eprintln!("{error}");
                return OPERATION_ERROR;
            }
        };
        let key = format!("{}#{}", request.repository, request.issue);
        let source = GhIssueSource;
        let existing = match ledger.load_publication(&key) {
            Ok(record) => record,
            Err(error) => {
                eprintln!("publication lookup failed: {error}");
                return OPERATION_ERROR;
            }
        };
        if let Some(record) = existing.as_ref() {
            if record.phase() == crate::PublicationPhase::Published {
                println!("published: {}", record.pull_request().unwrap_or_default());
                return SUCCESS;
            }
            if matches!(
                record.phase(),
                crate::PublicationPhase::Committed | crate::PublicationPhase::Pushed
            ) {
                let issue = match source.fetch(&IssueRef::new(&request.repository, request.issue)) {
                    Ok(issue) => issue,
                    Err(error) => {
                        eprintln!("resume failed: {error}");
                        return OPERATION_ERROR;
                    }
                };
                let publication = match crate::ValidatedPublication::resume(record, issue) {
                    Ok(publication) => publication,
                    Err(error) => {
                        eprintln!("resume failed: {error}");
                        return OPERATION_ERROR;
                    }
                };
                return match GitHubWorkflow::publish(
                    &publication,
                    &ledger,
                    &GhRepositoryEffects,
                    &GhPullRequestGateway,
                ) {
                    Ok(PublishResult::Published(url))
                    | Ok(PublishResult::AlreadyPublished(url)) => {
                        println!("published: {url}");
                        SUCCESS
                    }
                    Err(error) => {
                        eprintln!("publication failed: {error}");
                        OPERATION_ERROR
                    }
                };
            }
        }
        let executor = ProductionIssueExecutor {
            root: request.repository_root.clone(),
            ledger,
            operation_ledger,
        };
        let issue = IssueRef::new(&request.repository, request.issue);
        let branch = format!("ai-dev/issue-{}", request.issue);
        let publication =
            match prepare_issue_publication(&source, &executor, &issue, branch, "main") {
                Ok(publication) => publication,
                Err(error) => {
                    eprintln!("run failed: {error}");
                    return OPERATION_ERROR;
                }
            };
        match GitHubWorkflow::publish(
            &publication,
            &executor.ledger,
            &GhRepositoryEffects,
            &GhPullRequestGateway,
        ) {
            Ok(PublishResult::Published(url)) | Ok(PublishResult::AlreadyPublished(url)) => {
                println!("published: {url}");
                SUCCESS
            }
            Err(error) => {
                eprintln!("publication failed: {error}");
                OPERATION_ERROR
            }
        }
    }
    fn status(&self, task_id: &str, ledger: &Path, json: bool) -> i32 {
        status_impl(task_id, ledger, json)
    }
}

#[derive(Default)]
pub struct FakeRuntime;
impl CliRuntime for FakeRuntime {
    fn doctor(&self, json: bool) -> i32 {
        if json {
            println!(
                r#"{{"providers":[{{"provider":"codex","launchable":true,"authenticated":"unknown"}},{{"provider":"copilot","launchable":true,"authenticated":"unknown"}},{{"provider":"antigravity","launchable":true,"authenticated":"unknown"}}]}}"#
            );
        } else {
            println!(
                "codex: launchable=yes authenticated=unknown\ncopilot: launchable=yes authenticated=unknown\nantigravity: launchable=yes authenticated=unknown"
            );
        }
        SUCCESS
    }
    fn run_issue(&self, request: &RunRequest) -> i32 {
        let ledger = match open_ledger_for_run(&request.ledger) {
            Ok(ledger) => ledger,
            Err(error) => {
                eprintln!("{error}");
                return OPERATION_ERROR;
            }
        };
        let task_id = TaskId::new(format!("issue-{}", request.issue));
        let mut task = Task::new(
            task_id.clone(),
            format!("Issue {} in {}", request.issue, request.repository),
            TaskRole::new("developer"),
        );
        let _ = task.start();
        let mut attempt = Attempt::new(AttemptId::new("attempt-1"), ProviderRef::new("codex"));
        let _ = attempt.start();
        let _ = attempt.record_agent_result(AgentResult::new("fake execution", true));
        let _ = attempt.apply_validation(ValidationResult::new("fake validation", true));
        let _ = attempt.finish();
        let _ = task.add_attempt(attempt);
        let _ = task.complete();
        if ledger
            .save_task(&task)
            .and_then(|_| ledger.save_attempt(&task_id, &task.attempts()[0], Some(0), Some(0)))
            .and_then(|_| {
                ledger.save_publication(
                    &PublicationRecord::new(
                        format!("{}#{}", request.repository, request.issue),
                        &request.repository,
                        format!("ai-dev/issue-{}", request.issue),
                    )
                    .with_task_id(task_id.as_str()),
                )
            })
            .is_err()
        {
            return OPERATION_ERROR;
        }
        println!(
            "task {} completed (publication: prepared)",
            task_id.as_str()
        );
        SUCCESS
    }
    fn status(&self, task_id: &str, ledger: &Path, json: bool) -> i32 {
        status_impl(task_id, ledger, json)
    }
}

fn status_impl(id: &str, path: &Path, json: bool) -> i32 {
    let Ok(ledger) = SqliteExecutionLedger::open(path) else {
        eprintln!("cannot open ledger");
        return OPERATION_ERROR;
    };
    let task_id = TaskId::new(id);
    let Ok(Some(task)) = ledger.get_task(&task_id) else {
        eprintln!("task not found: {id}");
        return OPERATION_ERROR;
    };
    let Ok(attempts) = ledger.list_attempts(&task_id) else {
        return OPERATION_ERROR;
    };
    let publication = ledger
        .load_publication(id)
        .ok()
        .flatten()
        .or_else(|| ledger.load_publication_for_task(&task_id).ok().flatten());
    if json {
        println!(
            r#"{{"task_id":"{}","state":"{:?}","attempts":{},"publication_phase":"{}"}}"#,
            task.id().as_str(),
            task.state(),
            attempts.len(),
            publication
                .as_ref()
                .map(|p| p.phase().as_str())
                .unwrap_or("none")
        );
    } else {
        println!("task {} state={:?}", task.id().as_str(), task.state());
        for r in attempts {
            println!(
                "  attempt {} provider={} state={:?}",
                r.attempt().id().as_str(),
                r.attempt().provider().as_str(),
                r.attempt().state()
            );
        }
        println!(
            "publication phase={}",
            publication
                .as_ref()
                .map(|p| p.phase().as_str())
                .unwrap_or("none")
        );
    }
    SUCCESS
}

fn run_command(args: &[String], runtime: &dyn CliRuntime) -> i32 {
    if !validate_options(
        args,
        &["--repo", "--issue", "--repository-root", "--ledger"],
    ) {
        return USAGE_ERROR;
    }
    let Some(repo) = value(args, "--repo") else {
        eprintln!("run requires --repo OWNER/NAME");
        return USAGE_ERROR;
    };
    let Some(issue) = value(args, "--issue") else {
        eprintln!("run requires --issue N");
        return USAGE_ERROR;
    };
    let Some(root) = value(args, "--repository-root") else {
        eprintln!("run requires --repository-root PATH");
        return USAGE_ERROR;
    };
    let ledger_path = value(args, "--ledger").unwrap_or_else(|| DEFAULT_LEDGER.into());
    let Ok(number) = issue.parse::<u64>() else {
        eprintln!("--issue must be an integer");
        return USAGE_ERROR;
    };
    runtime.run_issue(&RunRequest {
        repository: repo,
        issue: number,
        repository_root: PathBuf::from(root),
        ledger: PathBuf::from(ledger_path),
    })
}

fn status_command(args: &[String], runtime: &dyn CliRuntime) -> i32 {
    if !validate_options(args, &["--task-id", "--ledger", "--json"]) {
        return USAGE_ERROR;
    }
    let Some(id) = value(args, "--task-id") else {
        eprintln!("status requires --task-id ID");
        return USAGE_ERROR;
    };
    let Some(path) = value(args, "--ledger") else {
        eprintln!("status requires --ledger PATH");
        return USAGE_ERROR;
    };
    runtime.status(&id, Path::new(&path), args.iter().any(|a| a == "--json"))
}

fn has_only(args: &[String], allowed: &[&str]) -> bool {
    args.iter().all(|arg| allowed.contains(&arg.as_str()))
}

fn validate_options(args: &[String], value_options: &[&str]) -> bool {
    let mut index = 0;
    let mut seen: Vec<&str> = Vec::new();
    while index < args.len() {
        let arg = &args[index];
        if arg == "--json" && value_options.contains(&"--json") {
            if seen.contains(&"--json") {
                eprintln!("duplicate option: {arg}");
                return false;
            }
            seen.push("--json");
            index += 1;
            continue;
        }
        if !value_options.contains(&arg.as_str()) || index + 1 >= args.len() {
            eprintln!("unknown or incomplete option: {arg}");
            return false;
        }
        if args[index + 1].starts_with('-') {
            eprintln!("option requires a value: {arg}");
            return false;
        }
        if seen.contains(&arg.as_str()) {
            eprintln!("duplicate option: {arg}");
            return false;
        }
        seen.push(arg.as_str());
        index += 2;
    }
    true
}

pub fn run_with_runtime<I: IntoIterator<Item = String>>(args: I, runtime: &dyn CliRuntime) -> i32 {
    let args: Vec<String> = args.into_iter().collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        print!("{}", help());
        return SUCCESS;
    }
    if args[0] == "--version" || args[0] == "-V" {
        println!("ai-dev-orchestrator {}", env!("CARGO_PKG_VERSION"));
        return SUCCESS;
    }
    match args[0].as_str() {
        "doctor" if has_only(&args[1..], &["--json"]) => {
            runtime.doctor(args.iter().any(|a| a == "--json"))
        }
        "run" => {
            if args[1..].iter().any(|a| a == "--help" || a == "-h") {
                print!("{}", help());
                return SUCCESS;
            }
            run_command(&args[1..], runtime)
        }
        "status" => status_command(&args[1..], runtime),
        _ => {
            eprintln!("unknown command '{}'.\n{}", args[0], help());
            USAGE_ERROR
        }
    }
}

pub fn run<I: IntoIterator<Item = String>>(args: I) -> i32 {
    run_with_runtime(args, &ProductionRuntime)
}

#[cfg(test)]
mod tests {
    use super::open_operation_ledger_for_run;
    use crate::{
        EventKind, OperationLedger, OperationRequest, OperationStatus, ProviderRef,
        SqliteOperationLedger, TaskId,
    };
    use std::fs;

    fn temporary_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "ai-dev-orchestrator-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn opening_operation_ledger_recovers_unfinished_operations() {
        let root = temporary_path("startup-recovery");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("ledger.sqlite3");
        let (accepted_id, running_id) = {
            let ledger = SqliteOperationLedger::open(&path).unwrap();
            ledger.set_task_revision(&TaskId::new("task-1"), 1).unwrap();
            ledger.set_task_revision(&TaskId::new("task-2"), 1).unwrap();
            let accepted = ledger
                .accept_operation(&OperationRequest::new(
                    "request-1",
                    TaskId::new("task-1"),
                    1,
                    "payload",
                    "instruction",
                    ProviderRef::new("codex"),
                    None,
                ))
                .unwrap();
            let running = ledger
                .accept_operation(&OperationRequest::new(
                    "request-2",
                    TaskId::new("task-2"),
                    1,
                    "payload",
                    "instruction",
                    ProviderRef::new("codex"),
                    None,
                ))
                .unwrap();
            ledger
                .start_operation(running.id(), &ProviderRef::new("codex"), None)
                .unwrap();
            (accepted.id().clone(), running.id().clone())
        };

        let recovered = open_operation_ledger_for_run(&path).unwrap();
        for id in [&accepted_id, &running_id] {
            let operation = recovered.get_operation(id).unwrap().unwrap();
            assert_eq!(operation.status(), OperationStatus::RecoveryRequired);
            assert!(operation.diagnostic().is_some());
            let events = recovered.events(id).unwrap();
            assert_eq!(events.last().unwrap().kind, EventKind::Recovery);
            assert_eq!(
                events.last().unwrap().detail,
                operation.diagnostic().unwrap()
            );
        }
        drop(recovered);

        // Reopening after recovery is idempotent and does not append another event.
        let reopened = open_operation_ledger_for_run(&path).unwrap();
        assert_eq!(reopened.events(&accepted_id).unwrap().len(), 2);
        assert_eq!(reopened.events(&running_id).unwrap().len(), 3);
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operation_ledger_open_failure_is_reported_without_continuing() {
        let root = temporary_path("startup-recovery-failure");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("ledger.sqlite3");
        let sidecar = std::path::PathBuf::from(format!("{}.operations.sqlite3", path.display()));
        fs::write(&sidecar, b"not a sqlite database").unwrap();

        let error = open_operation_ledger_for_run(&path).err().unwrap();
        assert!(error.contains("cannot open operation ledger"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operation_ledger_recovery_failure_is_reported_without_partial_update() {
        let root = temporary_path("startup-recovery-transaction");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("ledger.sqlite3");
        let id = {
            let ledger = SqliteOperationLedger::open(&path).unwrap();
            ledger.set_task_revision(&TaskId::new("task-1"), 1).unwrap();
            ledger
                .accept_operation(&OperationRequest::new(
                    "request-1",
                    TaskId::new("task-1"),
                    1,
                    "payload",
                    "instruction",
                    ProviderRef::new("codex"),
                    None,
                ))
                .unwrap()
                .id()
                .clone()
        };
        let sidecar = std::path::PathBuf::from(format!("{}.operations.sqlite3", path.display()));
        let connection = rusqlite::Connection::open(&sidecar).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_recovery BEFORE INSERT ON operation_events
                 WHEN NEW.kind = 'recovery' BEGIN SELECT RAISE(ABORT, 'injected recovery failure'); END;",
            )
            .unwrap();
        drop(connection);

        let error = open_operation_ledger_for_run(&path).err().unwrap();
        assert!(error.contains("cannot recover operation ledger"));
        let ledger = SqliteOperationLedger::open(&path).unwrap();
        let operation = ledger.get_operation(&id).unwrap().unwrap();
        assert_eq!(operation.status(), OperationStatus::Accepted);
        assert_eq!(operation.diagnostic(), None);
        assert_eq!(ledger.events(&id).unwrap().len(), 1);
        drop(ledger);
        fs::remove_dir_all(root).unwrap();
    }
}
