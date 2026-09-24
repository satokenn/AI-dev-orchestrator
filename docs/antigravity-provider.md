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

`observe_model_catalog()` は `agy models` の候補 ID / 表示名を `ProviderCli` source、コマンド名、
取得時刻付きで返します。CLI出力は安定した機械形式ではないため、起動失敗、非0終了、timeout、
不正UTF-8、切り詰め、未対応形式は `unknown_reason` を持つ空の observation になります。
列挙された ID はアカウントでの利用権や実行成功を保証しないため、この結果だけから Model
availability を `available` にしてはいけません。

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

`ProviderRequest::model()` が `ModelChoice::Named` の場合は `--model <slug>` を
CLIへ渡します。`ProviderDefault` の場合はModel引数を省き、Antigravity CLIの設定または
既定Modelを使います。現行のJSON resultは実際に解決したModel名を含まないため、
observed Providerは`antigravity`、observed Modelはunknownです。CLIがModel slugを
認識しない場合は、Provider境界で`ProviderError::UnsupportedModel`として返します。
