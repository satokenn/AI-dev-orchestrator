//! Git-tree Artifacts persisted in the same SQLite file as Tasks and Attempts.

use std::{
    ffi::OsString,
    fmt, fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::{
    AttemptId, LedgerError, ProcessError, ProcessRequest, ProcessRunner, SqliteExecutionLedger,
    TaskId, Workspace, WorkspaceError, WorkspaceManager,
};

const REF_PREFIX: &str = "refs/ai-dev-orchestrator/artifacts/";
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactRecord {
    id: String,
    task_id: TaskId,
    source_attempt_id: Option<String>,
    input_artifact_id: Option<String>,
    base_commit: String,
    tree_oid: String,
    repository_root: PathBuf,
    ref_name: String,
    state: ArtifactState,
}

impl ArtifactRecord {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    pub fn source_attempt_id(&self) -> Option<&str> {
        self.source_attempt_id.as_deref()
    }
    pub fn input_artifact_id(&self) -> Option<&str> {
        self.input_artifact_id.as_deref()
    }
    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }
    pub fn tree_oid(&self) -> &str {
        &self.tree_oid
    }
    pub fn state(&self) -> ArtifactState {
        self.state
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactState {
    PendingRef,
    Available,
    RecoveryRequired,
}

#[derive(Debug)]
pub enum ArtifactError {
    Ledger(LedgerError),
    Workspace(WorkspaceError),
    Git(String),
    Io(String),
    NotFound,
    Invalid(String),
    RecoveryRequired,
    IgnoredFiles,
    WorkspaceChanged { expected: String, actual: String },
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ledger(e) => write!(f, "artifact ledger error: {e}"),
            Self::Workspace(e) => write!(f, "artifact workspace error: {e}"),
            Self::Git(e) => write!(f, "artifact Git error: {e}"),
            Self::Io(e) => write!(f, "artifact I/O error: {e}"),
            Self::NotFound => f.write_str("artifact was not found"),
            Self::Invalid(e) => write!(f, "invalid artifact: {e}"),
            Self::RecoveryRequired => f.write_str("artifact requires recovery"),
            Self::IgnoredFiles => {
                f.write_str("ignored workspace files cannot be captured as an artifact")
            }
            Self::WorkspaceChanged { expected, actual } => {
                write!(f, "workspace changed: expected {expected}, found {actual}")
            }
        }
    }
}
impl std::error::Error for ArtifactError {}
impl From<LedgerError> for ArtifactError {
    fn from(e: LedgerError) -> Self {
        Self::Ledger(e)
    }
}
impl From<rusqlite::Error> for ArtifactError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Ledger(LedgerError::from(e))
    }
}
impl From<WorkspaceError> for ArtifactError {
    fn from(e: WorkspaceError) -> Self {
        Self::Workspace(e)
    }
}

pub(crate) struct ArtifactManager<'a> {
    workspaces: &'a WorkspaceManager,
    ledger: &'a SqliteExecutionLedger,
    runner: ProcessRunner,
    git_executable: OsString,
}

impl<'a> ArtifactManager<'a> {
    pub(crate) fn new(workspaces: &'a WorkspaceManager, ledger: &'a SqliteExecutionLedger) -> Self {
        Self {
            workspaces,
            ledger,
            runner: ProcessRunner,
            git_executable: OsString::from("git"),
        }
    }

    pub(crate) fn read(&self, task: &TaskId, id: &str) -> Result<ArtifactRecord, ArtifactError> {
        let mut record = self.load(task, id)?.ok_or(ArtifactError::NotFound)?;
        if record.repository_root != self.workspaces.repository_root() {
            return Err(ArtifactError::Invalid("repository mismatch".into()));
        }
        if record.ref_name != format!("{REF_PREFIX}{}", record.id)
            || !record
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(ArtifactError::Invalid(
                "invalid dedicated Artifact ref".into(),
            ));
        }
        if record.state == ArtifactState::RecoveryRequired {
            return Err(ArtifactError::RecoveryRequired);
        }
        if let Err(error) = self.ensure_tree(&record) {
            if matches!(error, ArtifactError::Invalid(_)) {
                self.set_state(task, id, ArtifactState::RecoveryRequired)?;
                return Err(ArtifactError::RecoveryRequired);
            }
            return Err(error);
        }
        match self.ref_target(&record)? {
            Some(target) if target == record.tree_oid => {}
            Some(_) => {
                self.set_state(task, id, ArtifactState::RecoveryRequired)?;
                return Err(ArtifactError::RecoveryRequired);
            }
            None => self.install_ref(&record)?,
        }
        if record.state == ArtifactState::PendingRef {
            self.set_state(task, id, ArtifactState::Available)?;
            record.state = ArtifactState::Available;
        }
        Ok(record)
    }

    pub(crate) fn materialize(
        &self,
        workspace: &Workspace,
        task: &TaskId,
        attempt: &AttemptId,
        id: &str,
    ) -> Result<(), ArtifactError> {
        let record = self.read(task, id)?;
        if record.repository_root != workspace.repository_root() {
            return Err(ArtifactError::Invalid("repository mismatch".into()));
        }
        self.workspaces.ensure_fresh_artifact_workspace(
            workspace,
            task,
            attempt,
            &record.base_commit,
        )?;
        self.run_git(
            workspace.path(),
            &["read-tree", "--reset", "-u", &record.tree_oid],
            &[],
        )?;
        let actual = self.snapshot(workspace.path())?;
        if actual != record.tree_oid {
            return Err(ArtifactError::WorkspaceChanged {
                expected: record.tree_oid,
                actual,
            });
        }
        Ok(())
    }

    pub(crate) fn capture(
        &self,
        workspace: &Workspace,
        task: &TaskId,
        attempt: &AttemptId,
        input_id: Option<&str>,
        initial_base: &str,
    ) -> Result<ArtifactRecord, ArtifactError> {
        self.workspaces
            .validate_artifact_workspace(workspace, task, attempt)?;
        let base_commit = if let Some(id) = input_id {
            self.read(task, id)?.base_commit
        } else {
            initial_base.to_owned()
        };
        let base_commit = self.normalize_commit(workspace.path(), &base_commit)?;
        let tree_oid = self.snapshot(workspace.path())?;
        let id = new_id();
        let ref_name = format!("{REF_PREFIX}{id}");
        let root = self.workspaces.repository_root();
        let record = ArtifactRecord {
            id: id.clone(),
            task_id: task.clone(),
            source_attempt_id: Some(attempt.as_str().to_owned()),
            input_artifact_id: input_id.map(str::to_owned),
            base_commit,
            tree_oid,
            repository_root: root.to_owned(),
            ref_name,
            state: ArtifactState::PendingRef,
        };
        self.persist_pending(task, attempt, input_id, &record)?;
        self.install_ref(&record)?;
        self.set_state(task, &record.id, ArtifactState::Available)?;
        let mut available = record;
        available.state = ArtifactState::Available;
        Ok(available)
    }

    fn persist_pending(
        &self,
        task: &TaskId,
        attempt: &AttemptId,
        input_id: Option<&str>,
        record: &ArtifactRecord,
    ) -> Result<(), ArtifactError> {
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(parent) = input_id {
            let available: Option<String> = tx.query_row("SELECT id FROM service_artifacts WHERE task_id=?1 AND id=?2 AND state='available'", params![task.as_str(), parent], |row| row.get(0)).optional()?;
            if available.is_none() {
                return Err(ArtifactError::NotFound);
            }
        }
        tx.execute("INSERT INTO service_artifacts(id,task_id,source_attempt_id,input_artifact_id,base_commit,tree_oid,repository_root,ref_name,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending_ref',?9)", params![record.id, task.as_str(), record.source_attempt_id.as_deref(), input_id, record.base_commit, record.tree_oid, record.repository_root.to_string_lossy().as_ref(), record.ref_name, now_ms()])?;
        tx.execute("UPDATE service_attempt_artifacts SET output_artifact_id=?3 WHERE task_id=?1 AND attempt_id=?2", params![task.as_str(), attempt.as_str(), record.id])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn verify_input(
        &self,
        task: &TaskId,
        id: &str,
    ) -> Result<ArtifactRecord, ArtifactError> {
        self.read(task, id)
    }

    /// Completes ref installation for durable pending records while the Service owns
    /// the persistent Ledger process lock. Missing objects become RecoveryRequired;
    /// transient Git failures abort startup without changing the pending record.
    pub(crate) fn recover_pending(&self) -> Result<(), ArtifactError> {
        let repository = self
            .workspaces
            .repository_root()
            .to_string_lossy()
            .into_owned();
        let connection = self.ledger.lock_connection()?;
        let mut statement = connection.prepare(
            "SELECT task_id,id FROM service_artifacts
             WHERE state='pending_ref' AND repository_root=?1 ORDER BY created_at,id",
        )?;
        let pending = statement
            .query_map(params![repository], |row| {
                Ok((
                    TaskId::new(row.get::<_, String>(0)?),
                    row.get::<_, String>(1)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        for (task, id) in pending {
            match self.read(&task, &id) {
                Ok(_) | Err(ArtifactError::RecoveryRequired) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn load(&self, task: &TaskId, id: &str) -> Result<Option<ArtifactRecord>, ArtifactError> {
        let connection = self.ledger.lock_connection()?;
        connection.query_row("SELECT id,source_attempt_id,input_artifact_id,base_commit,tree_oid,repository_root,ref_name,state FROM service_artifacts WHERE task_id=?1 AND id=?2", params![task.as_str(), id], |row| {
            let state: String = row.get(7)?;
            let state = match state.as_str() { "pending_ref" => ArtifactState::PendingRef, "available" => ArtifactState::Available, "recovery_required" => ArtifactState::RecoveryRequired, _ => return Err(rusqlite::Error::InvalidQuery) };
            Ok(ArtifactRecord { id: row.get(0)?, task_id: task.clone(), source_attempt_id: row.get(1)?, input_artifact_id: row.get(2)?, base_commit: row.get(3)?, tree_oid: row.get(4)?, repository_root: PathBuf::from(row.get::<_, String>(5)?), ref_name: row.get(6)?, state })
        }).optional().map_err(Into::into)
    }

    fn set_state(
        &self,
        task: &TaskId,
        id: &str,
        state: ArtifactState,
    ) -> Result<(), ArtifactError> {
        let text = match state {
            ArtifactState::PendingRef => "pending_ref",
            ArtifactState::Available => "available",
            ArtifactState::RecoveryRequired => "recovery_required",
        };
        let connection = self.ledger.lock_connection()?;
        connection.execute(
            "UPDATE service_artifacts SET state=?3 WHERE task_id=?1 AND id=?2",
            params![task.as_str(), id, text],
        )?;
        Ok(())
    }

    fn snapshot(&self, path: &Path) -> Result<String, ArtifactError> {
        self.reject_ignored_files(path)?;
        let temp = TempIndex::create()?;
        let index = temp.path().join("index").to_string_lossy().into_owned();
        let index_env = [("GIT_INDEX_FILE", index)];
        self.run_git(path, &["read-tree", "HEAD"], &index_env)?;
        // Ignored files are deliberately excluded; the temporary index preserves the user's index.
        self.run_git(path, &["add", "-A", "--", "."], &index_env)?;
        let output = self.run_git(path, &["write-tree"], &index_env)?;
        let tree = String::from_utf8(output.stdout)
            .map_err(|_| ArtifactError::Invalid("tree id is not UTF-8".into()))?
            .trim()
            .to_owned();
        if !is_oid(&tree) {
            return Err(ArtifactError::Invalid("invalid Git tree id".into()));
        }
        Ok(tree)
    }

    fn reject_ignored_files(&self, path: &Path) -> Result<(), ArtifactError> {
        let output = self.run_git(
            path,
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignored=matching",
            ],
            &[],
        )?;
        if output.output_truncated {
            return Err(ArtifactError::Git(
                "workspace status output truncated".into(),
            ));
        }
        if output
            .stdout
            .split(|byte| *byte == b'\n')
            .any(|line| line.starts_with(b"!! "))
        {
            return Err(ArtifactError::IgnoredFiles);
        }
        Ok(())
    }

    fn normalize_commit(&self, cwd: &Path, oid: &str) -> Result<String, ArtifactError> {
        if !is_oid(oid) {
            return Err(ArtifactError::Invalid("invalid base commit".into()));
        }
        let expr = format!("{oid}^{{commit}}");
        let output = self.run_git(
            cwd,
            &["rev-parse", "--verify", "--end-of-options", &expr],
            &[],
        )?;
        let actual = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if actual != oid {
            return Err(ArtifactError::Invalid(
                "base commit is not canonical".into(),
            ));
        }
        Ok(actual)
    }

    fn ensure_tree(&self, record: &ArtifactRecord) -> Result<(), ArtifactError> {
        if !is_oid(&record.tree_oid) {
            return Err(ArtifactError::Invalid("invalid Git tree object ID".into()));
        }
        let output = match self.runner.run(
            ProcessRequest::new(self.git_executable.clone())
                .args(["cat-file", "-t", &record.tree_oid])
                .cwd(&record.repository_root),
        ) {
            Ok(output) => output,
            Err(ProcessError::NonZeroExit(output))
                if is_missing_git_object_diagnostic(&output.stderr) =>
            {
                return Err(ArtifactError::Invalid("tree object missing".into()));
            }
            Err(error) => return Err(ArtifactError::Git(process_error(error))),
        };
        if output.stdout_truncated || output.stderr_truncated {
            return Err(ArtifactError::Git("tree type output truncated".into()));
        }
        if String::from_utf8_lossy(&output.stdout).trim() != "tree" {
            return Err(ArtifactError::Invalid("tree object missing".into()));
        }
        Ok(())
    }

    fn ref_target(&self, record: &ArtifactRecord) -> Result<Option<String>, ArtifactError> {
        let output = self.run_git(
            &record.repository_root,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)",
                &record.ref_name,
            ],
            &[],
        )?;
        if output.output_truncated {
            return Err(ArtifactError::Git("ref output truncated".into()));
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some((name, target)) = line.split_once('\0') {
                if name == record.ref_name {
                    return Ok(Some(target.to_owned()));
                }
            }
        }
        Ok(None)
    }

    fn install_ref(&self, record: &ArtifactRecord) -> Result<(), ArtifactError> {
        self.ensure_tree(record)?;
        let zero = "0".repeat(record.tree_oid.len());
        self.run_git(
            &record.repository_root,
            &["update-ref", &record.ref_name, &record.tree_oid, &zero],
            &[],
        )?;
        Ok(())
    }

    fn run_git(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, String)],
    ) -> Result<crate::ProcessOutput, ArtifactError> {
        let mut request = ProcessRequest::new(self.git_executable.clone())
            .args(args.iter().copied())
            .cwd(cwd);
        for (key, value) in env {
            request = request.env(*key, value.clone());
        }
        self.runner
            .run(request)
            .map_err(|e| ArtifactError::Git(process_error(e)))
    }
}

struct TempIndex(PathBuf);
impl TempIndex {
    fn create() -> Result<Self, ArtifactError> {
        for _ in 0..10 {
            let path = std::env::temp_dir()
                .join(format!("ai-dev-orchestrator-artifact-index-{}", new_id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(ArtifactError::Io(e.to_string())),
            }
        }
        Err(ArtifactError::Io(
            "could not allocate temporary index".into(),
        ))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn new_id() -> String {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("artifact-{time:x}-{:x}-{seq:x}", std::process::id())
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn is_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn is_missing_git_object_diagnostic(stderr: &[u8]) -> bool {
    let diagnostic = String::from_utf8_lossy(stderr);
    diagnostic.contains("Not a valid object name")
        || diagnostic.contains("could not get object info")
}
fn process_error(error: ProcessError) -> String {
    match error {
        ProcessError::NonZeroExit(output) => format!("git exited with status {:?}", output.status),
        _ => "git command could not be completed".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ArtifactError, ArtifactManager, ArtifactRecord, ArtifactState, REF_PREFIX,
        is_missing_git_object_diagnostic, new_id,
    };
    use crate::{
        AgentProvider, AttemptId, AttemptRunRequest, BaseInput, ModelChoice, OperationService,
        ProviderError, ProviderRef, ProviderRegistry, ProviderRequest, ProviderResult,
        SqliteExecutionLedger, Task, TaskId, TaskRole, WorkspaceManager,
    };
    use std::{
        fs,
        path::PathBuf,
        process::{Command, Stdio},
    };

    const CRASH_LEDGER: &str = "AI_DEV_ORCHESTRATOR_ARTIFACT_CRASH_LEDGER";
    const CRASH_REPOSITORY: &str = "AI_DEV_ORCHESTRATOR_ARTIFACT_CRASH_REPOSITORY";
    const CRASH_TASK: &str = "AI_DEV_ORCHESTRATOR_ARTIFACT_CRASH_TASK";
    const CRASH_ATTEMPT: &str = "AI_DEV_ORCHESTRATOR_ARTIFACT_CRASH_ATTEMPT";
    const CRASH_BASE: &str = "AI_DEV_ORCHESTRATOR_ARTIFACT_CRASH_BASE";
    const CRASH_TREE: &str = "AI_DEV_ORCHESTRATOR_ARTIFACT_CRASH_TREE";

    struct NoopProvider(ProviderRef);

    impl AgentProvider for NoopProvider {
        fn provider_ref(&self) -> &ProviderRef {
            &self.0
        }
        fn execute(&self, _request: &ProviderRequest) -> Result<ProviderResult, ProviderError> {
            unreachable!("crash recovery test does not execute a provider")
        }
        fn check_availability(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    fn git(path: &std::path::Path, args: &[&str]) -> String {
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
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    #[test]
    fn pending_ref_crash_child_exits_after_persist() {
        let Some(ledger_path) = std::env::var_os(CRASH_LEDGER) else {
            return;
        };
        let repository = PathBuf::from(std::env::var_os(CRASH_REPOSITORY).unwrap());
        let ledger = SqliteExecutionLedger::open(ledger_path).unwrap();
        let workspaces = WorkspaceManager::new(&repository).unwrap();
        let task = TaskId::new(std::env::var(CRASH_TASK).unwrap());
        let attempt = AttemptId::new(std::env::var(CRASH_ATTEMPT).unwrap());
        let id = "crash-window-artifact".to_owned();
        let record = ArtifactRecord {
            id: id.clone(),
            task_id: task.clone(),
            source_attempt_id: Some(attempt.as_str().to_owned()),
            input_artifact_id: None,
            base_commit: std::env::var(CRASH_BASE).unwrap(),
            tree_oid: std::env::var(CRASH_TREE).unwrap(),
            repository_root: fs::canonicalize(repository).unwrap(),
            ref_name: format!("{REF_PREFIX}{id}"),
            state: ArtifactState::PendingRef,
        };
        ArtifactManager::new(&workspaces, &ledger)
            .persist_pending(&task, &attempt, None, &record)
            .unwrap();
        std::process::exit(77);
    }

    #[test]
    fn restart_recovers_artifact_persisted_before_ref_installation() {
        let root = std::env::temp_dir().join(format!("artifact-crash-window-{}", new_id()));
        let repository = root.join("repo");
        fs::create_dir_all(&repository).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repository)
                .status()
                .unwrap()
                .success()
        );
        git(
            &repository,
            &["config", "user.email", "artifact-test@example.invalid"],
        );
        git(&repository, &["config", "user.name", "Artifact Test"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
        git(&repository, &["add", "tracked.txt"]);
        git(&repository, &["commit", "-m", "artifact base"]);
        let repository = fs::canonicalize(repository).unwrap();
        let base = git(&repository, &["rev-parse", "HEAD"]);
        let tree = git(&repository, &["rev-parse", "HEAD^{tree}"]);
        let ledger_path = root.join("execution.sqlite3");
        let ledger = SqliteExecutionLedger::open(&ledger_path).unwrap();
        let task = TaskId::new("artifact-crash-task");
        ledger
            .save_task(&Task::new(
                task.clone(),
                "crash recovery",
                TaskRole::new("implementer"),
            ))
            .unwrap();
        let workspaces = WorkspaceManager::new(&repository).unwrap();
        let mut providers = ProviderRegistry::new();
        providers.register(NoopProvider(ProviderRef::new("crash-test")));
        let service = OperationService::new(
            &ledger,
            &workspaces,
            &providers,
            1,
            std::time::Duration::from_secs(30),
        )
        .unwrap();
        let accepted = service
            .submit_attempt(&AttemptRunRequest::new(
                "crash-window-request",
                task.clone(),
                0,
                ProviderRef::new("crash-test"),
                ModelChoice::ProviderDefault,
                "produce an artifact",
                TaskRole::new("implementer"),
                BaseInput::new(&repository, base.clone()),
            ))
            .unwrap();
        let connection = ledger.lock_connection().unwrap();
        connection
            .execute(
                "UPDATE service_operations SET status='running' WHERE id=?1",
                rusqlite::params![accepted.operation_id().as_str()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE attempts SET state='running',started_at=1 WHERE task_id=?1 AND id=?2",
                rusqlite::params![task.as_str(), accepted.attempt_id().as_str()],
            )
            .unwrap();
        drop(connection);
        let attempt_id = accepted.attempt_id().as_str().to_owned();
        let operation_id = accepted.operation_id().clone();
        drop(service);
        drop(ledger);

        let executable = std::env::current_exe().unwrap();
        let status = Command::new(executable)
            .args([
                "--exact",
                "artifact::tests::pending_ref_crash_child_exits_after_persist",
            ])
            .env(CRASH_LEDGER, &ledger_path)
            .env(CRASH_REPOSITORY, &repository)
            .env(CRASH_TASK, task.as_str())
            .env(CRASH_ATTEMPT, &attempt_id)
            .env(CRASH_BASE, &base)
            .env(CRASH_TREE, &tree)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(77));

        let ledger = SqliteExecutionLedger::open(&ledger_path).unwrap();
        let workspaces = WorkspaceManager::new(&repository).unwrap();
        let mut providers = ProviderRegistry::new();
        providers.register(NoopProvider(ProviderRef::new("crash-test")));
        let service = OperationService::new(
            &ledger,
            &workspaces,
            &providers,
            1,
            std::time::Duration::from_secs(30),
        )
        .unwrap();
        let snapshot = service.get_operation(&operation_id).unwrap();
        assert_eq!(
            snapshot.status(),
            crate::ServiceOperationStatus::RecoveryRequired
        );
        assert_eq!(snapshot.output_artifact_id(), Some("crash-window-artifact"));
        let manager = ArtifactManager::new(&workspaces, &ledger);
        let artifact = manager
            .verify_input(&task, "crash-window-artifact")
            .unwrap();
        assert_eq!(artifact.state(), ArtifactState::Available);
        assert_eq!(git(&repository, &["rev-parse", &artifact.ref_name]), tree);
        drop(service);
        drop(manager);
        drop(ledger);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn only_explicit_missing_object_diagnostics_confirm_absence() {
        assert!(is_missing_git_object_diagnostic(
            b"fatal: git cat-file: could not get object info"
        ));
        assert!(is_missing_git_object_diagnostic(
            b"fatal: Not a valid object name deadbeef"
        ));
        assert!(!is_missing_git_object_diagnostic(
            b"fatal: cannot open .git/objects: I/O error"
        ));
        assert!(!is_missing_git_object_diagnostic(b""));
    }

    #[cfg(unix)]
    #[test]
    fn transient_git_failure_does_not_poison_available_artifact() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("artifact-read-{}", new_id()));
        fs::create_dir_all(&root).unwrap();
        let repository = root.join("repo");
        fs::create_dir_all(&repository).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repository)
                .status()
                .unwrap()
                .success()
        );
        let repository = fs::canonicalize(repository).unwrap();
        let failing_git = root.join("git-failure");
        fs::write(
            &failing_git,
            "#!/bin/sh\necho 'fatal: temporary object store I/O failure' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&failing_git, fs::Permissions::from_mode(0o755)).unwrap();

        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = TaskId::new("transient-artifact-task");
        ledger
            .save_task(&Task::new(
                task.clone(),
                "artifact recovery test",
                TaskRole::new("implementer"),
            ))
            .unwrap();
        let id = "artifact-transient";
        let ref_name = format!("{REF_PREFIX}{id}");
        ledger
            .lock_connection()
            .unwrap()
            .execute(
                "INSERT INTO service_artifacts(id,task_id,base_commit,tree_oid,repository_root,ref_name,state,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,'available',0)",
                rusqlite::params![
                    id,
                    task.as_str(),
                    "0".repeat(40),
                    "1".repeat(40),
                    repository.to_string_lossy().as_ref(),
                    ref_name
                ],
            )
            .unwrap();
        let workspaces = WorkspaceManager::new(&repository).unwrap();
        let mut artifacts = ArtifactManager::new(&workspaces, &ledger);
        artifacts.git_executable = failing_git.into_os_string();

        let result = artifacts.verify_input(&task, id);
        assert!(matches!(result, Err(ArtifactError::Git(_))), "{result:?}");
        let state: String = ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT state FROM service_artifacts WHERE task_id=?1 AND id=?2",
                rusqlite::params![task.as_str(), id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "available");
        let _ = fs::remove_dir_all(root);
    }
}
