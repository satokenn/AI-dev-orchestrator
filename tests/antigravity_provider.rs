#![cfg(unix)]

use ai_dev_orchestrator::{
    AgentProvider, AntigravityProvider, CancellationToken, ModelChoice, ProviderError,
    ProviderRequest,
};
use std::{
    fs,
    io::Write,
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
        let mut script = fs::File::create(&executable).expect("create fake CLI");
        script
            .write_all(format!("#!/bin/sh\n{body}\n").as_bytes())
            .expect("write fake CLI");
        drop(script);
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
    ProviderRequest::new(workspace, prompt, timeout, ModelChoice::ProviderDefault)
}

fn named_model_request(
    workspace: impl Into<PathBuf>,
    prompt: &str,
    timeout: Duration,
) -> ProviderRequest {
    ProviderRequest::new(
        workspace,
        prompt,
        timeout,
        ModelChoice::Named(ai_dev_orchestrator::ModelRef::new("gemini-test")),
    )
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
    assert_eq!(result.observed_provider().unwrap().as_str(), "antigravity");
    assert!(result.observed_model().is_none());
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
fn passes_named_model_and_types_invalid_model_selection() {
    let cli = FakeCli::new(
        r#"set -e; if [ "$1" = "--version" ]; then exit 0; fi; found=0; while [ "$#" -gt 0 ]; do if [ "$1" = "--model" ]; then shift; test "$1" = "gemini-test"; found=1; break; fi; shift; done; test "$found" = 1; printf '{"status":"SUCCESS","response":"done"}\n'"#,
    );
    let workspace = cli.directory.join("workspace");
    fs::create_dir(&workspace).expect("create workspace");
    let result = cli.provider().execute(&named_model_request(
        workspace,
        "hello",
        Duration::from_secs(5),
    ));
    assert!(result.is_ok(), "named model execution failed: {result:?}");

    let empty_model_request = ProviderRequest::new(
        cli.directory.join("workspace"),
        "hello",
        Duration::from_secs(1),
        ModelChoice::Named(ai_dev_orchestrator::ModelRef::new("")),
    );
    assert!(matches!(
        cli.provider().execute(&empty_model_request),
        Err(ProviderError::InvalidRequest(message)) if message.contains("model identifier")
    ));

    let cli = FakeCli::new(
        r#"if [ "$1" = "--version" ]; then exit 0; fi; printf '{"status":"ERROR","error":"invalid model selection: unknown model"}\n'; exit 1"#,
    );
    let workspace = cli.directory.join("workspace");
    fs::create_dir(&workspace).expect("create workspace");
    let error = cli
        .provider()
        .execute(&named_model_request(
            workspace,
            "hello",
            Duration::from_secs(1),
        ))
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        ProviderError::UnsupportedModel { model, .. } if model.as_str() == "gemini-test"
    ));

    let cli = FakeCli::new(
        r#"if [ "$1" = "--version" ]; then exit 0; fi; printf '{"status":"ERROR","error":"invalid model selection: unknown model"}\n'"#,
    );
    let workspace = cli.directory.join("workspace");
    fs::create_dir(&workspace).expect("create workspace");
    let error = cli
        .provider()
        .execute(&named_model_request(
            workspace,
            "hello",
            Duration::from_secs(1),
        ))
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        ProviderError::UnsupportedModel { model, .. } if model.as_str() == "gemini-test"
    ));
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

    let error = result.unwrap_err();
    assert!(
        matches!(error.kind(), ProviderError::Unavailable(message) if message.contains("authentication required"))
    );
}

#[test]
fn maps_timeout_to_provider_error() {
    let cli = FakeCli::new("printf partial; exec sleep 10");
    let result = cli.provider().execute(&request(
        cli.directory.clone(),
        "hello",
        Duration::from_secs(1),
    ));

    let error = result.unwrap_err();
    assert!(
        matches!(error.kind(), ProviderError::TimedOutWithOutput { timeout, stdout, .. } if *timeout == Duration::from_secs(1) && stdout == "partial")
    );
    assert_eq!(error.captured_output().unwrap().stdout(), b"partial");
}

#[test]
fn maps_cancellation_to_provider_error() {
    let ready = std::env::temp_dir().join(format!("agy-cancel-ready-{}", std::process::id()));
    let cli = Arc::new(FakeCli::new(&format!(
        "printf partial; touch {}; exec sleep 10",
        ready.display()
    )));
    let provider = cli.provider();
    let token = CancellationToken::new();
    let other = token.clone();
    let workspace = cli.directory.clone();
    let thread = thread::spawn(move || {
        provider
            .execute_with_cancellation(&request(workspace, "hello", Duration::from_secs(10)), other)
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !ready.exists() && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(ready.exists(), "fake CLI did not reach the ready marker");
    let _ = fs::remove_file(&ready);
    token.cancel();
    let result = thread.join().expect("provider thread");
    assert!(
        matches!(
            &result,
            Err(error) if matches!(error.kind(), ProviderError::CancelledWithOutput { stdout, .. } if stdout == "partial")
                && error.captured_output().is_some_and(|output| output.stdout() == b"partial")
        ),
        "unexpected cancellation result: {result:?}"
    );
}
