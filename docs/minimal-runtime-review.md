# Codexを監督主体とする最小実行基盤の再設計案

調査日: 2026-09-21。状態: **調査結果と採用提案。実行時の仕様変更は未適用**。

本書は同日の調査を詳述したものであり、Issueや実装の状態は下記の固定commitと調査時点に対応する。後から読む場合は、その後の変更を別途照合する。

## 読み方

まず「目的をどう評価するか」と「現在の実行経路と不足」で、今回の評価基準と問題を確認する。「推奨する最小構成」と「代表的な利用シナリオ」は、利用時に何が変わるかを説明する。「実装を分ける単位と依存関係」は、採用後にどの順番で変更するかを示す。

API名、データ項目、実装単位は設計案であり、現在利用できる機能の一覧ではない。既存仕様を変える提案は、確認済みの不具合と分けて読む必要がある。

## 結論と作業範囲

目的は、Codexが判断に集中できるよう、状態管理・モデル呼び出し・予算管理・ログ・検証・CI観測をRustへ外出しすることである。推奨構成は「監督中のCodex → 小さな操作API → Rustの実行・記録 → 判断材料をCodexへ返す」という反復である。

現行の部品は多くを再利用できる。必要なのは全面的な書き直しではなく、CLI内部の自動計画・公開を分離し、失敗や途中状態も含む永続的な操作境界へ組み直すこと。内部のCodex Planner呼び出し、必須の別AIレビュー、複数roleの一括選定を最小構成の前提から外す。モデル選択・再試行・レビュー要否・完了判断は監督Codexが担う。

今回の成果物は全Issueの分類、実装経路の監査、最小構成、移行順、検証条件である。既存の未コミット変更を保持し、GitHubのIssue・PR・設定は変更しない。公開API・状態モデル・DB・既存CLI動作の変更は、下記の具体案を確認した後の実装とする。

## 調査基準と確度

- 最新main: [`4ff2b12dfe22f04022bb265c12fe662c2115600a`](https://github.com/satokenn/AI-dev-orchestrator/tree/4ff2b12dfe22f04022bb265c12fe662c2115600a)。全source module、テスト構成、CI・公開workflow、設定、関連設計文書を調査した。
- GitHub Issue: open/closedを含む45件、本文とコメントを取得。Open 13件、Closed 32件。Open PRは0件。以下の番号はPR番号を含まない。
- ローカルHEADは`0ee72be`。README、`src/copilot_provider.rs`の既存変更と未追跡`HANDOFF.md`を保持した。古いローカルにない機能を「未実装」と誤判定しないため、mainを`/private/tmp/ai-dev-orchestrator-audit-20260921`へ隔離取得した。
- 確認済み: コード経路、Issue記載、ローカル試験結果、GitHub mainの有効なRequired Check。推奨: 以下の最小構成と優先順位。未確認: 実Providerの現在の利用可否・モデル一覧・料金、実運用DB、実サービスでの再起動復旧。
- 閉じたIssueは実装完了の証拠ではない。#46–#49、#51はコメントで設計撤回・分割が明記される。#14のcloseにはcommitが紐づかず、現行mainにHerdr実装はない。

## 目的をどう評価するか

### 外出ししたい負担

Codexが長い作業を監督する際、繰り返し必要になるのは、どの実行が動いているかの確認、終了待ち、ログ回収、試行数や利用量の集計、検証結果の保存、PRのCI状態の確認である。これらを会話履歴とその場のshell操作だけで管理すると、会話が途切れたときの復元や、同じ操作を再送したときの重複防止が難しい。

Rustへ移す価値があるのは、毎回同じ規則で処理でき、結果を記録して後から照合できる作業である。一方、「この失敗は入力不足か実装ミスか」「修正を同じモデルに任せるか」「テストが通っていても要求に対して不十分か」といった判断には、要求・差分・文脈の解釈が必要になる。この部分は監督Codexに残す。

この区別は、Rustが一切の条件分岐を持たないという意味ではない。timeout到達時の停止、予算不足の拒否、検証失敗の記録、API待機の期限管理はRustが自動処理する。事前に許可された操作の実行手順まで、Codexへ逐一問い合わせる必要はない。

### 成功を測る基準

| 観点 | 良い状態 | 今回は成功指標にしないもの |
| --- | --- | --- |
| Codexの作業負担 | 指示後の状態・ログ・利用量を1か所から取得できる | 単に利用するモデル数が増えること |
| 判断の連続性 | 再接続後も、何を指示し何が起きたかを復元できる | 内部Plannerが自律的に長く動くこと |
| 実行の確実性 | 同じ指示を再送しても重複副作用を避けられる | 正常系だけの実演が通ること |
| 予算 | 守れる上限と観測値を区別して制御できる | 根拠のない「安いモデル」順位を出すこと |
| 成果物 | Codexが見た差分、検証した内容、公開した内容が一致する | テスト成功だけでTaskを完了すること |
| 保守性 | 1つの規則の変更が1つのサービス境界へ収まる | APIや抽象化の数を増やすこと |

上記は評価基準の提案である。例えば「操作後にCodexが何回pollしたか」「失敗診断を得るために何個のログを探したか」は将来比較できるが、今回その数値を測定したわけではない。

### 分類の意味

「不要」は、この目的の最小構成に入れる必然性がないという意味であり、将来も価値がないという断定ではない。「過剰」は、必要な責務に比べて入口・状態・前提の数が多い場合を指す。「不足」は、既存機能または要求された目的を成立させるための保証が欠けている場合を指す。

同じ機能が複数の分類にまたがる場合もある。たとえばretry APIの重複は過剰だが、前回成果物の引継ぎは不足している。したがって、retry機能全体を削除するという結論にはならない。

P0/P1は今回の再設計内の優先順位であり、外部の脆弱性評価ではない。P0は主要な責務境界または継続実行の信頼性に直接関わるもの、P1はその境界を完成させるために必要な整合性・設定・周辺接続とする。

## 現在の実行経路と不足

現行の本番経路は`main.rs → cli::ProductionRuntime → ProductionIssueExecutor → CodexPlanner → Orchestrator → Provider → Validator → Ledger保存 → GitHubWorkflow::publish`。CLIの`run`はIssue入力専用で、全Providerを登録し、内部Plannerを呼び、試行上限1・各3600秒で実行する。MCP、監督Codexへの途中返却、CI待機は存在しない。

| 分類・優先度 | 確認した事実と因果関係 | 必要な変更 |
| --- | --- | --- |
| 責務のずれ・P0 | `cli.rs:118`以降は内部Plannerが選択し、検証成功後にそのまま公開する。監督Codexが今回の差分を採否判断する境界がない | 選定・実行・確認・公開を分け、各結果を監督Codexへ返す |
| 不足・P0 | `cli.rs:161`の保存はOrchestratorが`Ok`を返した後のみ。Provider/Workspace/Validatorエラーで保存に到達せず、開始・終了時刻も`None`。実行中にstatusから見えない | 外部起動前に実行意図とAttemptを記録し、全終了経路で結果を保存。Task/Attempt更新を同一transactionにする |
| 不足・P0 | `github_workflow.rs:502`は検証のboolだけで公開を許す。検証対象のtree/commit識別がなく、公開時は`:276`で`git add -A` | 検証・Codexの採否・公開対象を同一成果物IDへ結び付け、変更後は再検証。採用ファイルとPR本文をCodexの指示として受け取る |
| 不足・P0 | `workspace.rs:293`の`git worktree add -b`に基準commit指定がなく、その時のローカルHEADから開始する。再試行は別worktreeで、前回の変更や診断の引継ぎはない | 初回base SHAと再試行の入力成果物を明示。修正は前の成果物を保持したworkspaceで直列実行する案を推奨 |
| 不足・P0 | `ExecutionPolicy`は回数とtimeoutだけ。金額・token・quotaの記録用型はあるが、予算予約・精算・不明時の処理はない | 実行前の上限チェック、予約、実績精算、計測不能時の扱いを実装する |
| 不足・P0 | `ProcessRunner`は直接の子だけkillし、pipe readerをjoinする。子孫がpipeを保持するとtimeout後も待つ。stdout/stderrは無制限にメモリへ蓄積 | 子孫を含む停止、期限付き出力回収、サイズ制限、ファイルへの逐次記録。起動前cancelも拒否する |
| 不足・P0 | Orchestratorは`provider.execute`を呼び、外部cancel tokenを伝えない。Providerのtimeout/cancel変換は`ProcessOutput`を捨てる | 操作IDから停止を要求でき、確認できた停止結果と部分ログを保存する共通契約 |
| 過剰かつ不整合・P1 | `execute`、`execute_decision`、`*_with_policy`など複数入口があり、前二者は全hard gateを通らない。`ProviderRegistry`が実行できないのに`AgentProvider`を実装する | 実行入口を1つに集約し、Registryは検索だけを担当。旧APIは移行期間後に除去 |
| 不足・P1 | `ValidatedPlannerDecision`はTask IDやsnapshot revisionを保持しない。検証済みの選択を別Taskへ渡すことを型で防げない | 指示をTask・revision・request IDへ束縛してRustが再検証 |
| 不足・P1 | `execution_ledger.rs:411`のupsertで終了済みAttemptのProvider/状態/時刻を上書きでき、検証履歴もdelete/reinsertされる | 実行中更新と終端記録を区別し、終端の事実は不変。イベントは追記する |
| 不足・P1 | `domain.rs:457`は遷移検証前に失敗理由を変更する。`Task::complete`はAttemptの状態を検査しない | 拒否操作の原子性と、サービス層の完了条件を保証。低水準state transitionと成果物受入を混同しない |
| 不足・P1 | Issueはtitle/bodyのみ取得。Task IDは`issue-N`で、共有Ledgerではrepository間で衝突する。PR本文はIssue本文と検証summaryの連結 | repositoryを含む同一性、要求snapshotとコメント、Codexが作るPR説明を保持 |
| 不足・P1 | 公開の冪等性keyはrepository+Issueのみ。commit直後のDB失敗、同時実行、再度同じIssueを処理する区別が弱い。PR検索はclosed/mergedも既存扱い | 操作単位のkey、ローカル排他、保存前後の外部状態照合。完了済み公開の再利用と新規実行を区別 |
| 不足・P1 | `GitHubWorkflow`のgit/ghは直接`Command::output`しtimeout/cancelなし。baseはCLIで`main`固定。repo引数とpush先originの照合がない | 既存ProcessRunnerへ統合し、remote・base・SHAを開始前に確定 |
| 不足・P1 | Validatorにはcwd境界検査があるがtimeout/cancel設定がない。CLIはRustValidator固定 | #50の構造化設定と期限・停止を接続。check未設定は成功扱いしない |
| 不足・P1 | #53のAntigravity不正JSON成功扱い、#56のCodex JSONL未正規化が現存。availabilityは`--version`だけ | 不正応答を診断として返し、起動可否・認証・モデル利用可否を区別。unknownを成功やゼロへ変換しない |
| 不足・P1 | PR PolicyはPR由来checkerを実行。有効なmain Required CheckもAPIで`PR Policy`のみと確認 | #52、#54を実施。CI観測では対象head SHAと必須check集合を確認する |

参照元: [CLI][src-cli]、[Orchestrator][src-orchestrator]、[Planner][src-planner]、[Domain][src-domain]、[Ledger][src-ledger]、[Workspace][src-workspace]、[ProcessRunner][src-process]、[GitHub workflow][src-github]、[Validator][src-validator]。行番号は上記固定commitを指す。

worktreeとcwd検査は誤操作防止であり、OSのアクセス制御ではない。Copilotのshell権限と継承環境を含め、workerがGitHub公開権限を持つ経路も検討対象になる。Promptだけで公開禁止を強制できたとは扱わない。MVPで汎用sandboxを新設するのではなく、許可したProvider設定・実行権限と保証できない境界を明示する。

### なぜ局所的な修正だけでは足りないか

**Ledgerの例。** `save_task`をエラー処理へ追加すれば、ある失敗の保存漏れは改善できる。しかし、起動前に何も記録しないままなら、実行中のプロセスが落ちた場合の追跡はできない。開始を保存しても、実行予約と予算消費が別々に更新されれば、再送やクラッシュで二重計上が起こり得る。必要なのは保存箇所の追加だけでなく、指示受付から終了までを同じ操作IDで管理することである。

**公開の例。** `ValidatedPublication`という型名だけでは、その中の検証結果が「今から公開する差分」に対応することは保証されない。検証後にファイルが変わる、workerが別のcommitを追加する、再開時に異なるworkspaceを参照する、といった変化を検出するには、成果物の同一性を検証・採否・公開へ通して保持する必要がある。

**timeoutの例。** エラー種別が`TimedOut`になることと、指定期限内に外部実行が止まり呼び出しが返ることは異なる。今回の観察では、直接の子プロセスをkillした後に、子孫が保持するstdout/stderrのpipeが閉じるまで待っていた。そのため、「TimedOutを返す」という既存テストだけでは停止保証を確認できない。

**再試行の例。** 新しいAttempt IDを作るだけでは修正を継続できない。前回の変更が別worktreeに残り、新しいworkspaceが初期HEADから作られるなら、修正担当は前回の変更を受け取れない。必要なのは、Attemptの履歴と修正対象の成果物を別々に識別して結び付けることである。

これらは、Plannerのモデル選定を高度化しても解消しない。判断主体へ渡す事実と、判断後に適用される操作の一貫性を先に整える必要がある。

## 不要・過剰・不足の整理

### 最小構成から外すもの

- **内部の第二の判断主体**: 監督Codexが既にいるとき、Rustが毎回別のCodex Plannerを起動する必要はない。単独CLIによる自動実行を残す場合だけ任意adapterにする。
- **固定の実装→別AIレビュー→再選定workflow**: 別モデルのレビューはCodexが必要に応じて依頼する補助情報。approveを意味的な最終承認へ自動昇格させない。#62の「Codexのレビューを減らすこと」を中核目的にしない。
- **#57の全面実装を入口の前提にすること**: 多role同時割当、能力行列、全期間のperformance集計、費用予測は最初の1実行に不要。Model指定、unknown、観測時刻・出所、指示とsnapshotの対応は残す。
- **並列DAG、integration branch、cherry-pick、Herdr、固定RunPhase、自動stash、merge自動化**: いずれも今の機械的外出しに必須ではない。#46–#49、#51の撤回方針を引き継ぐ。cancel/復旧/CI観測はこの撤回に巻き込まない。
- **一般配布の拡張**: 既存release workflowを急に削除する価値は小さいが、#38の当初対象外だった二種macOS向けGitHub Releaseより先に中核を完成させる。

### 残すもの

Task/Attemptの区別、Provider adapter、ProcessRunner、WorkspaceManager、機械的Validator、SQLite Ledger、PR公開の外部操作adapter、通常CIとLive testの分離を残す。単一crateのまま進め、サービス分割、汎用workflow engine、プラグインregistryの追加はしない。DB schema migrationは既存データ保持に必要であり、単に長いという理由で削らない。

### 内部Plannerを外す理由と、残してよい場合

現在の`CodexPlanner`そのものが弱いモデルという意味ではない。問題は、監督中のCodexが目的と会話文脈を持っているのに、Rustが別の呼び出しへ限定された情報を渡して選択をやり直し、その後の処理を一続きで進めることである。この構成は追加の呼び出し、情報の組み直し、失敗処理を必要にする。

主経路では、監督Codexの選択をそのまま構造化入力として受け取り、Rustは利用可否・予算・現在状態との整合性を検証すればよい。既存Plannerにある「未知Providerを拒否する」「理由を記録する」といった処理は再利用できる。

将来、人間が単独CLIにIssueだけを渡し、監督Codexとの対話なしに起動したいという独立要件が確定した場合は、そのCLIがCodexを監督役として起動する構成は成立する。その場合も判断主体は明確に1つにし、同じ操作サービスを利用させる。現時点でこの第二の利用方式を必須にする根拠はない。

### 別AIレビューを任意化する理由

別モデルによるレビューは有用な場合があるが、「別モデルがapproveした」という事実から要求達成を決めることはできない。レビュー入力の範囲、参照した差分、見落とし、モデルの失敗などを監督Codexが評価する必要がある。

任意化とはレビュー機能の禁止ではない。Codexが必要と判断したら、対象成果物と観点を指定してProviderへ依頼し、結果を通常の実行記録として保存する。実装モデルと異なるモデルを必須にするかどうかも、Taskの条件として扱えばよい。

全Taskに同じレビュー工程を強制すると、文書の軽微な修正にも追加の実行費用・状態・失敗分岐が必要になる。最小構成では「任意の作業指示を実行して結果を返す」機能を先に完成させ、専門的なレビュー出力の標準化は実際の利用を見て追加する。

## 推奨する最小構成

| 境界 | 所有する情報と操作 | 所有しない判断 |
| --- | --- | --- |
| 監督Codex | 要求解釈、作業指示、Provider/Model選択、差分評価、修正/再試行/レビュー要否、完了判断、公開内容 | 実測usageやプロセス終了の捏造、予算制約の上書き |
| 操作サービス | request検証、状態遷移、排他、予算、実行、永続化、取消、復旧、結果の返却 | 次の仕事やモデルを自発的に選ぶこと |
| Provider / Process / Workspace | 指定モデルの起動、プロセス制御、workspace・成果物の同一性、入出力 | 仕事全体の成功判定 |
| Validator / GitHub adapter | 指定checkの実行、指定PRの公開、CIの取得・待機、SHA付き証拠 | test成功だけで要求達成やmergeを決めること |
| Ledger | 操作・Attempt・結果・利用量・公開参照・判断の記録 | GitHub側のcheck状態や実課金額の独自推定 |

実際の1件の操作は次の順序になる。

1. CodexがIssue/要求を確認し、RustへTask作成を依頼する。Rustがrepository・base SHA・要求snapshot・policyを記録する。
2. Rustが観測結果と現在の履歴を返す。CodexがProvider/Model・具体的prompt・修正元workspaceを指定する。
3. Rustがrevision・予算・権限を検証し、実行予約とAttemptを保存してから起動する。すぐに操作IDを返し、進捗と結果を後から取得できる。
4. 実行後、Rustが差分参照、部分ログ、exit/error、要求モデルと実際のモデル、usageを保存する。機械検証は明示されたcheck列として実行する。
5. Codexが同じ成果物の差分・検証・必要なログを確認する。修正や任意のレビューは新しい操作で指示する。Rustは自動的にモデルを切り替えない。
6. Codexが公開対象と本文を決める。Rustが対象成果物と検証証拠の一致を確認してcommit/push/PRを処理する。
7. Rustが当該PR headのCIを取得・待機し結果を返す。失敗の修正判断はCodex。要求された完了点がCI成功までなら、PR作成だけではTaskを完了しない。mergeはMVPに含めない。

### 操作API案

これは#55の改訂案であり、既存APIではない。MCPと診断用CLIは同じサービスを呼ぶ薄い入口にする。最初は1Taskあたり変更する操作を直列にし、永続キュー・分散schedulerは作らない。

| 操作 | 主な入力 | 結果 |
| --- | --- | --- |
| `create_task` | repo、Issueまたは目的、base、実行制約、request ID | Task ID、revision、要求snapshot |
| `get_context` | Task ID、必要な履歴範囲 | 状態、候補の観測値、予算、Attempt、差分/検証/CIの参照 |
| `execute` | Task ID、expected revision、request ID、Provider/Model、prompt、workspace参照 | Attempt/operation ID。実行結果は`get_operation`で取得 |
| `validate` | Task ID、成果物ID、設定済みcheck集合、request ID | operation ID、成果物に結び付いたValidation |
| `get_operation` / `read_log` | ID、cursor、取得上限 | 進捗、終了結果、切り詰めたログと次cursor |
| `cancel` | Task/operation ID、request ID | 取消要求の受付と、後続の停止確認結果 |
| `publish_pr` | 成果物ID、検証ID、Codexの採否記録、base、タイトル/本文、request ID | 公開操作ID、commit SHA、PR URL |
| `wait_checks` | PR、expected head SHA、期限 | 当該SHAの必須check結果、pending/errorと観測時刻 |
| `finish_task` | Task ID、expected revision、成果物/検証/CI参照、理由 | 制約と証拠を確認して確定した状態 |

新規実行指示の概形（IDとモデル名は例）:

```json
{
  "schema_version": 1,
  "request_id": "request-2",
  "task_id": "task-1",
  "expected_revision": 3,
  "target": {"provider": "example", "model": {"kind": "provider_default"}},
  "workspace_id": "workspace-1",
  "instruction": "前回の検証失敗を修正し、変更理由を報告する"
}
```

受付結果例:

```json
{"request_id":"request-2","operation_id":"op-2","attempt_id":"attempt-2","revision":4,"status":"accepted"}
```

`accepted`は実行成功を意味しない。同じrequest ID・同じpayloadは同じ操作を返し、別payloadは拒否する。stale revision、未知target、予算不足、閉じたTaskは外部副作用前に拒否する。Codexからstate/usage/CI結果を設定させるAPIは設けない。

### 状態・ログ・成果物

- Taskの粗い状態は維持する。`Active`は「プロセス動作中」だけでなくCodexの次判断を待つ状態も含める。判断待ちのためだけのRunPhaseは追加しない。
- 現行AttemptはProvider終了後に必ず`Validating`へ進むため、独立した実行・任意のレビューには合わない。推奨はAttemptを1回のモデル呼び出しの記録にし、実行結果・機械検証・Codexの採否を分離すること。`Succeeded`を残すなら「呼び出し成功」に限定し、要求達成と区別する。これは#19/#21の意味変更を伴うため要確認。
- 既存履歴の`Succeeded`は旧来の「検証成功」として保存し、新しい意味へ無断変換しない。schema versionと旧結果の由来を残す。既存DBを削除して移行しない。
- workspaceはTaskに紐づけて直列で再利用し、Attemptごとの開始・終了時点の成果物IDを残す案を推奨する。以前のworktreeは削除せず明示的に採用する。レビューは対象snapshotを固定し、書き換えを許さない実行設定を使う。
- 成果物IDはHEAD SHAだけでは不足する。未コミット・新規ファイルを含む正確な入力treeと差分を識別する。公開時のtreeが異なるなら採否・検証を失効させる。
- durableな実行意図→起動→観測→終了の順で記録する。クラッシュとspawnの間には不確実な窓があるため、再起動時に同じ処理を無条件で再実行しない。PID単独に依存せず所有権を照合し、判断不能ならinterruptedという診断を返す。これは独立RunPhaseではない。
- ログは上限付きで逐次保存する。MCPには必要な断片を返し、認証値を含む環境やraw log全量を自動投入しない。原文保持が必要な場合はローカルの制限されたアクセス下に置く。

### 予算の最小保証

「usageを表示できる」と「予算を守れる」を分ける。まず回数、経過時間、取得可能なtoken/金額を単位付きで管理する。指示の受付transactionで予約し、成功・失敗・取消の全経路で実績を精算する。停止しても既に発生した費用は消えない。

金額上限の厳密な保証には、Provider側の実行上限または既知の最大消費量が必要である。事後usageしか得られないProviderに「残予算が正なら必ず予算内」とは言えない。hardな金額上限を指定されたのに上限を強制できない場合は実行を拒否する。回数/時間制限のみで許可する設定とは明確に区別する。unknownを0円、定額subscriptionを無制限と扱わない。外部で共用するaccountの残量は、このLedgerだけでは保証できない。

予測価格、モデルランキング、全Provider横断の成功率は後回しにする。#60の観測APIと予算の強制処理は別責務であり、集計を実装しても予算管理が完成したことにはならない。

## 操作境界を成立させるための詳細

### 最小限の記録と、その存在理由

以下は論理的に必要な情報であり、そのまま同数のDB tableやRust moduleを作る提案ではない。既存Task/Attempt/Ledgerへ収められる部分はそこへ追加する。

| 記録 | 最低限の情報 | 必要な理由 |
| --- | --- | --- |
| Task | ID、repository、目的・要求snapshot、状態、revision、policy参照 | 仕事の同一性と、指示時点の前提を固定する |
| 操作受付 | request ID、入力の識別情報、Task ID、操作種別、受付・終了情報 | 再送と新規操作を区別し、処理中でも問い合わせ可能にする |
| Attempt | Task/操作ID、要求Provider/Model、実測Model、指示、開始・終了、結果・診断 | モデル呼び出し1回の事実を残す |
| Workspace参照 | 管理ID、repository、path、branch、初回base SHA | 任意pathの実行や別repositoryとの混同を防ぐ |
| 成果物参照 | 対象workspace、内容の識別子、元の成果物、差分参照 | 修正・検証・レビュー・公開が同じ内容を指すようにする |
| 検証結果 | 成果物ID、check定義の識別子、各終了結果・時刻・診断 | コードだけでなく検証条件の変化も追跡する |
| Codexの判断記録 | Task、対象成果物、指示または採否、理由、参照した証拠 | 「なぜ次へ進んだか」と「何を確認したか」を後から追えるようにする |
| 予算記録 | 適用範囲、単位、上限、予約量、実績、不明な消費 | 起動前判定と精算を同じ台帳に結び付ける |
| 公開・CI参照 | repository、branch、commit、PR、check対象SHA、観測時刻 | 外部副作用の再確認と、古い結果の誤適用防止 |

ここでいう「操作」は、モデル実行・検証・公開・待機の受付を識別するためのものに限定する。Taskの進行を別の固定RunPhaseへ複製しない。モデル実行では操作IDとAttemptが対応し、検証や公開にはそれぞれの結果が対応する。

API上にroleを持たせる場合も、まずは「今回の実行は調査か実装かレビューか」を説明する任意の属性で足りる。roleごとに別のschedulerやTask階層を作る必要はない。

### 非同期実行と排他

モデル実行中も状態取得と取消を受け付ける必要があるため、MCPの受信処理を長時間の同期呼び出しで塞がない。実行受付後にoperation IDを返し、実行処理はサーバー内で継続する方式を提案する。具体的な非同期ライブラリの選択は、この文書では固定しない。

最初の実装では、同じworkspaceを変更する操作を1件に制限すればよい。Taskのrevisionを受付時に比較し、操作の登録・予算予約・必要な状態更新を1つのtransactionで確定する。2件の同時要求のうち後から確定しようとしたものは、古いrevisionまたは実行中として拒否する。

この排他はモデル実行だけでなく、検証中・成果物確定中・公開中にも必要になる。検証と同時に別のworkerがファイルを編集できるなら、成果物IDを持っていても証拠が不安定になる。読み取り専用の状態取得は並行して許可する。

MCP切断後も同一サーバープロセスが生きている場合と、サーバー自体が終了した場合は分ける。前者は同じoperation IDで再取得できる。後者の無停止継続は最小構成では保証せず、保存済み情報から実行の残存・終了を照合する。常駐daemonを新設するかは、切断後も必ず継続したい要件が確定した場合に検討する。

### 冪等性と復旧

冪等性は「何度呼んでも何も起きない」という意味ではなく、同じ指示の再送を新しい仕事として二重に実行しないことである。最初の実行を終えた後に再度修正したい場合は、新しいrequest IDを使う。

| 障害発生位置 | 再接続後の扱い |
| --- | --- |
| 受付transactionより前 | 操作は未受付。同じrequest IDを再送できる |
| 受付済み・起動前と確認できる | 記録済み操作として扱い、重複Attemptを作らない |
| 起動したか不明 | 自動再実行しない。所有プロセスとworkspaceを照合し、不明なら診断をCodexへ返す |
| 実行中 | 残存が確認できれば観測を継続。確認不能なら成功と推定しない |
| 実行終了・結果保存前 | ログ・終了証拠を回収できる範囲で復旧し、回収不能なusageはunknownにする |
| push成功・DB更新前 | remoteの当該branchと期待SHAを照合してから後続へ進む |
| PR作成成功・DB更新前 | repository/head/baseと対象実行の対応を確認して既存PRへ接続する |

SQLite transactionだけではOSのspawnやGitHub操作まで原子的にできない。「すべてexactly once」と主張せず、受付の一意性、外部状態の照合、判断不能時の停止を組み合わせる。この区別をエラー契約とテストへ反映する。

### エラーは次の判断に必要な事実として返す

エラーを一律の文字列にすると、Codexはログを読み直して、実行が始まったのか、workspaceに変更が残ったのか、再実行すると二重になるのかを推測することになる。機械的に判明した範囲を構造化する。

| 分類案 | 意味 | Codexへ返すべき情報 |
| --- | --- | --- |
| invalid_request | 入力が形式・範囲に反する | 不正field、適用されなかったこと |
| stale_revision / busy | 指示時点の状態が変わった、または実行中 | 現在revision、先行operation ID |
| policy_rejected | 予算・実行先・権限の条件を満たさない | 拒否した規則と観測値。新規実行の有無 |
| provider_failed / invalid_output | 起動後に失敗、または応答形式が不正 | exit、部分ログ、変更の有無、usageの確度 |
| timed_out / cancelled | 期限到達または明示取消 | 停止要求と停止確認の区別、残存の有無 |
| interrupted / recovery_required | 実行結果または外部副作用の確定ができない | 確定済みの最後の段階と照合可能な参照 |
| validation_failed | checkを実行し不合格だった | 成果物ID、checkごとの結果・診断 |
| observation_unavailable | CIやusageの取得に失敗した | 取得対象、最後の観測時刻、今回の取得失敗 |

`retryable`を返す場合は、「再要求を技術的に受け付けられる」という補助情報に限定する。「同じモデルへ同じ指示を再送すべき」という意味的判断とは分ける。

### 予算予約・精算の例

以下の金額は説明用であり、実モデルの価格ではない。Taskの上限が100単位、確定消費が30単位、進行中の予約が20単位なら、新規予約に使えるのは50単位となる。40単位を上限として強制できる次の呼び出しは受け付けられるが、60単位は拒否する。

40単位を予約した呼び出しが12単位の実績で終了した場合、確定消費へ12単位を加え、当該予約を解放する。usageを取得できなかった場合は、0として解放せず、未精算として保持する等の規則が必要になる。後で実績が取得できたら同じ操作へ追記し、二重計上しない。

最小実装では通貨換算や全Provider共通の点数化はしない。回数、時間、token、通貨をそれぞれの単位で検査する。金額を強制できないProviderでも回数・時間を制限して使うことは可能だが、その設定を金額保証と呼ばない。

予算上限を変える場合は、通常の実行指示と分けて記録する。最初に利用者が許可した上限を、workerの応答やCodexのtarget選択が暗黙に増額しないようにする。

### Codexへ返す情報量

`get_context`は全履歴と全ログを毎回返すAPIにしない。通常はTaskの目的とrevision、直近の操作結果、予算、差分・検証・CIの要約と参照を返す。詳しい診断は`get_operation`や`read_log`で必要な分だけ取得する。

履歴は件数やcursorで分割し、省略したことを明示する。ログには取得範囲と切り詰めの有無を付ける。要約と原文の参照を結び付け、要約が原文の代替証拠にならないようにする。最初から要約用モデルを追加せず、機械的な集計と抽出で始める。

## 代表的な利用シナリオ

### 1. 一度の実装で完了する

CodexがIssueと関連資料から目的・制約を整理し、Taskを作る。Rustがrepositoryとbase SHAを確定し、Codexは候補情報を見て1つのProvider/Modelを指定する。Rustが実行・記録し、Codexは返された差分と機械検証の結果を評価する。

要件が満たされていれば、Codexは対象成果物とPR本文を指定して公開を要求する。Rustが同じ成果物をcommit/pushし、PRの当該head SHAのCIを待機する。Codexはその証拠を用いて、依頼された完了点へ到達したことを判断する。

このケースで別AIレビューを必須にはしない。Codexが採否判断へ必要な情報を受け取れることが中核条件である。

### 2. 検証失敗を修正する

Attempt 1はモデル呼び出しとして正常に終了しても、Validation 1が不合格になることがある。Rustは両方の事実を残してCodexへ返す。Codexは失敗ログを読み、同じモデルへ修正を頼むか、別モデルへ切り替えるか、検証条件を見直すかを判断する。

修正を依頼する場合は新しいAttempt 2を作り、Attempt 1の成果物を入力にする。以前のAttemptの失敗を成功へ書き換えない。Validation 2は修正後の成果物に結び付ける。これにより、何回修正し、どこで何が変わったかが残る。

検証コマンド自体を変更した場合も、その変更を記録する。単にテストを弱めて合格した結果を、元の条件を満たしたこととして扱わない。変更の妥当性はCodexの判断対象である。

### 3. モデルを使わずに検証・CIだけを外出しする

Codex自身がコードを書き、Rustへ検証・記録・CI待機だけを依頼する使い方も、この目的に合っている。全Taskにworkerモデル呼び出しを要求する必要はない。

その場合は、管理済みworkspaceの成果物を参照し、AttemptなしでもValidationや公開操作を記録できる必要がある。任意の利用者workspaceを無条件に採用するのではなく、管理対象の同一性を確認する。具体的なworkspace登録操作は#55で必要最小限に定める。

### 4. 必要な箇所だけ別AIへレビューを依頼する

Codexが「この変更は並行処理に関わるので追加の観点が欲しい」と判断した場合、対象成果物と確認観点を固定してレビューを依頼する。Rustはレビューを1回の呼び出しとして記録し、出力をCodexへ返す。

レビューAIのapprove/changes_requestedは報告内容である。Codexは根拠を評価して採用・追加調査・修正を選ぶ。レビュー中に実装成果物が変更された場合、そのレビュー結果を新しい成果物の証拠として流用しない。

### 5. 取消や再起動が発生する

Codexまたは利用者が取消を要求すると、Rustは取消要求を記録し、実行中の処理へ停止を伝える。受付応答だけで`Cancelled`確定とは扱わず、停止を確認した結果と部分ログを返す。取消後に新しいAttemptを自動追加しない。

サーバー再起動の場合は、保存済みの実行中操作を照合する。停止済みか、まだ動いているか、既に公開が完了したかを確認し、確定できない点をCodexへ戻す。「途中だったからもう一度実行する」という一律処理を避ける。

### 6. CI中にPRのheadが変わる

`wait_checks`がSHA Aを対象としている間に、追加pushでPRがSHA Bへ進む場合がある。RustはAの成功をBの成功として返さず、対象が変わったことを通知する。BのCIを待つ操作は、新しい対象として扱う。

CI取得失敗、まだ登録されていないcheck、実行中、失敗、成功を区別する。必須check集合が不明なとき、取得できたcheckがすべて緑であるという理由だけで完了にしない。

## 全Issueの処置案


Open/Closedは調査時点。以下は変更提案であり、GitHub上の状態変更はしていない。

| Issue | 状態 | 分類・処置 |
| --- | --- | --- |
| [#1](https://github.com/satokenn/AI-dev-orchestrator/issues/1) 開発ルール | Closed | 維持。今回も設計変更の確認と検証水準を適用 |
| [#2](https://github.com/satokenn/AI-dev-orchestrator/issues/2) PRテンプレート | Closed | 維持。内容は監督Codexが作り、Rustは受け渡し・形式検査 |
| [#3](https://github.com/satokenn/AI-dev-orchestrator/issues/3) 初期architecture | Closed | 目的に合う。採用後に現在の実装・最小構成へ更新 |
| [#4](https://github.com/satokenn/AI-dev-orchestrator/issues/4) Rust品質 | Closed | 維持。検証合格と意味的完了を分離 |
| [#5](https://github.com/satokenn/AI-dev-orchestrator/issues/5) 基本CI | Closed | 維持。#54で実際のmerge条件と一致させる |
| [#13](https://github.com/satokenn/AI-dev-orchestrator/issues/13) workspace初期化 | Closed | 維持。単一crateで十分 |
| [#14](https://github.com/satokenn/AI-dev-orchestrator/issues/14) Herdr | Closed | 最小構成外。未実装。追加runtimeの必須化はしない |
| [#15](https://github.com/satokenn/AI-dev-orchestrator/issues/15) PR Policy | Closed | 維持・#52で信頼境界を修正。本文自己申告を内容の正しさとは扱わない |
| [#19](https://github.com/satokenn/AI-dev-orchestrator/issues/19) domain設計 | Closed | Task/Attemptを維持。実行・検証・受入の結合は改訂 |
| [#21](https://github.com/satokenn/AI-dev-orchestrator/issues/21) domain実装 | Closed | 改訂。拒否操作の原子性、終端記録、完了条件を補う |
| [#23](https://github.com/satokenn/AI-dev-orchestrator/issues/23) ProcessRunner | Closed | 中核。子孫停止、出力上限、起動前cancelを補う |
| [#24](https://github.com/satokenn/AI-dev-orchestrator/issues/24) Provider契約 | Closed | 中核。モデル・ログ・部分失敗・取消能力を明示 |
| [#25](https://github.com/satokenn/AI-dev-orchestrator/issues/25) Codex worker | Closed | 任意workerとして維持。監督Codexと同一役割にしない |
| [#26](https://github.com/satokenn/AI-dev-orchestrator/issues/26) Antigravity | Closed | 既存adapterを維持。#53修正と実CLI確認は別検証 |
| [#27](https://github.com/satokenn/AI-dev-orchestrator/issues/27) Copilot | Closed | 維持。設定済み権限と実行保証を明示 |
| [#28](https://github.com/satokenn/AI-dev-orchestrator/issues/28) worktree | Closed | 中核。base固定・修正元の引継ぎを補う |
| [#29](https://github.com/satokenn/AI-dev-orchestrator/issues/29) Validator | Closed | 中核。#50と成果物ID、timeout/cancelを接続 |
| [#30](https://github.com/satokenn/AI-dev-orchestrator/issues/30) Orchestrator | Closed | 固定連続workflowから操作サービスへ縮小・再構成 |
| [#31](https://github.com/satokenn/AI-dev-orchestrator/issues/31) Codex Planner | Closed | 監督経路から内部呼び出しを外す。選択検証のロジックは再利用 |
| [#32](https://github.com/satokenn/AI-dev-orchestrator/issues/32) retry | Closed | 回数制限を維持。次の実行はCodexの明示指示。重複APIを整理 |
| [#33](https://github.com/satokenn/AI-dev-orchestrator/issues/33) hard policy | Closed | 中核。全入口の統一、予算、実行権限を補う |
| [#34](https://github.com/satokenn/AI-dev-orchestrator/issues/34) Issue→PR | Closed | 公開操作は維持。自動連結を外し、成果物と採否を束縛 |
| [#35](https://github.com/satokenn/AI-dev-orchestrator/issues/35) Ledger | Closed | 中核。事後保存だけでなく全経路へ組み込む |
| [#36](https://github.com/satokenn/AI-dev-orchestrator/issues/36) CLI | Closed | 薄い操作/診断入口に縮小。Issueの手動target・timeout指定は本番CLI未対応。doctorもgit/ghを検査していない |
| [#37](https://github.com/satokenn/AI-dev-orchestrator/issues/37) E2E | Closed | 検証基盤を再利用。現在のfake E2Eはライブラリ配線であり本番CLI/MCP経路の証明ではない |
| [#38](https://github.com/satokenn/AI-dev-orchestrator/issues/38) macOS binary | Closed | 保守のみ。実binary名は要求のai-devでなくai-dev-orchestrator。配布拡張は中核後 |
| [#45](https://github.com/satokenn/AI-dev-orchestrator/issues/45) MCP | Open | 優先。#55を縮小して、監督Codexから直接操作できる入口を実装 |
| [#46](https://github.com/satokenn/AI-dev-orchestrator/issues/46) RunStatus/Phase | Closed | 撤回を維持。必要なイベント・ログは#63へ |
| [#47](https://github.com/satokenn/AI-dev-orchestrator/issues/47) capability/並列 | Closed | 撤回を維持。固定role行列、並列integrationは不要 |
| [#48](https://github.com/satokenn/AI-dev-orchestrator/issues/48) CI修正/merge | Closed | 一括workflowは復活させない。ただしCI取得・待機は今回の目的に必要なので小さく切り出す |
| [#49](https://github.com/satokenn/AI-dev-orchestrator/issues/49) stash/復旧 | Closed | 自動stashは不要。取消・中断検出・冪等復旧だけを#55/#63と接続 |
| [#50](https://github.com/satokenn/AI-dev-orchestrator/issues/50) config | Open | 維持。既存.ai-dev-orchestratorと提案.orchestratorの二重保存を避ける移行方針も必要 |
| [#51](https://github.com/satokenn/AI-dev-orchestrator/issues/51) 旧MCP E2E | Closed | 撤回を維持。新APIの縦断テストに置き換える |
| [#52](https://github.com/satokenn/AI-dev-orchestrator/issues/52) Policy正本 | Open | 必要な修正。信頼済みcheckerで判定 |
| [#53](https://github.com/satokenn/AI-dev-orchestrator/issues/53) 不正JSON | Open | 必要な修正。成功を捏造しない |
| [#54](https://github.com/satokenn/AI-dev-orchestrator/issues/54) Required Check | Open | 必要な修正。宣言ファイルだけでなく有効設定まで検証 |
| [#55](https://github.com/satokenn/AI-dev-orchestrator/issues/55) MCP契約 | Open | 最優先で改訂。execute/get/cancelと判断材料の返却を中核にする |
| [#56](https://github.com/satokenn/AI-dev-orchestrator/issues/56) Codex JSONL | Open | 維持。料金推測はせずusage・最終報告・失敗を分離 |
| [#57](https://github.com/satokenn/AI-dev-orchestrator/issues/57) 選定仕様 | Closed | 仕様のみ。最小部分へ改訂し、統計・予測・多roleを延期。互換性変更を明示 |
| [#58](https://github.com/satokenn/AI-dev-orchestrator/issues/58) role/review設計 | Open | 縮小。まず実行・検証・採否を分離。必須review工程のための状態追加はしない |
| [#59](https://github.com/satokenn/AI-dev-orchestrator/issues/59) Model指定 | Open | 必要。要求モデルと観測した実モデルを別に保持。未確認はunknown |
| [#60](https://github.com/satokenn/AI-dev-orchestrator/issues/60) 利用状況/実績 | Open | 分割。利用量・利用可否・残予算を先行、性能統計とランキング材料は延期 |
| [#61](https://github.com/satokenn/AI-dev-orchestrator/issues/61) Planner拡張 | Open | 置換。内蔵Planner拡張でなく監督Codexへの観測返却と指示適用 |
| [#62](https://github.com/satokenn/AI-dev-orchestrator/issues/62) 別AIレビュー | Open | 任意補助機能へ延期。Codexの受入判断を置き換えない |
| [#63](https://github.com/satokenn/AI-dev-orchestrator/issues/63) Ledger拡張 | Open | 優先。ログ項目追加より先に実行前記録・失敗保存・冪等性・復旧を完成 |

不足している独立作業は、予算の強制、ProcessRunnerの停止保証、成果物同一性と公開ゲート、CI観測、状態と実行の一体的永続化である。#60や#63へ名称だけで押し込まず、それぞれ受入条件を持たせる。

## 選択肢と移行順

| 案 | 適合性 | 移行・運用コスト |
| --- | --- | --- |
| 現行CLIにMCP wrapperだけ追加 | 内部Plannerと公開自動連結が残り、監督Codexへ制御が戻らない | 初期差分は小さいが目的未達 |
| **既存部品を小さな操作サービスへ再構成（推奨）** | 判断をCodexへ集約し、Rustに機械処理を残せる | API・状態意味・DBの段階移行が必要。単一crateとadapterを再利用できる |
| CLI wrapper/JSONログだけへ全面縮退 | 起動は簡単だが、予算・再起動・重複公開の保証を失う | 既存SQLiteやdomainを捨てて再実装する利益が小さい |

採用後の順序:

1. #55/#57/#58/#61を本案に合わせて更新。旧CLI・型の互換方針とAttemptの意味を確定する。
2. ProcessRunner、domainの原子性、#53、#52/#54を各目的ごとに修正。失敗を正しく観測できる土台を作る。
3. 単一操作サービスにLedger transaction・排他・request ID・実行前予算検証・取消を組み込み、Fake Providerで縦断する。ログ・worktreeの参照も保存する。
4. #59/#56/#50の最小部分を接続し、#45のstdio MCPと診断CLIを同じサービスへ配線する。1Task・直列実行で完成させる。
5. 成果物ID・Codex採否・PR公開・CI待機を接続する。旧runの内部Planner→自動公開経路は、置換経路の検証後に廃止または任意adapter化する。
6. 実Providerの隔離fixtureで受付→実行→失敗→再指示→検証→公開→CI観測を確認する。不要な旧APIはcall siteを移行してから除去する。

最初から#57の全収集項目、#62、並列実行を揃える必要はない。一方、予算とログを後回しにした「モデル起動だけのMCP」は完成としない。

## 実装を分ける単位と依存関係

以下は採用後の作業分割案であり、新規Issueを作成したものではない。各単位は1つの目的としてレビューできる大きさを目指す。文書だけで仕様を確定する単位と、runtimeを変更する単位を分ける。

| 単位 | 主な対象 | 完了時にできること | 前提 |
| --- | --- | --- | --- |
| A. 責務と契約の改訂 | #55/#57/#58/#61、architecture/domain文書 | Codexの入力、Rustの結果、完了の意味が一意に読める | 本案の設計判断 |
| B. プロセス停止と出力回収 | ProcessRunner、Provider error変換 | timeout/cancel後の停止結果と部分ログを返せる | 対象OSでの停止方式の確認 |
| C. 状態更新の原子性 | Domain、Ledger | 拒否操作で値が変わらず、終端事実を上書きしない | Aの状態意味 |
| D. 受付・永続化・予算 | 操作サービス、Ledger、policy | 再送を識別し、起動前記録と予算検査を行える | A/B/C |
| E. モデル指定・検証設定 | #59/#56/#50 | 指定モデル実行、usage取得、設定した検証を行える | A/B、Dへの接続 |
| F. MCPとCLIの接続 | #45、CLI | 本番入口から受付・状態取得・取消できる | D、Eの最小機能 |
| G. 成果物と公開 | Workspace、Validator、GitHub workflow | 確認した成果物だけを公開し、復旧できる | A/D、成果物識別方式 |
| H. CI観測とmerge条件 | CI adapter、#52/#54 | 同じSHAの必須checkを取得・待機できる | G。#52/#54自体は独立修正可能 |
| I. 縦断検証と旧経路整理 | E2E、README、旧API | 本番入口の一連の動作を確認し、不要な入口を整理できる | B〜H |

この表は全作業を1PRへまとめる指示ではない。例えば#53の不正JSON修正や#52のPolicy正本修正は、責務再設計と独立して確認できる。その一方、新しいAPIだけを追加してLedgerや取消の接続を後回しにする分割は、完成していない機能を利用可能に見せるため避ける。

### モジュールごとの変更方針

| 現在の実装 | 再利用する部分 | 変更する部分 |
| --- | --- | --- |
| `src/cli.rs` | 引数受付、診断、status表示 | 内部Planner・固定policy・自動公開の組立てを操作サービス呼び出しへ変更 |
| `src/planner.rs` | target検証、理由の記録、入力の検査 | 主経路のCodex起動を外し、Task/revisionに対応する指示検証へ整理 |
| `src/orchestrator.rs` | Provider/Workspace/Validatorを呼ぶ境界 | 複数execute経路の集約、受付・保存・取消の一体化 |
| `src/retry.rs` | Provider検索、回数・timeout制約 | 重複した制約適用と自動選択に見える入口を整理 |
| `src/domain.rs` | Task/Attemptの識別、遷移検査 | 実行・検証・採否の分離と拒否操作の原子性 |
| `src/execution_ledger.rs` | SQLite、migration、保存・取得 | 全終了経路、終端不変性、request ID、予算予約、操作記録 |
| `src/process_runner.rs` | 引数配列、stdout/stderr回収 | 子孫停止、回収期限、出力上限、逐次保存 |
| Provider各実装 | CLIごとの引数・出力差分の吸収 | Model指定、実Model観測、部分結果付き失敗 |
| `src/workspace.rs` | worktree作成・所属確認・安全なcleanup | base固定、修正元の引継ぎ、成果物参照 |
| `src/validator.rs` | check列と結果集約、cwd境界 | timeout/cancel、設定の識別、成果物との対応 |
| `src/github_workflow.rs` | 外部操作trait、公開結果の保存 | 採否と成果物の照合、remote/base検査、再開・CI観測 |

新しい責務を既存fileへ置くか独立moduleへ切り出すかは、実装時の差分と依存関係を見て決める。この文書では将来のcrate分割やdirectory treeを先取りしない。

### 既存データと互換性

既存Task/AttemptのID、Provider、結果、Validation、usage、公開URLは保存する。旧データに実モデル名や正確な時刻が存在しない場合はunknownとして扱い、今回のProvider設定から過去の値を補わない。

移行は、新しい記録領域と読取経路を追加し、旧形式の解釈を残してから書込先を切り替える方法を提案する。旧`Succeeded`の意味を保ち、必要なら旧形式由来であることを表示する。実行中のTaskを意味変更の途中で移行しないため、書込みを止めた状態でmigrationを行う運用も定義する必要がある。

現行`.ai-dev-orchestrator`と#50の`.orchestrator`の名称差は、機能上の必要性と無関係なデータ移動を起こし得る。初回の再構成では既存保存場所を尊重し、改名する場合は独立した移行手順とする案を推奨する。

旧CLIの`run --issue`を残すか廃止するかは、内部Plannerを主経路から外す判断とは分ける。旧CLIを残すなら明確な互換入口として扱い、新しい監督用入口が旧自動公開経路を誤って呼ばないようにする。旧APIの削除は呼び出し元と文書の移行が済んだ段階で行う。

## 検証と完了条件

今回の検証は最新mainの隔離cloneに対して実施した。

| 水準 | 結果 |
| --- | --- |
| ソース・設定・Issue調査 | 全45Issueとコメント、全実装module、関連テスト・設計・workflowを照合 |
| 静的検査 | `cargo fmt --all -- --check`成功。`cargo clippy --workspace --all-targets --all-features --offline -- -D warnings`成功 |
| コンパイル・自動テスト | `cargo test --workspace --all-features --offline --quiet`: 120成功、Live 3件ignored。Python unittest 10成功 |
| 不具合の局所実行確認 | 隔離cloneの`tests/audit_observations.rs`で4件再現。30ms timeoutが約416msで返る、拒否遷移による失敗理由変更、終端Attempt上書き、FakeRuntimeが検証中AttemptのままTask完了を返す |
| GitHub読み取り | mainの有効ルールもRequired CheckがPR Policyのみ。Open PRなし |
| 未実施 | 実Provider呼び出し、課金、実GitHubへの公開、MCP runtime、再設計の実装、利用者の受入確認 |

4件の監査テストは**現存する問題を確認する観察用**であり、望ましい仕様の回帰テストではない。修正時には期待値を反転させる等の回帰テストへ置き換える。通常テスト合格は本設計の実装完了を意味しない。

再設計後に必須とする受入条件:

- 監督Codexの指示なしに内部Plannerや別モデルへ自動委譲しない。
- 同じ指示の再送、同時送信、Ledger障害、起動前後のクラッシュで重複実行・履歴消失を起こさず、不明な外部状態は不明として返す。
- timeout/cancelが子孫・Validator・GitHub待機へ届き、停止後も部分ログと発生済みusageを確認できる。
- 予算不足、unknownなhard上限、旧revision、不正targetは起動前に拒否する。
- 修正Attemptが前回の成果物を参照し、過去Attemptの事実を変更しない。
- 検証後に成果物が変化すれば公開を拒否する。古いSHAのCI成功、check未取得、必須check不足を成功扱いしない。
- Fakeではなく本番MCP/CLIの配線をFake外部adapterで縦断し、その後に明示されたLive fixtureで実動作を確認する。

### 再設計後のテストケース

| 場面 | 確認する結果 | 主な検証水準 |
| --- | --- | --- |
| 不正な状態遷移 | errorを返し、stateもfailure reasonも変わらない | unit |
| 終端Attemptへの再保存 | 同一事実の再送は許容し、異なる事実への変更は拒否する | Ledger integration |
| 2件の同時実行要求 | 片方だけが受付・予約・起動される | service integration |
| 同じrequest IDの再送 | 同じoperationを返し、Provider呼び出し数が増えない | service integration |
| 同じIDで異なるpayload | 明示的に拒否し、最初の記録を変えない | service integration |
| 予算不足・不明なhard上限 | 起動回数が0、拒否理由と観測値が残る | policy integration |
| Provider失敗・usage不明 | 部分ログと失敗を保存し、消費を0と断定しない | Provider/Ledger integration |
| 子孫がpipeを保持 | 停止手順の許容時間内に返り、子孫の残存を検査する | 対象OSのruntime |
| 大量ログ | メモリに無制限蓄積せず、切り詰め・保存範囲を返す | runtime |
| 途中クラッシュ | 記録済み段階から照合し、無条件の二重起動をしない | 障害注入integration |
| 修正Attempt | 前回の成果物を入力にし、以前の記録を保つ | worktree integration |
| 検証後のファイル変更 | 公開を拒否し、再検証が必要と返す | publication integration |
| PR作成後の保存失敗 | 再要求時に外部参照を照合し、二重PRを作らない | fake GitHub integration |
| CI取得中のhead変更 | 古いSHAの合格を完了条件に使わない | fake GitHub integration |
| 本番CLI/MCPの受付→完了 | 実際のサービス配線を通り、状態と結果を取得できる | fake外部adapterによるE2E |

timeoutの検証では、OS schedulingによる小さな誤差を許容しつつ、明らかに子孫の自然終了まで待つ実装を検出する。固定の数msだけに依存する不安定なテストにはしない。クラッシュ復旧では、受付直後、spawn直後、実行終了直後、push/PR作成直後を分けて障害を注入する。

### 今回再現した4件の読み方

1. **拒否した遷移の副作用**: 成功済みAttemptへ`fail_with_reason(Timeout)`を要求するとerrorになるが、失敗理由だけTimeoutに変わった。期待する回帰テストでは、操作前後のAttempt全体が一致することを確認する。
2. **終端Attemptの上書き**: 失敗済みAttemptと同じIDでQueued・別Providerを保存すると置換された。期待する回帰テストでは、異なる終端事実への更新を拒否し、元のProviderと終了時刻を保持する。
3. **FakeRuntimeの見かけ上の完了**: 不正な順序のValidation適用エラーを無視し、AttemptがValidatingのままTaskがCompletedになった。これは本番が必ず同じ動作をする証拠ではなく、現在のfakeテストが完了保証を十分に表していない証拠である。
4. **timeout後のpipe待ち**: `sh`配下で約0.4秒動く子孫を起動し30msのtimeoutを設定したところ、約416msで戻った。TimedOutというerror分類だけでは、実行時間の上限を保証できないことを示す。

この4件の確認と、今回の文書詳細化は別の作業である。詳細化の際には本体コードや通常テストを変更しておらず、上表の新しいテストを実装済みとは扱わない。

## 実装前の確認点

主要な設計判断は、(1)内部Plannerを監督経路から外す、(2)別AIレビューを任意化する、(3)Attemptをモデル呼び出しの記録へ整理して機械検証・採否を分離する、の3点である。本書は組み合わせて採用する案を推奨するが、それぞれ独立に評価できる。いずれも既存仕様を変えるため、採用前に本体へ適用しない。CI観測・予算・記録・停止保証は最小構成に含める。

他に実装時に決める必要があるのは、成果物を識別・保存する具体的方法、既存CLIの互換期間、対象OSでの子孫停止方式、実測usageを返さないProviderに許可する予算設定である。これらを未決定のまま暗黙のdefaultへ変換しない。

今回の依頼に対する文書化はここまでとする。本書の詳細化は実装案の承認を意味せず、Issueの閉鎖・書換え、既存APIの削除、DB移行、公開動作の変更は行っていない。

[src-cli]: https://github.com/satokenn/AI-dev-orchestrator/blob/4ff2b12dfe22f04022bb265c12fe662c2115600a/src/cli.rs
[src-orchestrator]: https://github.com/satokenn/AI-dev-orchestrator/blob/4ff2b12dfe22f04022bb265c12fe662c2115600a/src/orchestrator.rs
[src-planner]: https://github.com/satokenn/AI-dev-orchestrator/blob/4ff2b12dfe22f04022bb265c12fe662c2115600a/src/planner.rs
[src-domain]: https://github.com/satokenn/AI-dev-orchestrator/blob/4ff2b12dfe22f04022bb265c12fe662c2115600a/src/domain.rs
[src-ledger]: https://github.com/satokenn/AI-dev-orchestrator/blob/4ff2b12dfe22f04022bb265c12fe662c2115600a/src/execution_ledger.rs
[src-workspace]: https://github.com/satokenn/AI-dev-orchestrator/blob/4ff2b12dfe22f04022bb265c12fe662c2115600a/src/workspace.rs
[src-process]: https://github.com/satokenn/AI-dev-orchestrator/blob/4ff2b12dfe22f04022bb265c12fe662c2115600a/src/process_runner.rs
[src-github]: https://github.com/satokenn/AI-dev-orchestrator/blob/4ff2b12dfe22f04022bb265c12fe662c2115600a/src/github_workflow.rs
[src-validator]: https://github.com/satokenn/AI-dev-orchestrator/blob/4ff2b12dfe22f04022bb265c12fe662c2115600a/src/validator.rs
