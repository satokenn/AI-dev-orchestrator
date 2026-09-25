//! Git-tree Artifacts persisted in the same SQLite file as Tasks and Attempts.

use std::{
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
}

impl<'a> ArtifactManager<'a> {
    pub(crate) fn new(workspaces: &'a WorkspaceManager, ledger: &'a SqliteExecutionLedger) -> Self {
        Self {
            workspaces,
            ledger,
            runner: ProcessRunner,
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
        if self.ensure_tree(&record).is_err() {
            self.set_state(task, id, ArtifactState::RecoveryRequired)?;
            return Err(ArtifactError::RecoveryRequired);
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
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(parent) = input_id {
            let available: Option<String> = tx.query_row("SELECT id FROM service_artifacts WHERE task_id=?1 AND id=?2 AND state='available'", params![task.as_str(), parent], |row| row.get(0)).optional()?;
            if available.is_none() {
                return Err(ArtifactError::NotFound);
            }
        }
        tx.execute("INSERT INTO service_artifacts(id,task_id,source_attempt_id,input_artifact_id,base_commit,tree_oid,repository_root,ref_name,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending_ref',?9)", params![record.id, task.as_str(), attempt.as_str(), input_id, record.base_commit, record.tree_oid, root.to_string_lossy().as_ref(), record.ref_name, now_ms()])?;
        tx.execute("UPDATE service_attempt_artifacts SET output_artifact_id=?3 WHERE task_id=?1 AND attempt_id=?2", params![task.as_str(), attempt.as_str(), record.id])?;
        tx.commit()?;
        drop(connection);
        self.install_ref(&record)?;
        self.set_state(task, &record.id, ArtifactState::Available)?;
        let mut available = record;
        available.state = ArtifactState::Available;
        Ok(available)
    }

    pub(crate) fn verify_input(
        &self,
        task: &TaskId,
        id: &str,
    ) -> Result<ArtifactRecord, ArtifactError> {
        self.read(task, id)
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
        let output = self
            .runner
            .run(
                ProcessRequest::new("git")
                    .args(["cat-file", "-t", &record.tree_oid])
                    .cwd(&record.repository_root),
            )
            .map_err(|e| ArtifactError::Git(process_error(e)))?;
        if output.output_truncated || String::from_utf8_lossy(&output.stdout).trim() != "tree" {
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
        let mut request = ProcessRequest::new("git")
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
fn process_error(error: ProcessError) -> String {
    match error {
        ProcessError::NonZeroExit(output) => format!("git exited with status {:?}", output.status),
        _ => "git command could not be completed".to_owned(),
    }
}
