# 監督Codex向け MCP 操作契約

監督Codexが Rust Operation Service に依頼できる操作と、その結果として返る事実を示す。操作の詳細な wire schema、再送・ページング・取消・エラー等の規則は[実装者向け詳細仕様](mcp-operation-contract-reference.md)を参照する。

## 監督CodexとServiceの分担

監督Codexは依頼の解釈、Provider / Model の選択、再実行やreviewの要否、Artifactの採否、Task完了を判断する。Rust Operation Serviceは依頼を検査し、明示された操作を実行・記録し、revision・権限・予算・workspace等の機械的制約を強制する。MCP Gatewayはtool requestをServiceへ渡して結果を返す。

Serviceは次のProvider / Model、retry、review、成果物の採否、Task完了を自分で選ばない。Providerの終了状態、Validation結果、review verdict、CI状態は別々の観測事実であり、ひとつが成功しても他の成功やTask完了を意味しない。Attempt / Artifact / Validation / ReviewVerdict / CodexDecision等の意味は[ドメインモデル](domain-model.md)を参照する。

## 操作の流れ

1. Taskを作成するか、既存Taskのcontextを読む。
2. 必要な操作を明示して依頼する。長時間処理はoperation IDを受け取り、後で状態や結果を読む。
3. 返された記録を判断し、必要なら次の操作を依頼する。Serviceは判断を代行しない。
4. Artifactを受け入れる場合は判断を記録し、必要な証拠を照合して公開・CI確認・Task完了を依頼する。

## 依頼できる操作

| Tool | 渡すもの | 返るものと読み方 |
| --- | --- | --- |
| `task.create` | Issue snapshot、または手入力の要求と制約 | Task ID と初期 revision。以後の操作対象を識別する |
| `task.get_context` | Task ID と読みたい情報の分類 | Taskの要求・revisionと、Provider、usage、Attempt、Artifact、Validation、review、decision、PR / CI等の記録。これは現在までに記録された事実であり、次の手順の自動提案ではない |
| `attempt.run` | Task、Provider / Model、instruction、role、入力Artifactまたは初期base | 受付時にoperation IDとAttempt ID。完了後にProvider実行の状態、出力Artifact、usage等。受付は実行成功を意味しない |
| `operation.get` / `operation.list_logs` | operation ID。ログ取得ではstreamと必要な範囲 | operationの進行状態・最終結果、または指定範囲のredacted log。ログ末尾を読んだだけではoperationの終了を意味しない |
| `operation.cancel` / `task.cancel` | 対象operation、またはTask全体の停止要求 | 取消要求の受付と現在状態。停止確認前に取消完了とは扱わない |
| `validation.run` | 対象Artifactと検査profileまたは検査内容 | 受付後、対象Artifactに対する各検査の結果。Validation成功はArtifactの採用判断ではない |
| `decision.record` | 対象Artifact、監督Codexの判断、理由、参照証拠 | 保存されたCodexDecisionと更新後revision。reviewerの判断とは別の記録 |
| `publication.publish` | 対象Artifactと公開先・PR情報 | 受付後、公開結果とPR / head SHAの参照。PR作成だけでCI成功やTask完了にはならない |
| `ci.get` / `ci.wait` | Publication、PRまたはcommitと、待機時は期限 | 対象SHAについて観測したcheck状態。`unknown` / `pending` は成功ではない |
| `task.finish` | 完了させるArtifact、accepted decision、必要な証拠 | 条件を満たせばcompleted Task。Serviceは証拠を照合し、不足や不一致があれば完了を拒否する |

## 結果を読むときの要点

- 非同期toolの受付応答は「依頼を受け付けた」という意味であり、処理結果は `operation.get` で確認する。
- Provider実行成功、Validation成功、review承認、監督Codexの採用判断、CI成功、Task完了は互いに置き換えられない。
- 証拠は保存済みの記録への参照である。別Artifactや別PR / SHAの結果を対象の根拠として使わない。
- `unknown`、未観測、timeout、中断は成功やゼロとして扱わず、状態を読み直して次の操作を判断する。
- Serviceが返す観測事実をCodexが書き換えたり、Serviceが未依頼の判断・次工程を自動実行したりしない。

toolごとの必須fieldと型、共通response、cursorの継続条件、requestの冪等性、revision競合、error code、secretとlogの扱いは[実装者向け詳細仕様](mcp-operation-contract-reference.md)を参照する。
