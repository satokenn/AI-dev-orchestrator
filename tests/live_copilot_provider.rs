use ai_dev_orchestrator::{AgentProvider, CopilotProvider, ProviderRequest};
use std::{env, time::Duration};

#[test]
#[ignore = "実 GitHub Copilot CLI と認証情報を必要とする手動テスト"]
fn copilot_cli_provider_live_smoke_test() {
    let workspace = env::var("COPILOT_PROVIDER_LIVE_WORKSPACE")
        .expect("COPILOT_PROVIDER_LIVE_WORKSPACE must point to an isolated workspace");
    let request = ProviderRequest::new(
        workspace,
        "Respond with exactly LIVE_PROVIDER_OK. Do not modify any files.",
        Duration::from_secs(120),
    );
    let result = CopilotProvider::new()
        .with_allowed_tools(["write", "shell"])
        .execute(&request)
        .expect("GitHub Copilot CLI provider should execute successfully");

    assert_eq!(result.exit_status(), Some(0));
    assert!(result.stdout().contains("LIVE_PROVIDER_OK"));
}
