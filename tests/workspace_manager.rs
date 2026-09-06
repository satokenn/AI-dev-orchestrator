use ai_dev_orchestrator::{
    Attempt, AttemptId, ProviderRef, Task, TaskId, TaskRole, WorkspaceError, WorkspaceManager,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

fn temporary_directory() -> PathBuf {
    let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "ai-dev-orchestrator-workspace-{timestamp}-{sequence}"
    ));
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
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn repository() -> PathBuf {
    let directory = temporary_directory();
    git(&directory, &["init", "-b", "main"]);
    git(
        &directory,
        &["config", "user.email", "test@example.invalid"],
    );
    git(&directory, &["config", "user.name", "Workspace Test"]);
    fs::write(directory.join("README"), "base\n").expect("write initial file");
    git(&directory, &["add", "README"]);
    git(&directory, &["commit", "-m", "initial"]);
    directory
}

#[test]
fn resolves_repository_and_isolates_each_attempt() {
    let repository = repository();
    let nested = repository.join("nested");
    fs::create_dir(&nested).expect("create nested directory");
    let manager = WorkspaceManager::new(&nested).expect("resolve repository root");
    assert_eq!(
        manager.repository_root(),
        repository.canonicalize().unwrap()
    );

    let task = Task::new(TaskId::new("task-1"), "implement", TaskRole::new("worker"));
    let attempt = Attempt::new(AttemptId::new("attempt-2"), ProviderRef::new("codex"));
    assert_eq!(
        manager.branch_name(task.id(), attempt.id()),
        "orchestrator/task/task-1/attempt/attempt-2"
    );
    assert_eq!(
        manager.worktree_path(task.id(), attempt.id()),
        manager.worktree_root().join("task-1").join("attempt-2")
    );

    let workspace = manager
        .create_for_task_attempt(&task, &attempt)
        .expect("create isolated worktree");
    assert_ne!(workspace.path(), manager.repository_root());
    assert!(workspace.path().is_dir());
    assert_eq!(
        git(workspace.path(), &["branch", "--show-current"]),
        workspace.branch().to_owned() + "\n"
    );
    manager
        .validate_provider_workspace(workspace.path())
        .expect("created worktree is allowed for a provider");
    assert!(matches!(
        manager.validate_provider_workspace(manager.repository_root()),
        Err(WorkspaceError::MainWorktreeNotAllowed { .. })
    ));

    manager.cleanup(&workspace).expect("remove worktree");
    assert!(!workspace.path().exists());
    assert!(matches!(
        manager.validate_provider_workspace(workspace.path()),
        Err(WorkspaceError::WorkspaceNotManaged { .. })
    ));

    fs::remove_dir_all(repository).expect("remove test repository");
}

#[test]
fn rejects_existing_path_and_branch_without_overwriting_them() {
    let repository = repository();
    let manager = WorkspaceManager::new(&repository).expect("resolve repository root");
    let task = TaskId::new("collision-task");
    let attempt = AttemptId::new("collision-attempt");
    let path = manager.worktree_path(&task, &attempt);
    fs::create_dir_all(&path).expect("create colliding path");
    fs::write(path.join("sentinel"), "keep").expect("write sentinel");
    assert!(matches!(
        manager.create(&task, &attempt),
        Err(WorkspaceError::PathCollision { .. })
    ));
    assert_eq!(fs::read_to_string(path.join("sentinel")).unwrap(), "keep");
    fs::remove_dir_all(manager.worktree_root()).expect("remove colliding path");

    let branch = manager.branch_name(&task, &attempt);
    git(&repository, &["branch", &branch]);
    assert!(matches!(
        manager.create(&task, &attempt),
        Err(WorkspaceError::BranchCollision { .. })
    ));
    assert_eq!(
        git(&repository, &["branch", "--list", &branch]),
        format!("  {branch}\n")
    );

    fs::remove_dir_all(repository).expect("remove test repository");
}

#[test]
fn dirty_cleanup_preserves_contents_until_force_is_requested() {
    let repository = repository();
    let manager = WorkspaceManager::new(&repository).expect("resolve repository root");
    let workspace = manager
        .create(&TaskId::new("dirty-task"), &AttemptId::new("dirty-attempt"))
        .expect("create isolated worktree");
    let changed_file = workspace.path().join("agent-result.txt");
    fs::write(&changed_file, "uncommitted result\n").expect("write agent result");

    let error = manager
        .cleanup(&workspace)
        .expect_err("ordinary cleanup must reject a dirty worktree");
    assert!(matches!(error, WorkspaceError::Git(_)));
    assert_eq!(
        fs::read_to_string(&changed_file).unwrap(),
        "uncommitted result\n"
    );
    assert!(workspace.path().is_dir());

    manager
        .cleanup_force(&workspace)
        .expect("explicit force cleanup");
    assert!(!workspace.path().exists());
    fs::remove_dir_all(repository).expect("remove test repository");
}

#[test]
fn reports_diagnostic_git_error_for_non_repository() {
    let directory = temporary_directory();
    let error = WorkspaceManager::new(&directory).expect_err("non-repository must fail");
    match error {
        WorkspaceError::Git(git) => {
            assert_eq!(git.operation(), "resolve repository root");
            assert!(git.status().is_some());
            assert!(git.stderr().contains("not a git repository"));
            assert!(git.command().contains("rev-parse"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    fs::remove_dir_all(directory).expect("remove test directory");
}
