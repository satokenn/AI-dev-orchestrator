//! Gated, resumable publication of a validated orchestration result to GitHub.
use std::{
    fmt,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{OrchestrationReport, SqliteExecutionLedger, Task, TaskId, TaskRole};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssueRef {
    pub repository: String,
    pub number: u64,
}
impl IssueRef {
    pub fn new(repository: impl Into<String>, number: u64) -> Self {
        Self {
            repository: repository.into(),
            number,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssueSnapshot {
    pub reference: IssueRef,
    pub title: String,
    pub body: String,
}
pub trait IssueSource {
    fn fetch(&self, issue: &IssueRef) -> Result<IssueSnapshot, WorkflowError>;
}
pub fn issue_to_task(issue: &IssueSnapshot) -> Task {
    Task::new(
        TaskId::new(format!("issue-{}", issue.reference.number)),
        format!("{}\n{}", issue.title, issue.body),
        TaskRole::new("developer"),
    )
}

/// Application boundary for the production hard-gated orchestrator adapter.
pub trait IssueExecutor {
    fn execute_issue(
        &self,
        task: &mut Task,
        issue: &IssueSnapshot,
    ) -> Result<OrchestrationReport, WorkflowError>;
}

/// Fetches an issue, maps it to a Task, and prepares publication without effects.
/// Repository effects begin only when the returned publication is passed to `publish`.
pub fn prepare_issue_publication<S: IssueSource, E: IssueExecutor>(
    source: &S,
    executor: &E,
    issue: &IssueRef,
    _branch: impl Into<String>,
    base: impl Into<String>,
) -> Result<ValidatedPublication, WorkflowError> {
    let snapshot = source.fetch(issue)?;
    let mut task = issue_to_task(&snapshot);
    let report = executor.execute_issue(&mut task, &snapshot)?;
    ValidatedPublication::prepare(snapshot, &report, "", base)
}

pub struct GhIssueSource;
impl IssueSource for GhIssueSource {
    fn fetch(&self, issue: &IssueRef) -> Result<IssueSnapshot, WorkflowError> {
        let number = issue.number.to_string();
        let out = Command::new("gh")
            .args([
                "issue",
                "view",
                &number,
                "--repo",
                &issue.repository,
                "--json",
                "title,body",
            ])
            .output()
            .map_err(|e| WorkflowError::Source(e.to_string()))?;
        if !out.status.success() {
            return Err(WorkflowError::Source(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ));
        }
        let value: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| WorkflowError::Source(e.to_string()))?;
        Ok(IssueSnapshot {
            reference: issue.clone(),
            title: value["title"].as_str().unwrap_or_default().into(),
            body: value["body"].as_str().unwrap_or_default().into(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationPhase {
    Prepared,
    Committed,
    Pushed,
    Published,
}
impl PublicationPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Committed => "committed",
            Self::Pushed => "pushed",
            Self::Published => "published",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        [
            Self::Prepared,
            Self::Committed,
            Self::Pushed,
            Self::Published,
        ]
        .into_iter()
        .find(|p| p.as_str() == s)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationRecord {
    key: String,
    task_id: Option<String>,
    repository: String,
    branch: String,
    commit_sha: Option<String>,
    pull_request: Option<String>,
    phase: PublicationPhase,
    workspace: Option<PathBuf>,
    base: Option<String>,
    title: Option<String>,
    body: Option<String>,
}
impl PublicationRecord {
    pub fn new(
        key: impl Into<String>,
        repository: impl Into<String>,
        branch: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            task_id: None,
            repository: repository.into(),
            branch: branch.into(),
            commit_sha: None,
            pull_request: None,
            phase: PublicationPhase::Prepared,
            workspace: None,
            base: None,
            title: None,
            body: None,
        }
    }
    pub fn idempotency_key(&self) -> &str {
        &self.key
    }
    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }
    pub fn with_task_id(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }
    pub fn repository(&self) -> &str {
        &self.repository
    }
    pub fn branch(&self) -> &str {
        &self.branch
    }
    pub fn workspace(&self) -> Option<&Path> {
        self.workspace.as_deref()
    }
    pub fn base(&self) -> Option<&str> {
        self.base.as_deref()
    }
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
    pub fn body(&self) -> Option<&str> {
        self.body.as_deref()
    }
    pub fn commit_sha(&self) -> Option<&str> {
        self.commit_sha.as_deref()
    }
    pub fn pull_request(&self) -> Option<&str> {
        self.pull_request.as_deref()
    }
    pub const fn phase(&self) -> PublicationPhase {
        self.phase
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore(
        key: String,
        task_id: Option<String>,
        repository: String,
        branch: String,
        commit: Option<String>,
        pr: Option<String>,
        phase: String,
        workspace: Option<String>,
        base: Option<String>,
        title: Option<String>,
        body: Option<String>,
    ) -> Result<Self, crate::LedgerError> {
        let phase = PublicationPhase::parse(&phase).ok_or_else(|| {
            crate::LedgerError::InvalidStoredValue(format!("publication phase: {phase}"))
        })?;
        if phase == PublicationPhase::Published && pr.as_deref().is_none_or(str::is_empty) {
            return Err(crate::LedgerError::InvalidStoredValue(
                "published publication has no pull request URL".into(),
            ));
        }
        Ok(Self {
            key,
            task_id,
            repository,
            branch,
            commit_sha: commit,
            pull_request: pr,
            phase,
            workspace: workspace.map(PathBuf::from),
            base,
            title,
            body,
        })
    }
    pub(crate) fn with_resume_data(
        mut self,
        workspace: &Path,
        base: &str,
        title: &str,
        body: &str,
    ) -> Self {
        self.workspace = Some(workspace.to_path_buf());
        self.base = Some(base.into());
        self.title = Some(title.into());
        self.body = Some(body.into());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitRequest {
    pub workspace: PathBuf,
    pub branch: String,
    pub message: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PushRequest {
    pub workspace: PathBuf,
    pub branch: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullRequestPayload {
    pub repository: String,
    pub head: String,
    pub base: String,
    pub title: String,
    pub body: String,
}
pub trait RepositoryEffects {
    fn commit(&self, request: &CommitRequest) -> Result<String, WorkflowError>;
    fn push(&self, request: &PushRequest) -> Result<(), WorkflowError>;
}
pub trait PullRequestGateway {
    fn create(&self, payload: &PullRequestPayload) -> Result<String, WorkflowError>;
}

pub struct GhRepositoryEffects;
impl RepositoryEffects for GhRepositoryEffects {
    fn commit(&self, r: &CommitRequest) -> Result<String, WorkflowError> {
        validate_git_branch(&r.workspace, &r.branch)?;
        run_git(&r.workspace, ["add", "-A"])?;
        run_git(&r.workspace, ["commit", "-m", &r.message])?;
        git_output(&r.workspace, ["rev-parse", "HEAD"])
    }
    fn push(&self, r: &PushRequest) -> Result<(), WorkflowError> {
        validate_git_branch(&r.workspace, &r.branch)?;
        run_git(
            &r.workspace,
            ["push", "--set-upstream", "origin", &r.branch],
        )
    }
}
fn validate_git_branch(workspace: &Path, expected: &str) -> Result<(), WorkflowError> {
    let inside = git_output(workspace, ["rev-parse", "--is-inside-work-tree"])?;
    if inside != "true" {
        return Err(WorkflowError::Repository(
            "workspace is not a git worktree".into(),
        ));
    }
    let actual = git_output(workspace, ["branch", "--show-current"])?;
    if actual != expected {
        return Err(WorkflowError::Repository(format!(
            "workspace branch mismatch: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}
pub struct GhPullRequestGateway;
impl Default for GhPullRequestGateway {
    fn default() -> Self {
        Self::new()
    }
}
impl GhPullRequestGateway {
    pub fn new() -> Self {
        Self
    }
    pub fn with_executable(executable: impl Into<PathBuf>) -> GhPullRequestGatewayCommand {
        GhPullRequestGatewayCommand {
            executable: executable.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GhPullRequestError {
    Spawn(String),
    CommandFailed {
        operation: &'static str,
        stderr: String,
    },
    InvalidJson {
        operation: &'static str,
        message: String,
    },
    MissingUrl,
}
impl fmt::Display for GhPullRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(message) => write!(f, "could not start gh: {message}"),
            Self::CommandFailed { operation, stderr } => {
                write!(f, "gh {operation} failed: {stderr}")
            }
            Self::InvalidJson { operation, message } => {
                write!(f, "gh {operation} returned invalid JSON: {message}")
            }
            Self::MissingUrl => f.write_str("gh pr list returned an entry without a URL"),
        }
    }
}
impl std::error::Error for GhPullRequestError {}

impl PullRequestGateway for GhPullRequestGateway {
    fn create(&self, p: &PullRequestPayload) -> Result<String, WorkflowError> {
        gh_create_or_get(&PathBuf::from("gh"), p)
    }
}

#[derive(Clone, Debug)]
pub struct GhPullRequestGatewayCommand {
    executable: PathBuf,
}
impl PullRequestGateway for GhPullRequestGatewayCommand {
    fn create(&self, p: &PullRequestPayload) -> Result<String, WorkflowError> {
        gh_create_or_get(&self.executable, p)
    }
}

fn gh_create_or_get(executable: &Path, p: &PullRequestPayload) -> Result<String, WorkflowError> {
    let list = Command::new(executable)
        .args([
            "pr",
            "list",
            "--state",
            "all",
            "--repo",
            &p.repository,
            "--head",
            &p.head,
            "--base",
            &p.base,
            "--json",
            "url",
            "--limit",
            "1",
        ])
        .output()
        .map_err(|e| WorkflowError::PublicationCommand(GhPullRequestError::Spawn(e.to_string())))?;
    if !list.status.success() {
        return Err(WorkflowError::PublicationCommand(
            GhPullRequestError::CommandFailed {
                operation: "pr list",
                stderr: String::from_utf8_lossy(&list.stderr).trim().into(),
            },
        ));
    }
    let existing: serde_json::Value = serde_json::from_slice(&list.stdout).map_err(|e| {
        WorkflowError::PublicationCommand(GhPullRequestError::InvalidJson {
            operation: "pr list",
            message: e.to_string(),
        })
    })?;
    if let Some(entries) = existing.as_array() {
        if let Some(entry) = entries.first() {
            return entry
                .get("url")
                .and_then(serde_json::Value::as_str)
                .map(Into::into)
                .ok_or(WorkflowError::PublicationCommand(
                    GhPullRequestError::MissingUrl,
                ));
        }
    } else {
        return Err(WorkflowError::PublicationCommand(
            GhPullRequestError::InvalidJson {
                operation: "pr list",
                message: "expected an array".into(),
            },
        ));
    }

    let out = Command::new(executable)
        .args([
            "pr",
            "create",
            "--repo",
            &p.repository,
            "--head",
            &p.head,
            "--base",
            &p.base,
            "--title",
            &p.title,
            "--body",
            &p.body,
        ])
        .output()
        .map_err(|e| WorkflowError::PublicationCommand(GhPullRequestError::Spawn(e.to_string())))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().into())
    } else {
        Err(WorkflowError::PublicationCommand(
            GhPullRequestError::CommandFailed {
                operation: "pr create",
                stderr: String::from_utf8_lossy(&out.stderr).trim().into(),
            },
        ))
    }
}
fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<(), WorkflowError> {
    let o = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| WorkflowError::Repository(e.to_string()))?;
    if o.status.success() {
        Ok(())
    } else {
        Err(WorkflowError::Repository(
            String::from_utf8_lossy(&o.stderr).into(),
        ))
    }
}
fn git_output<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<String, WorkflowError> {
    let o = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| WorkflowError::Repository(e.to_string()))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().into())
    } else {
        Err(WorkflowError::Repository(
            String::from_utf8_lossy(&o.stderr).into(),
        ))
    }
}

pub trait PublicationLedger {
    fn load_publication(&self, key: &str) -> Result<Option<PublicationRecord>, WorkflowError>;
    fn save_publication(&self, r: &PublicationRecord) -> Result<(), WorkflowError>;
}
impl PublicationLedger for SqliteExecutionLedger {
    fn load_publication(&self, k: &str) -> Result<Option<PublicationRecord>, WorkflowError> {
        self.load_publication(k)
            .map_err(|e| WorkflowError::Ledger(e.to_string()))
    }
    fn save_publication(&self, r: &PublicationRecord) -> Result<(), WorkflowError> {
        self.save_publication(r)
            .map_err(|e| WorkflowError::Ledger(e.to_string()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedPublication {
    issue: IssueSnapshot,
    report_summary: String,
    workspace: PathBuf,
    branch: String,
    base: String,
    key: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedPublication(PreparedPublication);
impl ValidatedPublication {
    pub fn prepare(
        issue: IssueSnapshot,
        report: &OrchestrationReport,
        _branch: impl Into<String>,
        base: impl Into<String>,
    ) -> Result<Self, WorkflowError> {
        if !report.validation_result().passed() {
            return Err(WorkflowError::Validation(
                "publication requires passed validation".into(),
            ));
        }
        Ok(Self(PreparedPublication {
            key: format!("{}#{}", issue.reference.repository, issue.reference.number),
            issue,
            report_summary: report.validation_result().summary().into(),
            workspace: report.workspace().path().into(),
            branch: report.workspace().branch().into(),
            base: base.into(),
        }))
    }

    pub fn prepared(&self) -> &PreparedPublication {
        &self.0
    }
    /// Reconstructs a publication for resuming after execution has already completed.
    pub fn resume(record: &PublicationRecord, issue: IssueSnapshot) -> Result<Self, WorkflowError> {
        let workspace = record.workspace().ok_or_else(|| {
            WorkflowError::RecoveryRequired("publication record has no workspace".into())
        })?;
        let base = record.base().ok_or_else(|| {
            WorkflowError::RecoveryRequired("publication record has no base".into())
        })?;
        let title = record.title().unwrap_or(&issue.title);
        let body = record.body().unwrap_or(&issue.body);
        Ok(Self(PreparedPublication {
            key: record.idempotency_key().into(),
            issue: IssueSnapshot {
                title: title.into(),
                body: body.into(),
                ..issue
            },
            report_summary: String::new(),
            workspace: workspace.into(),
            branch: record.branch().into(),
            base: base.into(),
        }))
    }
}
impl PreparedPublication {
    pub fn new(
        issue: IssueSnapshot,
        report: &OrchestrationReport,
        branch: impl Into<String>,
        base: impl Into<String>,
    ) -> Result<Self, WorkflowError> {
        ValidatedPublication::prepare(issue, report, branch, base).map(|v| v.0)
    }
    pub fn idempotency_key(&self) -> &str {
        &self.key
    }
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
    pub fn branch(&self) -> &str {
        &self.branch
    }
    pub fn base(&self) -> &str {
        &self.base
    }
    pub fn payload(&self) -> PullRequestPayload {
        PullRequestPayload {
            repository: self.issue.reference.repository.clone(),
            head: self.branch.clone(),
            base: self.base.clone(),
            title: self.issue.title.clone(),
            body: format!(
                "{}\n\n{}\n\nCloses #{}",
                self.issue.body, self.report_summary, self.issue.reference.number
            ),
        }
    }
}

#[derive(Debug)]
pub enum WorkflowError {
    Source(String),
    Repository(String),
    Publication(String),
    PublicationCommand(GhPullRequestError),
    Ledger(String),
    Validation(String),
    RecoveryRequired(String),
}
impl fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(s) => write!(f, "issue source failed: {s}"),
            Self::Repository(s) => write!(f, "repository effect failed: {s}"),
            Self::Publication(s) => write!(f, "publication failed: {s}"),
            Self::PublicationCommand(error) => write!(f, "publication command failed: {error}"),
            Self::Ledger(s) => write!(f, "ledger failed: {s}"),
            Self::Validation(s) => f.write_str(s),
            Self::RecoveryRequired(s) => write!(f, "recovery required: {s}"),
        }
    }
}
impl std::error::Error for WorkflowError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishResult {
    Published(String),
    AlreadyPublished(String),
}
pub struct GitHubWorkflow;
impl GitHubWorkflow {
    pub fn publish<L: PublicationLedger, E: RepositoryEffects, G: PullRequestGateway>(
        p: &ValidatedPublication,
        ledger: &L,
        effects: &E,
        gateway: &G,
    ) -> Result<PublishResult, WorkflowError> {
        let mut r = ledger.load_publication(&p.0.key)?.unwrap_or_else(|| {
            PublicationRecord::new(&p.0.key, &p.0.issue.reference.repository, &p.0.branch)
                .with_task_id(format!("issue-{}", p.0.issue.reference.number))
                .with_resume_data(&p.0.workspace, &p.0.base, &p.0.issue.title, &p.0.issue.body)
        });
        if r.phase == PublicationPhase::Published {
            let url = r.pull_request.ok_or_else(|| {
                WorkflowError::Ledger("published publication has no pull request URL".into())
            })?;
            if url.is_empty() {
                return Err(WorkflowError::Ledger(
                    "published publication has an empty pull request URL".into(),
                ));
            }
            return Ok(PublishResult::AlreadyPublished(url));
        }
        if r.phase == PublicationPhase::Prepared {
            let sha = effects.commit(&CommitRequest {
                workspace: p.0.workspace.clone(),
                branch: p.0.branch.clone(),
                message: format!("Issue #{}: {}", p.0.issue.reference.number, p.0.issue.title),
            })?;
            r.commit_sha = Some(sha.clone());
            r.phase = PublicationPhase::Committed;
            ledger.save_publication(&r).map_err(|e| WorkflowError::RecoveryRequired(format!("commit {sha} completed but ledger save failed: {e}; workspace HEAD may be recovered at {sha}")))?;
        }
        if r.phase == PublicationPhase::Committed {
            effects.push(&PushRequest {
                workspace: p.0.workspace.clone(),
                branch: p.0.branch.clone(),
            })?;
            r.phase = PublicationPhase::Pushed;
            ledger.save_publication(&r)?;
        }
        if r.phase == PublicationPhase::Pushed {
            let payload = p.0.payload();
            let url = gateway.create(&payload)?;
            r.pull_request = Some(url.clone());
            r.phase = PublicationPhase::Published;
            ledger.save_publication(&r)?;
            return Ok(PublishResult::Published(url));
        }
        unreachable!()
    }
}
