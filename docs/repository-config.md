# Repository-local 設定

Repository固有の機械検証は `.ai-dev-orchestrator/config.toml` に設定します。既存のLedger・管理worktreeと同じディレクトリ系統を使い、保存場所の改名やデータ移行は行いません。`init_repository(root)` は初期templateを作成します。既存設定を上書きせず、初期化と読込の両方でconfig directory/fileのsymlinkを拒否し、解決先がRepository内にあることを確認します。これらは通常時のpath境界を検証するもので、検査とfile openの間に別プロセスがpathを置き換える競合までは保証しません。

## Validation check

checkは構造化されたプロセス起動として記述し、配列の順に実行します。引数をshell文字列へ連結しません。cwdは対象workspace内で解決し、親dir参照やworkspace外へ解決されるsymlinkはcheck開始前に拒否します。timeoutは正のミリ秒で指定します。cancelはProcessRunnerへ渡され、process groupの停止契約が適用されます。

```toml
schema_version = 1

[validation]
checks = [
  { name = "format", command = "cargo", args = ["fmt", "--all", "--", "--check"], cwd = ".", timeout_ms = 60000 },
  { name = "tests", command = "cargo", args = ["test", "--workspace"], cwd = ".", timeout_ms = 300000 },
]
```

config fileがない状態での読込はエラーです。check listがない、または空の場合、Validatorは `NoChecksConfigured` で失敗し、無条件成功を返しません。未知のTOML fieldは拒否され、credentialやsecret用fieldは定義していません。credentialは外部commandが通常使う仕組みで管理し、このファイルへ書き込まないでください。

現在の公開Validation結果schemaにはconfig/profile識別子がありません。そのため、この実装はValidation結果をconfig versionへ関連付けません。DomainとOperationの結果schemaで保存先が定義されるまで、この条件は未対応であり、Issueは完了扱いになりません。
