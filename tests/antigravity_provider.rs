#![cfg(unix)]

use ai_dev_orchestrator::{
    AgentProvider, AntigravityProvider, CancellationToken, ProviderError, ProviderRequest,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

static NEXT_FAKE_CLI_ID: AtomicU64 = AtomicU64::new(0);

struct FakeCli {
    directory: PathBuf,
    executable: PathBuf,
}

impl FakeCli {
    fn new(body: &str) -> Self {
        let unique_directory = std::env::temp_dir().join(format!(
            "ai-dev-orchestrator-antigravity-{}-{}",
            std::process::id(),
            NEXT_FAKE_CLI_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&unique_directory).expect("create fake CLI directory");
        let executable = unique_directory.join("agy");
        fs::write(&executable, format!("#!/bin/sh\n{body}\n")).expect("write fake CLI");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("make fake CLI executable");
        Self {
            directory: unique_directory,
            executable,
        }
    }

    fn provider(&self) -> AntigravityProvider {
        AntigravityProvider::with_executable(self.executable.clone())
    }
}

impl Drop for FakeCli {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn request(workspace: impl Into<PathBuf>, prompt: &str, timeout: Duration) -> ProviderRequest {
    ProviderRequest::new(workspace, prompt, timeout)
}

#[test]
fn executes_headless_cli_in_workspace_and_maps_json_result() {
    let cli = FakeCli::new(
        r#"set -e; if [ "$1" = "--version" ]; then printf 'agy fake 1.0\n'; exit 0; fi; test "$1" = "-p"; test "$2" = "hello"; test "$3" = "--output-format"; test "$4" = "json"; printf '{"status":"SUCCESS","response":"done","usage":{"input_tokens":12,"output_tokens":4}}\n'; printf 'fake diagnostic:%s\n' "$PWD" >&2"#,
    );
    let workspace = cli.directory.join("workspace");
    fs::create_dir(&workspace).expect("create workspace");
    let provider = cli.provider();

    assert_eq!(provider.provider_ref().as_str(), "antigravity");
    provider
        .check_availability()
        .expect("fake CLI is available");
    let result = provider
        .execute(&request(workspace.clone(), "hello", Duration::from_secs(1)))
        .expect("provider succeeds");

    assert_eq!(result.exit_status(), Some(0));
    assert!(result.stdout().contains("\"status\":\"SUCCESS\""));
    assert!(result.stderr().starts_with("fake diagnostic:"));
    assert!(
        result
            .stderr()
            .contains(workspace.to_string_lossy().as_ref())
    );
    assert_eq!(
        result.agent_result().expect("agent result").summary(),
        "done"
    );
    let usage = result.usage().expect("usage");
    assert!(
        usage
            .metrics()
            .iter()
            .any(|metric| metric.name() == "input_tokens" && metric.value() == "12")
    );
}

#[test]
fn reports_missing_cli_as_unavailable() {
    let provider =
        AntigravityProvider::with_executable("/definitely/not/installed/antigravity-cli");

    assert!(matches!(
        provider.check_availability(),
        Err(ProviderError::Unavailable(message)) if message.contains("was not found")
    ));
}

#[test]
fn validates_workspace_before_starting_cli() {
    let cli = FakeCli::new("exit 0");
    let result = cli.provider().execute(&request(
        cli.directory.join("missing-workspace"),
        "hello",
        Duration::from_secs(1),
    ));

    assert!(matches!(result, Err(ProviderError::InvalidRequest(_))));
}

#[test]
fn reports_authentication_failure_as_unavailable() {
    let cli = FakeCli::new(
        r#"printf '{"status":"ERROR","error":"authentication required"}\n'; printf 'authentication required\n' >&2; exit 1"#,
    );
    let result = cli.provider().execute(&request(
        cli.directory.clone(),
        "hello",
        Duration::from_secs(10),
    ));

    assert!(matches!(
        result,
        Err(ProviderError::Unavailable(message)) if message.contains("authentication required")
    ));
}

#[test]
fn maps_timeout_to_provider_error() {
    let cli = FakeCli::new("sleep 10");
    let result = cli.provider().execute(&request(
        cli.directory.clone(),
        "hello",
        Duration::from_millis(20),
    ));

    assert!(matches!(
        result,
        Err(ProviderError::TimedOut { timeout }) if timeout == Duration::from_millis(20)
    ));
}

#[test]
fn maps_cancellation_to_provider_error() {
    let cli = Arc::new(FakeCli::new("sleep 10"));
    let provider = cli.provider();
    let token = CancellationToken::new();
    let other = token.clone();
    let workspace = cli.directory.clone();
    let thread = thread::spawn(move || {
        provider
            .execute_with_cancellation(&request(workspace, "hello", Duration::from_secs(10)), other)
    });

    thread::sleep(Duration::from_millis(20));
    token.cancel();
    assert!(matches!(
        thread.join().expect("provider thread"),
        Err(ProviderError::Cancelled)
    ));
}
