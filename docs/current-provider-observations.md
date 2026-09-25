# Providerの現在状態を観測する

この実装段階では、Codex、GitHub Copilot、AntigravityのローカルCLIを短い期限付きで起動し、CLIの有無と
`--version`確認結果を記録できる。CLI起動成功だけでは、アカウント認証、Model利用権、quota、利用量、料金を
確認できないため、これらを`unknown`として扱う。

```rust
let observation = CodexProvider::new().observe_current();
```

返す`ProviderObservation`は、CLIの存在とversion確認、Provider全体の利用可否、認証状態、Provider既定Modelの
利用可否を別々に保持する。時刻と情報源は各Evidence / AvailabilityObservationに付く。CLIが見つからない場合は
CLIの不在を観測値として記録し、Providerのavailabilityを`unavailable`とする。CLIがある場合は、認証や現在の
Model利用権を調べていないため、それらとProvider availabilityは`unknown`となる。probe時のstdout/stderrは保存・返却しない。
`AttemptTargetObservation`はLedger上のrequested Provider / Modelとobserved Provider / Modelを別fieldとして読み、
未記録のobserved値をrequested値で補完しない。

この観測APIは現在値だけを返す。以前の観測値を保存したり、stale値と最新取得失敗を並べたりするcacheはまだない。
また、既存の`task.get_context` wire schemaは認証状態や情報源の個別fieldを持たないため、この段階では観測をcontextへ
変換しない。Provider APIのquota、rate limit、credit、reset、実使用量・料金、repository設定budgetとcomputed remainderも
データ源がないものは追加しない。これらを`0`、無制限、`available`、推定値として扱わない。

観測型の正本は[モデル選定仕様](model-selection-spec.md)である。ここで追加したCLI probeは、その仕様全体の実装ではなく、
Issue #60 の取得可能な現在情報に対する準備段階である。#55 context契約への接続、取得値を保存するLedger、認証・Model利用権・
Provider usage/budget sourceは後続作業が必要であり、Issue #60を完了扱いにはしない。
