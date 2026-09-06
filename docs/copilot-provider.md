# GitHub Copilot CLI Provider

`CopilotProvider` は GitHub Copilot CLI (`copilot`) の headless 実行を
`AgentProvider` 契約へ適合させる Adapter です。実行時には次の形式で CLI を
起動します。

```text
copilot -p <prompt> -s --no-ask-user --allow-tool=write,shell
```

実行の cwd には `ProviderRequest::workspace()` を使用します。`-s` により
Copilot の応答だけを stdout に出力させ、元の stdout / stderr と exit status を
`ProviderResult` から参照できます。`ProviderRequest::timeout()` と
`CancellationToken` は `ProcessRunner` に渡されます。

既定の tool permission は、ファイル変更用の `write` とリポジトリの検証・操作用の
`shell` です。権限を狭める場合は `with_allowed_tools` で明示します。

```rust,no_run
use ai_dev_orchestrator::CopilotProvider;

let provider = CopilotProvider::new().with_allowed_tools(["write"]);
assert_eq!(provider.allowed_tools(), ["write"]);
```

## 可用性と診断

インストール確認は次のように行います。

```rust,no_run
use ai_dev_orchestrator::CopilotProvider;

let provider = CopilotProvider::new();
provider.check_availability()?;
# Ok::<(), ai_dev_orchestrator::ProviderError>(())
```

CLI が PATH にない場合は `ProviderError::Unavailable` になります。未認証や
Copilot Requests 権限の不足を示す headless 実行の診断も `Unavailable` として返し、
その他の非 0 終了は `ExecutionFailed` として返します。

## 手動 Live Provider Test

実 Agent を呼ぶため、通常の unit / integration test ではなく、認証済みの開発環境で
明示的に実行します。

1. GitHub Copilot CLI をインストールし、`copilot` の `/login` で認証する。
2. Copilot が変更してよい隔離 workspace を用意する。
3. 次を実行する。

   ```shell
   copilot --version
   COPILOT_PROVIDER_LIVE_WORKSPACE=/tmp/copilot-provider-live \
     cargo test --test live_copilot_provider -- --ignored --nocapture
   ```

このテストは利用枠と実行時間を消費するため、Pull Request の通常 CI では実行しません。
