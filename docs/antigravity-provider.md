# Antigravity CLI Provider

`AntigravityProvider` は、Antigravity CLI (`agy`) の headless 実行を
`AgentProvider` 契約へ適合させる Adapter です。実行時には次の形式で CLI を
起動します。

```text
agy -p <prompt> --output-format json
```

実行の cwd には `ProviderRequest::workspace()` を使用します。CLI の JSON
出力に含まれる `response` は `AgentResult` に、`usage` の各項目は
`UsageMetric` に変換されます。stdout / stderr は
`ProviderResult::expose_stdout_for_trusted_processing` /
`expose_stderr_for_trusted_processing` で明示的に取得できます。これらはredaction前の
本文を返すため、信頼された呼び出し側だけが使い、既知secretのredactionなしに保存・返却してはいけません。
Providerの通常error本文にはraw診断を含めません。

## 可用性と診断

インストール確認は次のように行います。

```rust,no_run
use ai_dev_orchestrator::AntigravityProvider;

let provider = AntigravityProvider::new();
provider.check_availability()?;
# Ok::<(), ai_dev_orchestrator::ProviderError>(())
```

CLI が PATH にない場合は `ProviderError::Unavailable` になります。認証が
必要な場合や認証情報が無効な場合は `Unavailable` を返します。その他の非0終了は
`ExecutionFailed` です。error本文にheadless実行のraw診断を含めません。

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
