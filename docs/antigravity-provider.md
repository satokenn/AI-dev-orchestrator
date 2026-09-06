# Antigravity CLI Provider

`AntigravityProvider` は、Antigravity CLI (`agy`) の headless 実行を
`AgentProvider` 契約へ適合させる Adapter です。実行時には次の形式で CLI を
起動します。

```text
agy -p <prompt> --output-format json
```

実行の cwd には `ProviderRequest::workspace()` を使用します。CLI の JSON
出力に含まれる `response` は `AgentResult` に、`usage` の各項目は
`UsageMetric` に変換されます。元の stdout / stderr と exit status も
`ProviderResult` から参照できます。

## 可用性と診断

インストール確認は次のように行います。

```rust,no_run
use ai_dev_orchestrator::AntigravityProvider;

let provider = AntigravityProvider::new();
provider.check_availability()?;
# Ok::<(), ai_dev_orchestrator::ProviderError>(())
```

CLI が PATH にない場合は `ProviderError::Unavailable` になります。認証が
必要な場合や認証情報が無効な場合も、headless 実行時の診断メッセージを
`Unavailable` として返します。その他の非0終了は `ExecutionFailed` です。

## 手動 Live Provider Test

実 Agent を呼ぶため、通常の unit / integration test ではなく、認証済みの
開発環境で明示的に実行します。

1. Antigravity CLI をインストールし、対話モードの `agy` を一度実行して認証する。
2. 対象 workspace で次を実行する。

   ```shell
   agy -p "このリポジトリの目的を一文で説明してください" --output-format json
   ```

3. 終了コードが 0 で、stdout の JSON に `status: "SUCCESS"`、`response`、
   `usage` が含まれることを確認する。
4. 認証していない環境では、ブラウザを開こうとしてハングしないこと、または
   `authentication required` 等の診断で終了することを確認する。

Provider 経由の利用では、`ProviderRequest` の timeout と
`AntigravityProvider::execute_with_cancellation` の
`CancellationToken` が ProcessRunner に渡されます。
