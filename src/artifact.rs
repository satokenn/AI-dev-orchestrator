//! Immutable, Git-native snapshots of managed workspaces.

use std::{
    fmt, fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    ArtifactRecord, ArtifactState, AttemptId, OperationLedgerError, ProcessError, ProcessRequest,
    ProcessRunner, SqliteOperationLedger, TaskId, Workspace, WorkspaceError, WorkspaceManager,
};

const ARTIFACT_REF_PREFIX: &str = "refs/ai-dev-orchestrator/artifacts/";
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
struct ArtifactId(String);

impl ArtifactId {
    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub enum ArtifactError {
    Ledger(String),
    Workspace(String),
    Git(String),
    Io(String),
    NotFound(String),
    MissingInitialBase,
    RepositoryMismatch,
    InvalidBaseCommit(String),
    InvalidTreeObject(String),
    RecoveryRequired(String),
    WorkspaceChanged { expected: String, actual: String },
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ledger(message) => write!(formatter, "artifact ledger error: {message}"),
            Self::Workspace(message) => write!(formatter, "artifact workspace error: {message}"),
            Self::Git(message) => write!(formatter, "artifact Git error: {message}"),
            Self::Io(message) => write!(formatter, "artifact I/O error: {message}"),
            Self::NotFound(id) => write!(formatter, "artifact was not found: {id}"),
            Self::MissingInitialBase => {
                formatter.write_str("initial artifact requires a base commit")
            }
            Self::RepositoryMismatch => {
                formatter.write_str("artifact belongs to another repository")
            }
            Self::InvalidBaseCommit(value) => write!(formatter, "invalid base commit: {value}"),
            Self::InvalidTreeObject(value) => write!(formatter, "invalid Git tree object: {value}"),
            Self::RecoveryRequired(id) => write!(formatter, "artifact requires recovery: {id}"),
            Self::WorkspaceChanged { expected, actual } => write!(
                formatter,
                "workspace tree changed: expected {expected}, found {actual}"
            ),
        }
    }
}

impl std::error::Error for ArtifactError {}

impl From<OperationLedgerError> for ArtifactError {
    fn from(value: OperationLedgerError) -> Self {
        Self::Ledger(value.to_string())
    }
}

impl From<WorkspaceError> for ArtifactError {
    fn from(value: WorkspaceError) -> Self {
        Self::Workspace(value.to_string())
    }
}

/// Captures and reads immutable Git tree snapshots for manager-owned workspaces.
///
/// Artifact refs are retained for the lifetime of their ledger records. This
/// component does not perform Task/Attempt Service validation or publication
/// evidence gating; those checks belong to the Operation Service integration.
pub struct ArtifactManager<'a> {
    workspaces: &'a WorkspaceManager,
    ledger: &'a SqliteOperationLedger,
    runner: ProcessRunner,
}

impl<'a> ArtifactManager<'a> {
    pub fn new(workspaces: &'a WorkspaceManager, ledger: &'a SqliteOperationLedger) -> Self {
        Self {
            workspaces,
            ledger,
            runner: ProcessRunner,
        }
    }

    /// Captures the workspace tree without changing its worktree or index.
    ///
    /// An initial artifact supplies `initial_base_commit`. A child artifact
    /// supplies `input_artifact_id` and inherits that artifact's base commit.
    /// `source_attempt_id` may be absent for supervisor-owned edits.
    pub fn capture(
        &self,
        workspace: &Workspace,
        task_id: &TaskId,
        source_attempt_id: Option<&AttemptId>,
        input_artifact_id: Option<&str>,
        initial_base_commit: Option<&str>,
    ) -> Result<ArtifactRecord, ArtifactError> {
        self.workspaces
            .validate_artifact_workspace(workspace, task_id, source_attempt_id)?;
        let base_commit = if let Some(input_id) = input_artifact_id {
            let parent = self.read(task_id, input_id)?;
            if parent.repository_root() != self.workspaces.repository_root() {
                return Err(ArtifactError::RepositoryMismatch);
            }
            if let Some(supplied_base) = initial_base_commit
                && supplied_base != parent.base_commit()
            {
                return Err(ArtifactError::InvalidBaseCommit(supplied_base.to_owned()));
            }
            parent.base_commit().to_owned()
        } else {
            initial_base_commit
                .ok_or(ArtifactError::MissingInitialBase)?
                .to_owned()
        };
        if self.workspaces.repository_root().to_str().is_none() {
            return Err(ArtifactError::Io(
                "repository path is not valid UTF-8 and cannot be persisted".into(),
            ));
        }
        let base_commit = self.normalize_base_commit(workspace.path(), &base_commit)?;

        let tree_oid = self.snapshot_tree(workspace.path())?;
        let id = new_artifact_id();
        let ref_name = format!("{ARTIFACT_REF_PREFIX}{}", id.as_str());
        let record = ArtifactRecord::new(
            id.as_str(),
            task_id.clone(),
            source_attempt_id.map(|attempt| attempt.as_str().to_owned()),
            input_artifact_id.map(str::to_owned),
            base_commit,
            tree_oid.clone(),
            self.workspaces.repository_root(),
            ref_name,
            ArtifactState::PendingRef,
        );
        self.ledger.prepare_artifact(&record)?;

        // Keep the pending record on failure. A later read can safely finish
        // the ref installation or report that recovery is required.
        self.create_artifact_ref(&record)?;
        self.ledger
            .set_artifact_state(task_id, record.id(), ArtifactState::Available)?;
        self.read(task_id, record.id())
    }

    /// Reads an artifact owned by `task_id`, reconciling an interrupted ref write.
    pub fn read(
        &self,
        task_id: &TaskId,
        artifact_id: &str,
    ) -> Result<ArtifactRecord, ArtifactError> {
        let mut record = self
            .ledger
            .get_artifact(task_id, artifact_id)?
            .ok_or_else(|| ArtifactError::NotFound(artifact_id.to_owned()))?;
        if record.repository_root() != self.workspaces.repository_root() {
            return Err(ArtifactError::RepositoryMismatch);
        }
        if record.state() == ArtifactState::RecoveryRequired {
            return Err(ArtifactError::RecoveryRequired(record.id().to_owned()));
        }

        self.ensure_tree_object(&record).inspect_err(|_error| {
            let _ = self.ledger.set_artifact_state(
                task_id,
                artifact_id,
                ArtifactState::RecoveryRequired,
            );
        })?;
        let ref_target = self.ref_target(&record)?;
        match ref_target {
            Some(target) if target == record.tree_oid() => {}
            Some(_) => {
                self.ledger.set_artifact_state(
                    task_id,
                    artifact_id,
                    ArtifactState::RecoveryRequired,
                )?;
                return Err(ArtifactError::RecoveryRequired(record.id().to_owned()));
            }
            None => {
                self.ensure_tree_object(&record)?;
                self.create_artifact_ref(&record)?;
                if self.ref_target(&record)?.as_deref() != Some(record.tree_oid()) {
                    self.ledger.set_artifact_state(
                        task_id,
                        artifact_id,
                        ArtifactState::RecoveryRequired,
                    )?;
                    return Err(ArtifactError::RecoveryRequired(record.id().to_owned()));
                }
            }
        }
        if record.state() == ArtifactState::PendingRef {
            self.ledger
                .set_artifact_state(task_id, artifact_id, ArtifactState::Available)?;
            record = self
                .ledger
                .get_artifact(task_id, artifact_id)?
                .ok_or_else(|| ArtifactError::NotFound(artifact_id.to_owned()))?;
        }
        Ok(record)
    }

    /// Recomputes the live workspace tree and compares it with a stored artifact.
    pub fn verify_workspace(
        &self,
        workspace: &Workspace,
        task_id: &TaskId,
        artifact_id: &str,
    ) -> Result<(), ArtifactError> {
        let record = self.read(task_id, artifact_id)?;
        let attempt_id = record
            .source_attempt_id()
            .map(|value| AttemptId::new(value.to_owned()));
        self.workspaces
            .validate_artifact_workspace(workspace, task_id, attempt_id.as_ref())?;
        let actual = self.snapshot_tree(workspace.path())?;
        if actual != record.tree_oid() {
            return Err(ArtifactError::WorkspaceChanged {
                expected: record.tree_oid().to_owned(),
                actual,
            });
        }
        Ok(())
    }

    fn snapshot_tree(&self, workspace: &Path) -> Result<String, ArtifactError> {
        let index_dir = TempIndex::create()?;
        let index_path = index_dir.path().join("index");
        let env = [("GIT_INDEX_FILE", index_path.to_string_lossy().into_owned())];
        self.run_git(workspace, &["read-tree", "HEAD"], &env)?;
        // `git add -A` sees tracked changes and non-ignored untracked files;
        // it deliberately leaves ignored files outside the artifact.
        self.run_git(workspace, &["add", "-A", "--", "."], &env)?;
        let output = self.run_git(workspace, &["write-tree"], &env)?;
        let tree = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !is_object_id(&tree) {
            return Err(ArtifactError::InvalidTreeObject(tree));
        }
        Ok(tree)
    }

    fn normalize_base_commit(&self, workspace: &Path, base: &str) -> Result<String, ArtifactError> {
        if !is_object_id(base) {
            return Err(ArtifactError::InvalidBaseCommit(base.to_owned()));
        }
        let output = self
            .run_git(
                workspace,
                &[
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &format!("{base}^{{commit}}"),
                ],
                &[],
            )
            .map_err(|_| ArtifactError::InvalidBaseCommit(base.to_owned()))?;
        let normalized = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !is_object_id(&normalized) {
            return Err(ArtifactError::InvalidBaseCommit(base.to_owned()));
        }
        Ok(normalized)
    }

    fn ensure_tree_object(&self, record: &ArtifactRecord) -> Result<(), ArtifactError> {
        let output = self.run_git(
            record.repository_root(),
            &["cat-file", "-t", record.tree_oid()],
            &[],
        )?;
        if String::from_utf8_lossy(&output.stdout).trim() != "tree" {
            return Err(ArtifactError::InvalidTreeObject(
                record.tree_oid().to_owned(),
            ));
        }
        Ok(())
    }

    fn ref_target(&self, record: &ArtifactRecord) -> Result<Option<String>, ArtifactError> {
        let output = self.runner.run(
            ProcessRequest::new("git")
                .args(["rev-parse", "--verify", "--quiet", record.ref_name()])
                .cwd(record.repository_root()),
        );
        match output {
            Ok(output) => Ok(Some(
                String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            )),
            Err(ProcessError::NonZeroExit(_)) => Ok(None),
            Err(error) => Err(ArtifactError::Git(process_error(error))),
        }
    }

    fn create_artifact_ref(&self, record: &ArtifactRecord) -> Result<(), ArtifactError> {
        self.ensure_tree_object(record)?;
        let zero_oid = "0".repeat(record.tree_oid().len());
        self.run_git(
            record.repository_root(),
            &[
                "update-ref",
                record.ref_name(),
                record.tree_oid(),
                &zero_oid,
            ],
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
        for (name, value) in env {
            request = request.env(*name, value.clone());
        }
        self.runner
            .run(request)
            .map_err(|error| ArtifactError::Git(process_error(error)))
    }
}

#[derive(Debug)]
struct TempIndex(PathBuf);

impl TempIndex {
    fn create() -> Result<Self, ArtifactError> {
        for _ in 0..10 {
            let id = new_artifact_id();
            let path =
                std::env::temp_dir().join(format!("ai-dev-orchestrator-index-{}", id.as_str()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(ArtifactError::Io(error.to_string())),
            }
        }
        Err(ArtifactError::Io(
            "could not allocate a temporary Git index".into(),
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

fn new_artifact_id() -> ArtifactId {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    ArtifactId(format!(
        "artifact-{nanos:x}-{:x}-{sequence:x}",
        std::process::id()
    ))
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn process_error(error: ProcessError) -> String {
    match error {
        ProcessError::NonZeroExit(output) => format!(
            "git exited with {:?}: {}",
            output.exit_code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        other => format!("git process failed: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn temporary_directory() -> PathBuf {
        let id = new_artifact_id();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("artifact-test-{}-{sequence}", id.as_str()));
        fs::create_dir(&path).expect("create temporary directory");
        path
    }

    fn git(directory: &Path, args: &[&str]) -> String {
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
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn git_status(directory: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()
            .expect("git is installed")
            .status
            .success()
    }

    fn repository() -> PathBuf {
        let path = temporary_directory();
        git(&path, &["init", "-b", "main"]);
        git(&path, &["config", "user.email", "artifact@example.invalid"]);
        git(&path, &["config", "user.name", "Artifact Test"]);
        fs::write(path.join(".gitignore"), "*.secret\n").expect("write ignore file");
        fs::write(path.join("source.txt"), "base\n").expect("write tracked source");
        git(&path, &["add", ".gitignore", "source.txt"]);
        git(&path, &["commit", "-m", "initial"]);
        path
    }

    #[test]
    fn snapshots_worktree_and_non_ignored_files_without_changing_index() {
        let repository = repository();
        let workspace_manager = WorkspaceManager::new(&repository).unwrap();
        let task_id = TaskId::new("task-a");
        let attempt_id = AttemptId::new("attempt-a");
        let workspace = workspace_manager.create(&task_id, &attempt_id).unwrap();
        fs::write(workspace.path().join("source.txt"), "staged version\n").unwrap();
        git(workspace.path(), &["add", "source.txt"]);
        fs::write(workspace.path().join("source.txt"), "worktree version\n").unwrap();
        fs::write(workspace.path().join("new.txt"), "new content\n").unwrap();
        fs::write(workspace.path().join("ignored.secret"), "ignored content\n").unwrap();
        let staged_diff_before = git(workspace.path(), &["diff", "--cached", "--", "source.txt"]);
        let status_before = git(workspace.path(), &["status", "--porcelain=v1", "-uall"]);

        let base = git(workspace.path(), &["rev-parse", "HEAD"]);
        let ledger_path = repository.with_extension("artifact-ledger");
        let ledger_db_path = PathBuf::from(format!("{}.operations.sqlite3", ledger_path.display()));
        let (artifact_id, artifact_tree) = {
            let ledger = SqliteOperationLedger::open(&ledger_path).unwrap();
            let artifacts = ArtifactManager::new(&workspace_manager, &ledger);
            let wrong_attempt = AttemptId::new("different-attempt");
            assert!(matches!(
                artifacts.capture(
                    &workspace,
                    &task_id,
                    Some(&wrong_attempt),
                    None,
                    Some(&base),
                ),
                Err(ArtifactError::Workspace(_))
            ));
            let artifact = artifacts
                .capture(&workspace, &task_id, Some(&attempt_id), None, Some(&base))
                .unwrap();

            assert_eq!(artifact.base_commit(), base);
            assert_eq!(artifact.source_attempt_id(), Some(attempt_id.as_str()));
            assert_eq!(artifact.state(), ArtifactState::Available);
            assert_eq!(
                git(
                    workspace.path(),
                    &["show", &format!("{}:source.txt", artifact.tree_oid())]
                ),
                "worktree version"
            );
            assert_eq!(
                git(
                    workspace.path(),
                    &["show", &format!("{}:new.txt", artifact.tree_oid())]
                ),
                "new content"
            );
            assert!(!git_status(
                workspace.path(),
                &[
                    "cat-file",
                    "-e",
                    &format!("{}:ignored.secret", artifact.tree_oid())
                ]
            ));
            assert_eq!(
                git(workspace.path(), &["rev-parse", artifact.ref_name()]),
                artifact.tree_oid()
            );
            assert_eq!(
                git(workspace.path(), &["diff", "--cached", "--", "source.txt"]),
                staged_diff_before
            );
            assert_eq!(
                git(workspace.path(), &["status", "--porcelain=v1", "-uall"]),
                status_before
            );
            artifacts
                .verify_workspace(&workspace, &task_id, artifact.id())
                .unwrap();

            fs::write(
                workspace.path().join("source.txt"),
                "changed after snapshot\n",
            )
            .unwrap();
            let child = artifacts
                .capture(
                    &workspace,
                    &task_id,
                    Some(&attempt_id),
                    Some(artifact.id()),
                    None,
                )
                .unwrap();
            assert_eq!(child.input_artifact_id(), Some(artifact.id()));
            assert_eq!(child.base_commit(), artifact.base_commit());
            assert_ne!(child.tree_oid(), artifact.tree_oid());
            assert!(matches!(
                artifacts.verify_workspace(&workspace, &task_id, artifact.id()),
                Err(ArtifactError::WorkspaceChanged { .. })
            ));
            let restored = artifacts.read(&task_id, artifact.id()).unwrap();
            assert_eq!(restored.tree_oid(), artifact.tree_oid());
            assert!(matches!(
                artifacts.read(&TaskId::new("another-task"), artifact.id()),
                Err(ArtifactError::NotFound(_))
            ));
            (artifact.id().to_owned(), artifact.tree_oid().to_owned())
        };

        workspace_manager.cleanup_force(&workspace).unwrap();
        {
            let reopened_ledger = SqliteOperationLedger::open(&ledger_path).unwrap();
            let reopened_artifacts = ArtifactManager::new(&workspace_manager, &reopened_ledger);
            assert_eq!(
                reopened_artifacts
                    .read(&task_id, &artifact_id)
                    .unwrap()
                    .tree_oid(),
                artifact_tree
            );
        }
        fs::remove_file(ledger_db_path).unwrap();
        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn readback_recovers_database_ref_intermediate_states() {
        let repository = repository();
        let workspace_manager = WorkspaceManager::new(&repository).unwrap();
        let task_id = TaskId::new("task-recovery");
        let attempt_id = AttemptId::new("attempt-recovery");
        let workspace = workspace_manager.create(&task_id, &attempt_id).unwrap();
        fs::write(workspace.path().join("new.txt"), "snapshot\n").unwrap();
        // The pending rows below simulate a crash at each side of the ref/DB
        // boundary.
        let ledger = SqliteOperationLedger::open_in_memory().unwrap();
        let artifacts = ArtifactManager::new(&workspace_manager, &ledger);
        let base = git(workspace.path(), &["rev-parse", "HEAD"]);
        let tree = artifacts.snapshot_tree(workspace.path()).unwrap();
        let missing_ref = format!("{ARTIFACT_REF_PREFIX}test-pending-without-ref");
        let pending = ArtifactRecord::new(
            "test-pending-without-ref",
            task_id.clone(),
            Some(attempt_id.as_str().to_owned()),
            None,
            &base,
            &tree,
            workspace_manager.repository_root(),
            missing_ref,
            ArtifactState::PendingRef,
        );
        ledger.prepare_artifact(&pending).unwrap();
        let recovered = artifacts.read(&task_id, pending.id()).unwrap();
        assert_eq!(recovered.state(), ArtifactState::Available);
        assert_eq!(recovered.tree_oid(), tree);

        // This state simulates the Git ref commit succeeding before the
        // SQLite pending_ref -> available transition.
        let existing_ref = format!("{ARTIFACT_REF_PREFIX}test-pending-with-ref");
        let interrupted = ArtifactRecord::new(
            "test-pending-with-ref",
            task_id.clone(),
            Some(attempt_id.as_str().to_owned()),
            Some(pending.id().to_owned()),
            &base,
            &tree,
            workspace_manager.repository_root(),
            existing_ref.clone(),
            ArtifactState::PendingRef,
        );
        ledger.prepare_artifact(&interrupted).unwrap();
        git(
            workspace.path(),
            &["update-ref", &existing_ref, &tree, &"0".repeat(tree.len())],
        );
        let recovered = artifacts.read(&task_id, interrupted.id()).unwrap();
        assert_eq!(recovered.state(), ArtifactState::Available);

        workspace_manager.cleanup_force(&workspace).unwrap();
        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn mismatched_existing_artifact_ref_fails_closed() {
        let repository = repository();
        let workspace_manager = WorkspaceManager::new(&repository).unwrap();
        let task_id = TaskId::new("task-ref-mismatch");
        let attempt_id = AttemptId::new("attempt-ref-mismatch");
        let workspace = workspace_manager.create(&task_id, &attempt_id).unwrap();
        let ledger = SqliteOperationLedger::open_in_memory().unwrap();
        let artifacts = ArtifactManager::new(&workspace_manager, &ledger);
        let base = git(workspace.path(), &["rev-parse", "HEAD"]);
        let expected_tree = artifacts.snapshot_tree(workspace.path()).unwrap();
        fs::write(workspace.path().join("changed.txt"), "another tree\n").unwrap();
        let other_tree = artifacts.snapshot_tree(workspace.path()).unwrap();
        let reference = format!("{ARTIFACT_REF_PREFIX}test-ref-mismatch");
        let pending = ArtifactRecord::new(
            "test-ref-mismatch",
            task_id.clone(),
            Some(attempt_id.as_str().to_owned()),
            None,
            &base,
            &expected_tree,
            workspace_manager.repository_root(),
            reference.clone(),
            ArtifactState::PendingRef,
        );
        ledger.prepare_artifact(&pending).unwrap();
        git(
            workspace.path(),
            &[
                "update-ref",
                &reference,
                &other_tree,
                &"0".repeat(other_tree.len()),
            ],
        );
        assert!(matches!(
            artifacts.read(&task_id, pending.id()),
            Err(ArtifactError::RecoveryRequired(_))
        ));
        assert_eq!(
            git(workspace.path(), &["rev-parse", &reference]),
            other_tree
        );

        workspace_manager.cleanup_force(&workspace).unwrap();
        fs::remove_dir_all(repository).unwrap();
    }
}
