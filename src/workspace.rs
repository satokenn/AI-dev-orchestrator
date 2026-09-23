//! Isolated Git workspaces for agent attempts.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    Attempt, AttemptId, ProcessError, ProcessRequest, ProcessRunner, ProviderRequest, Task, TaskId,
};

const WORKTREE_DIRECTORY: &str = ".ai-dev-orchestrator/worktrees";

/// A diagnostic for a failed Git command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitError {
    operation: String,
    command: String,
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

impl GitError {
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    #[must_use]
    pub const fn status(&self) -> Option<i32> {
        self.status
    }

    #[must_use]
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    #[must_use]
    pub fn stderr(&self) -> &str {
        &self.stderr
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "git {} failed (status {:?}): {}",
            self.operation,
            self.status,
            self.stderr.trim()
        )
    }
}

impl std::error::Error for GitError {}

/// Failure while resolving or managing an isolated workspace.
#[derive(Debug)]
pub enum WorkspaceError {
    InvalidRepositoryPath {
        path: PathBuf,
        reason: String,
    },
    Io {
        operation: String,
        path: PathBuf,
        source: std::io::Error,
    },
    Git(GitError),
    BranchCollision {
        branch: String,
    },
    PathCollision {
        path: PathBuf,
    },
    MainWorktreeNotAllowed {
        path: PathBuf,
    },
    WorkspaceNotManaged {
        path: PathBuf,
    },
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRepositoryPath { path, reason } => {
                write!(
                    formatter,
                    "invalid repository path '{}': {reason}",
                    path.display()
                )
            }
            Self::Io {
                operation,
                path,
                source,
            } => {
                write!(
                    formatter,
                    "{operation} '{}' failed: {source}",
                    path.display()
                )
            }
            Self::Git(error) => error.fmt(formatter),
            Self::BranchCollision { branch } => {
                write!(formatter, "workspace branch already exists: {branch}")
            }
            Self::PathCollision { path } => {
                write!(
                    formatter,
                    "workspace path already exists: {}",
                    path.display()
                )
            }
            Self::MainWorktreeNotAllowed { path } => {
                write!(
                    formatter,
                    "the repository working tree cannot be an agent workspace: {}",
                    path.display()
                )
            }
            Self::WorkspaceNotManaged { path } => {
                write!(
                    formatter,
                    "path is not a managed Git worktree: {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for WorkspaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Git(error) => Some(error),
            _ => None,
        }
    }
}

/// A Git worktree owned by a [`WorkspaceManager`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    repository_root: PathBuf,
    branch: String,
    path: PathBuf,
}

impl Workspace {
    #[must_use]
    pub fn repository_root(&self) -> &Path {
        &self.repository_root
    }

    #[must_use]
    pub fn branch(&self) -> &str {
        &self.branch
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Creates and removes one isolated Git worktree per task attempt.
#[derive(Clone, Debug)]
pub struct WorkspaceManager {
    repository_root: PathBuf,
    worktree_root: PathBuf,
    runner: ProcessRunner,
}

impl WorkspaceManager {
    /// Resolves `path` to the root of its Git repository.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let path = path.as_ref();
        let metadata =
            fs::metadata(path).map_err(|error| WorkspaceError::InvalidRepositoryPath {
                path: path.to_owned(),
                reason: error.to_string(),
            })?;
        if !metadata.is_dir() {
            return Err(WorkspaceError::InvalidRepositoryPath {
                path: path.to_owned(),
                reason: "path is not a directory".to_owned(),
            });
        }
        let output = ProcessRunner
            .run(ProcessRequest::new("git").args([
                "-C",
                path.to_string_lossy().as_ref(),
                "rev-parse",
                "--show-toplevel",
            ]))
            .map_err(|error| {
                git_error(
                    "resolve repository root",
                    ["git", "rev-parse", "--show-toplevel"],
                    error,
                )
            })?;
        let root_text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let repository_root = fs::canonicalize(&root_text).map_err(|error| {
            WorkspaceError::InvalidRepositoryPath {
                path: path.to_owned(),
                reason: format!("Git returned an invalid repository root: {error}"),
            }
        })?;
        let repository_name = repository_root
            .file_name()
            .and_then(|name| name.to_str())
            .map_or_else(|| "repository".to_owned(), branch_component);
        let worktree_root = repository_root
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(WORKTREE_DIRECTORY)
            .join(repository_name);
        Ok(Self {
            worktree_root,
            repository_root,
            runner: ProcessRunner,
        })
    }

    /// Resolves the repository containing the current process directory.
    pub fn from_current_dir() -> Result<Self, WorkspaceError> {
        Self::new(std::env::current_dir().map_err(|error| {
            WorkspaceError::InvalidRepositoryPath {
                path: PathBuf::from("."),
                reason: error.to_string(),
            }
        })?)
    }

    #[must_use]
    pub fn repository_root(&self) -> &Path {
        &self.repository_root
    }

    #[must_use]
    pub fn worktree_root(&self) -> &Path {
        &self.worktree_root
    }

    /// Returns the deterministic branch name for a task and attempt.
    #[must_use]
    pub fn branch_name(&self, task: &TaskId, attempt: &AttemptId) -> String {
        format!(
            "orchestrator/task/{}/attempt/{}",
            branch_component(task.as_str()),
            branch_component(attempt.as_str())
        )
    }

    /// Returns the deterministic path for a task and attempt.
    #[must_use]
    pub fn worktree_path(&self, task: &TaskId, attempt: &AttemptId) -> PathBuf {
        self.worktree_root
            .join(branch_component(task.as_str()))
            .join(branch_component(attempt.as_str()))
    }

    /// Alias naming the operation as a workspace creation explicitly.
    pub fn create_workspace(
        &self,
        task: &TaskId,
        attempt: &AttemptId,
    ) -> Result<Workspace, WorkspaceError> {
        self.create(task, attempt)
    }

    /// Convenience form accepting the domain objects directly.
    pub fn create_for_task_attempt(
        &self,
        task: &Task,
        attempt: &Attempt,
    ) -> Result<Workspace, WorkspaceError> {
        self.create(task.id(), attempt.id())
    }

    /// Creates a new branch and worktree for one attempt.
    pub fn create(&self, task: &TaskId, attempt: &AttemptId) -> Result<Workspace, WorkspaceError> {
        let branch = self.branch_name(task, attempt);
        let path = self.worktree_path(task, attempt);
        if fs::symlink_metadata(&path).is_ok() {
            return Err(WorkspaceError::PathCollision { path });
        }
        if self.branch_exists(&branch)? {
            return Err(WorkspaceError::BranchCollision { branch });
        }
        fs::create_dir_all(path.parent().expect("worktree path has a parent")).map_err(
            |source| WorkspaceError::Io {
                operation: "create worktree parent".to_owned(),
                path: path.clone(),
                source,
            },
        )?;
        let path_string = path.to_string_lossy().into_owned();
        let args = [
            "worktree",
            "add",
            "-b",
            branch.as_str(),
            path_string.as_str(),
        ];
        if let Err(error) = self.run_git("create worktree", &args) {
            return Err(classify_creation_error(error, &branch, &path));
        }
        Ok(Workspace {
            repository_root: self.repository_root.clone(),
            branch,
            path,
        })
    }

    /// Removes an owned clean worktree without discarding local changes.
    ///
    /// Git returns a diagnostic error for a dirty worktree, leaving it intact.
    /// The branch is deliberately retained so a caller can inspect or merge
    /// agent commits in a later operation.
    pub fn cleanup(&self, workspace: &Workspace) -> Result<(), WorkspaceError> {
        self.ensure_owned_workspace(workspace)?;
        self.remove_worktree(workspace, false)
    }

    /// Removes an owned worktree (alias for [`Self::cleanup`]).
    pub fn remove(&self, workspace: &Workspace) -> Result<(), WorkspaceError> {
        self.cleanup(workspace)
    }

    /// Explicitly removes an owned worktree, discarding uncommitted changes.
    pub fn cleanup_force(&self, workspace: &Workspace) -> Result<(), WorkspaceError> {
        self.ensure_owned_workspace(workspace)?;
        self.remove_worktree(workspace, true)
    }

    fn remove_worktree(&self, workspace: &Workspace, force: bool) -> Result<(), WorkspaceError> {
        let path_string = workspace.path.to_string_lossy().into_owned();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(path_string.as_str());
        self.run_git("remove worktree", &args)?;
        Ok(())
    }

    /// Checks that a Provider path is an existing, manager-created worktree.
    pub fn validate_provider_workspace(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<(), WorkspaceError> {
        let path =
            fs::canonicalize(path.as_ref()).map_err(|_| WorkspaceError::WorkspaceNotManaged {
                path: path.as_ref().to_owned(),
            })?;
        if path == self.repository_root {
            return Err(WorkspaceError::MainWorktreeNotAllowed { path });
        }
        if !path.starts_with(&self.worktree_root) {
            return Err(WorkspaceError::WorkspaceNotManaged { path });
        }
        if !self
            .list_worktrees()?
            .iter()
            .any(|candidate| candidate == &path)
        {
            return Err(WorkspaceError::WorkspaceNotManaged { path });
        }
        Ok(())
    }

    /// Alias emphasizing that this check is a precondition for Provider use.
    pub fn ensure_provider_workspace(&self, path: impl AsRef<Path>) -> Result<(), WorkspaceError> {
        self.validate_provider_workspace(path)
    }

    /// Validates the workspace carried by a Provider request.
    pub fn validate_provider_request(
        &self,
        request: &ProviderRequest,
    ) -> Result<(), WorkspaceError> {
        self.validate_provider_workspace(request.workspace())
    }

    fn ensure_owned_workspace(&self, workspace: &Workspace) -> Result<(), WorkspaceError> {
        if workspace.repository_root != self.repository_root {
            return Err(WorkspaceError::WorkspaceNotManaged {
                path: workspace.path.clone(),
            });
        }
        self.validate_provider_workspace(&workspace.path)
    }

    fn branch_exists(&self, branch: &str) -> Result<bool, WorkspaceError> {
        let request = ProcessRequest::new("git")
            .args([
                "-C",
                self.repository_root.to_string_lossy().as_ref(),
                "show-ref",
                "--verify",
                "--quiet",
            ])
            .arg(format!("refs/heads/{branch}"));
        match self.runner.run(request) {
            Ok(_) => Ok(true),
            Err(ProcessError::NonZeroExit(_)) => Ok(false),
            Err(error) => Err(git_error(
                "check branch",
                ["git", "show-ref", "--verify", "--quiet"],
                error,
            )),
        }
    }

    fn list_worktrees(&self) -> Result<Vec<PathBuf>, WorkspaceError> {
        let output = self.run_git("list worktrees", &["worktree", "list", "--porcelain"])?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .filter_map(|path| fs::canonicalize(path).ok())
            .collect())
    }

    fn run_git(
        &self,
        operation: &str,
        args: &[&str],
    ) -> Result<crate::ProcessOutput, WorkspaceError> {
        self.runner
            .run(
                ProcessRequest::new("git")
                    .args(args.iter().copied())
                    .cwd(&self.repository_root),
            )
            .map_err(|error| git_error(operation, args.iter().copied(), error))
    }
}

fn branch_component(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
            result.push(character);
        } else {
            result.push('-');
        }
    }
    if result.is_empty() {
        result.push_str("unnamed");
    }
    if result == "." || result == ".." || result.ends_with(".lock") || result.starts_with('.') {
        result.insert(0, '_');
    }
    result
}

fn classify_creation_error(error: WorkspaceError, branch: &str, path: &Path) -> WorkspaceError {
    match &error {
        WorkspaceError::Git(git) if git.stderr.contains("already exists") => {
            if git.stderr.contains("branch") {
                WorkspaceError::BranchCollision {
                    branch: branch.to_owned(),
                }
            } else {
                WorkspaceError::PathCollision {
                    path: path.to_owned(),
                }
            }
        }
        _ => error,
    }
}

fn git_error(
    operation: &str,
    args: impl IntoIterator<Item = impl AsRef<str>>,
    error: ProcessError,
) -> WorkspaceError {
    let (status, stdout, stderr) = match error {
        ProcessError::Spawn(error) | ProcessError::Io(error) => {
            (None, Vec::new(), error.to_string())
        }
        ProcessError::NonZeroExit(output)
        | ProcessError::TimedOut(output)
        | ProcessError::Cancelled(output) => (
            output.exit_code(),
            output.stdout,
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ),
    };
    let command = std::iter::once("git".to_owned())
        .chain(args.into_iter().map(|arg| arg.as_ref().to_owned()))
        .collect::<Vec<_>>()
        .join(" ");
    WorkspaceError::Git(GitError {
        operation: operation.to_owned(),
        command,
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr,
    })
}
