# Repository-local 設定

Repository固有の機械検証は `.ai-dev-orchestrator/config.toml` に設定します。既存のLedger・管理worktreeと同じディレクトリ系統を使い、保存場所の改名やデータ移行は行いません。`init_repository(root)` は初期templateを作成します。既存設定を上書きせず、初期化と読込の両方でconfig directory/fileのsymlinkを拒否し、解決先がRepository内にあることを確認します。これらは通常時のpath境界を検証するもので、検査とfile openの間に別プロセスがpathを置き換える競合までは保証しません。

## Validation check

checkは構造化されたプロセス起動として記述し、配列の順に実行します。引数をshell文字列へ連結しません。cwdは対象workspace内で解決し、親dir参照やworkspace外へ解決されるsymlinkはcheck開始前に拒否します。timeoutは正のミリ秒で指定します。commandのstdout/stderrはsecretを含む可能性があるため、ValidationResultへ返さずLedgerにも保存しません。結果はcheck名、exit status、固定診断だけを含みます。cancelはProcessRunnerへ渡され、process groupの停止契約が適用されます。停止確認済みのcancelは通常のfailed validationと区別し、ValidationResultを作りません。Rust呼出元は`Validator::validate_with_cancellation`、Orchestratorの`execute_with_cancellation` / `execute_with_model_and_cancellation` / `execute_validated_decision_with_policy_and_cancellation`、またはArtifact Serviceの`validate_artifact_with_cancellation`へ自身のtokenを渡せます。既存の引数なしvalidation APIは独立した新規tokenで実行します。現行CLIには呼出元のcancel tokenを渡す入口がないため、CLIからcancel可能とは扱いません。

```toml
schema_version = 1

[validation]
checks = [
  { name = "format", command = "cargo", args = ["fmt", "--all", "--", "--check"], cwd = ".", timeout_ms = 60000 },
  { name = "tests", command = "cargo", args = ["test", "--workspace"], cwd = ".", timeout_ms = 300000 },
]
```

config fileがない状態での読込はエラーです。check listがない、または空の場合、設定読込とValidator生成は `NoChecksConfigured` で失敗し、無条件成功を返しません。CLIの通常実行ではこの確認をPlannerやProviderの起動前に行います。未知のTOML fieldは拒否され、credentialやsecret用fieldは定義していません。credentialは外部commandが通常使う仕組みで管理し、このファイルへ書き込まないでください。

通常のIssue実行では、PlannerやProviderを起動する前にRepositoryの設定を一度読み、同じsnapshotからValidatorを選びます。生成されたworktree内の設定変更で実行中のcheck定義が変わることはありません。ValidationResultには設定ID（`.ai-dev-orchestrator/config.toml`）と、設定ファイル本文のSHA-256をconfig versionとして記録します。schema_versionは設定形式の互換性判定に使い、設定内容の識別には本文hashを使います。古いLedger行ではこれらの列をNULLとして保持し、既存結果の意味を変えません。
