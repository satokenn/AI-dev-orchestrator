# Providerの現在状態を観測する

Codex、GitHub Copilot、AntigravityのローカルCLIを短い期限付きで起動し、CLIの有無と`--version`確認結果を
`task.get_context` v2の`providers` ContextPageへ返せる。CLI起動成功だけでは、アカウント認証、Model利用権、
quota、利用量、料金を確認できないため、これらを`unknown`として扱う。

```rust
let observation = CodexProvider::new().observe_current();
```

Serviceからv2 ContextPageを組み立てる場合は次のread-only APIを使う。返値はMCPのenvelopeではなく、#45のtransportが後で対応するstructured contentである。
`task.create`ではSecretScannerによるredaction後の要求textだけをsnapshotと冪等性記録に保存する。冪等性照合もredacted canonical payload同士で行う。redaction結果は再適用で変化しない固定点である必要があり、Serviceが固定点を確認できない場合を含め、Scanner未設定・失敗時は固定の`policy_denied`で拒否する。`task.get_context`はsnapshotを返す前、かつProviderのCLI probeより前に再redactして固定点を確認するため、既存の未redacted snapshotもraw textをcontextへ出さない。redaction不能なら`policy_denied`としてProvider probeも行わない。

```rust
let context = service.get_context(
    &task_id,
    &[TaskContextSection::Providers, TaskContextSection::Usage, TaskContextSection::Attempts],
    20,
    &BTreeMap::new(),
)?;
let structured_content = context.to_json_value();
```

返す`ProviderObservation`は、CLIの存在とversion確認、Provider全体の利用可否、認証状態、Provider既定Modelの
利用可否を別々に保持する。時刻と情報源は各Evidence / AvailabilityObservationに付く。CLIが見つからない場合は
CLIの不在を観測値として記録し、Providerのavailabilityを`unavailable`とする。CLIがある場合は、認証や現在の
Model利用権を調べていないため、それらとProvider availabilityは`unknown`となる。probe時のstdout/stderrは保存・返却しない。
`AttemptTargetObservation`はLedger上のrequested Provider / Modelとobserved Provider / Modelを別fieldとして読み、
未記録のobserved値をrequested値で補完しない。

この観測はread-only contextを組み立てる時に取得し、観測履歴としてLedgerへ保存しない。別の`usage` ContextPageは既にLedgerに保存されたProvider報告metricだけを返し、quotaやbudgetを補わない。`providers` detailsには
authentication、CLI状態、Modelごとの状態と、それぞれのsource・timestamp・unknown理由を保持する。
`attempts` detailsもLedgerのrequested Provider / Modelとobserved Provider / Modelを別々のEvidenceとして返し、
requested値をobserved値へコピーしない。Provider APIのquota、rate limit、credit、reset、実使用量・料金、repository設定budgetとcomputed remainderも
データ源がないものは追加しない。これらを`0`、無制限、`available`、推定値として扱わない。

観測型の正本は[モデル選定仕様](model-selection-spec.md)である。このContextPage接続はIssue #60全体の完了を意味しない。
過去のProvider観測cache、実使用量・予算・性能統計、認証やModel利用権を確認する権威あるsourceは後続作業が必要である。
Context builderは`task.create` request snapshotを持たない既存Taskを、誤ったsnapshotで返さず`TaskSnapshotUnavailable`として拒否する。
