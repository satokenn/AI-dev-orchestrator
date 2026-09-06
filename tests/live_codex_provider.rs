use ai_dev_orchestrator::{AgentProvider, CodexProvider, ProviderRequest};
use std::{env, time::Duration};

#[test]
#[ignore = "実 Codex CLI と認証情報を必要とする手動テスト"]
fn codex_cli_provider_live_smoke_test() {
    let workspace = env::var("CODEX_PROVIDER_LIVE_WORKSPACE")
        .expect("CODEX_PROVIDER_LIVE_WORKSPACE must point to an isolated workspace");
    let request = ProviderRequest::new(
        workspace,
        "Respond with exactly LIVE_PROVIDER_OK. Do not modify any files.",
        Duration::from_secs(120),
    );
    let result = CodexProvider::new()
        .execute(&request)
        .expect("Codex CLI provider should execute successfully");

    assert_eq!(result.exit_status(), Some(0));
    assert!(result.stdout().contains("LIVE_PROVIDER_OK"));
}
