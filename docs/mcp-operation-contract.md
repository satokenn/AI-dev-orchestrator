# 監督Codexから操作する MCP 契約

この文書は、監督Codexが Rust の Operation Service を MCP 経由で操作するときの約束を定める。
監督Codexが何を判断し、Rust が何を実行・記録・拒否し、失敗時に何を返すかを、#45 の stdio MCP Gateway がそのまま実装できる形で残すことが目的である。

ここで決めるのは MCP tool の入力と出力、識別子、非同期操作、失敗、公開範囲である。Rust 型、SQLite schema、MCP transport、Provider 固有 CLI、自動 merge は扱わない。

## この文書で決めること

- 監督Codexと Rust Operation Service の責務境界
- Task 作成、情報取得、モデル実行、検証、公開、CI 観測、完了の tool
- Task revision、request ID、operation ID、Attempt ID の関係
- 再送、取消、timeout、途中中断、復旧が必要な状態の扱い
- Artifact、Validation、review、受入、公開、CI を同じ成果物へ結び付ける規則
- secret、認証情報、raw log を監督Codexへ返さない規則

Task、Attempt、Artifact、Validation、review verdict、CodexDecision の意味は[ドメインモデル](domain-model.md)を正本とする。特に Attempt の Succeeded は Provider 呼び出しの正常終了だけを表す。Validation 成功、review の承認、監督Codexの受入、公開、Task 完了は別の事実である。

## 誰が何を決めるか

| 担当 | 決めること | 決めないこと |
| --- | --- | --- |
| 監督Codex | 要求解釈、Provider / Model 選択、再試行・review の要否、成果物の採否、Task 完了 | usage、CI、Provider 実行結果などの観測事実の書換え、policy の迂回 |
| Rust Operation Service | request 検証、状態遷移、排他、予算・timeout・workspace 境界の強制、Provider / Validator / GitHub の実行と記録 | 次に使う Provider / Model や成果物の採否を自発的に選ぶこと |
| reviewer Provider | 指定された成果物を review し、形式が正しければ verdict を返す | 監督Codexの受入、Task 完了 |

Rust 内部の CodexPlanner を呼ぶことは、この主経路の前提にも fallback にも含めない。worker として起動する Codex CLI は、監督Codexとは別の Provider / Model 実行である。

## 基本の流れ

通常は、監督Codexが観測した事実を基に次の操作を一つずつ選ぶ。

~~~text
Taskを作成 → contextを読む → Provider実行を受け付ける → 結果を読む
                                      ↓
                            検証 / review / 修正を判断
                                      ↓
                    受入を記録 → 公開 → CI観測 → Task完了を要求
~~~

Provider、Validation、review、公開、CI 待機はすぐに終わらない場合がある。その場合 Rust は、外部処理を始める前に受付を記録し、operation ID を返す。監督Codexは operation.get、必要なら log または context を読み、次の操作を明示的に選ぶ。失敗したからといって Rust が retry、rework、review、公開、完了を自動選択することはない。

| 起きたこと | Rust が記録・返却すること | 監督Codexが次に判断すること |
| --- | --- | --- |
| Provider が正常終了した | Attempt=Succeeded と出力 Artifact | 検証、review、修正、受入の要否 |
| Validation が失敗した | 対象 Artifact の ValidationResult=failed | 修正、別モデル、追加調査の要否 |
| reviewer が修正を求めた | reviewer Attempt=Succeeded と ReviewVerdict=changes_requested | 修正、別 review、受入しない判断 |
| CI が失敗・観測不能 | 対象 PR / SHA の CI 観測値 | 修正、再実行、公開を続けない判断 |
| 外部処理が timeout・中断した | operation の timeout / interrupted / recovery_required | 状態を再観測し、再送・取消・次の操作の要否 |

## 用語と識別子

| 名前 | 意味 | 生成する主体 |
| --- | --- | --- |
| task_id | 利用者の目的を完了まで追う Task の ID | Rust |
| revision | Task の観測時点を表す連番 | Rust |
| request_id | 同じ副作用を再送しても重複させないための ID | 監督Codex |
| operation_id | 非同期処理の状態と結果を読むための ID | Rust |
| attempt_id | Provider / Model 呼び出し1回の記録 ID | Rust |
| artifact_id | 管理対象 workspace の特定時点の内容を表す ID | Rust |

request ID は Task、Attempt、operation とは別物である。副作用を持つ request ごとに、認証済み caller の範囲で一意にする。Task を変更する request は task ID と expected revision を必須とする。Task がまだない task.create だけは request ID を使い、Task を指定しない。

Task に属する操作受付、Attempt / Artifact / Validation / verdict / decision / publication / CI の記録、Task 状態変更は revision を進める。情報を読むだけの tool は revision を進めない。

## 操作一覧

| Tool | 目的 | 同期性 | 主な入力 | 主な出力 |
| --- | --- | --- | --- | --- |
| task.create | Issue または手入力から Task を作る | 同期 | request ID、source、要求本文、constraints、任意の Issue snapshot | task ID、revision、正規化した要求 |
| task.get_context | 次の判断に必要な事実を読む | 同期・読取 | task ID、sections、cursor | context snapshot、cursor |
| task.cancel | Task と未終了 operation の取消を要求する | 非同期受付 | 共通 request、取消理由 | operation ID、取消要求状態 |
| attempt.run | 指定 Provider / Model を1回実行する | 非同期受付 | 共通 request、provider、model、instruction、role、入力 Artifact または base | operation ID、Attempt ID、結果参照 |
| operation.get / operation.list_logs | operation の状態、結果、必要な log 範囲を読む | 同期・読取 | operation ID、stream、cursor、limit | 状態、結果、redacted log chunk |
| operation.cancel | 一つの operation の取消を要求する | 非同期受付 | 共通 request、operation ID | cancel operation、取消要求状態 |
| validation.run | Artifact に機械検証を実行する | 非同期受付 | 共通 request、artifact ID、check profile または checks | operation ID、ValidationResult 参照 |
| decision.record | 監督Codexの採否を記録する | 同期 | 共通 request、artifact ID、decision、理由、evidence | CodexDecision、revision |
| publication.publish | 指定 Artifact を PR として公開する | 非同期受付 | 共通 request、Artifact、base branch、evidence | operation ID、publication / PR 参照 |
| ci.get / ci.wait | CI を観測または期限まで待機する | 読取 / 非同期受付 | publication、PR、SHA、deadline | check 要約、観測時刻、operation ID |
| task.finish | 証拠を照合して Task 完了を要求する | 同期 | 共通 request、Artifact、accepted decision、evidence | completed Task、確定に使った evidence |

## 共通 request と response

すべての request と response は schema_version: v1 を持つ。未知の version は unsupported_schema_version として拒否する。任意 field の後方互換な追加は v1 のまま許すが、必須 field、enum、既存 field の意味を変えるときは新 version を作る。旧 Ledger の意味を新 version へ黙って読み替えない。

副作用を持つ request の共通部分は次である。

~~~json
{
  "schema_version": "v1",
  "request_id": "req_01J...",
  "task": {
    "task_id": "task_01J...",
    "expected_revision": 12
  }
}
~~~

受付に成功した非同期 operation は、外部処理の成功を予測せず、次の形で返す。

~~~json
{
  "schema_version": "v1",
  "request_id": "req_01J...",
  "task": { "task_id": "task_01J...", "revision": 13, "state": "active" },
  "operation": {
    "operation_id": "op_01J...",
    "kind": "attempt.run",
    "state": "accepted",
    "submitted_at": "2026-09-23T12:00:00Z"
  },
  "observation": { "attempt": { "attempt_id": "attempt_01J...", "state": "queued" } }
}
~~~

同期 tool は同じ schema version、Task、現在 revision、観測値を返す。副作用 request の response は対応する request ID を必ず返す。読取 request が caller request ID を持つ場合も同じ値を返してよい。

### 再送と stale request

Rust は caller、tool 名、正規化した payload を含む request fingerprint と最初の response を保存する。

- 同じ caller、tool、request ID、同一 payload の再送では、保存済み response を返し副作用を繰り返さない。
- 同じ caller、tool、request ID で payload が異なる場合は idempotency_conflict として拒否する。
- expected revision が現在値と違う場合は stale_revision として拒否する。現在 revision と context の再取得先を返し、外部副作用を一切始めない。
- Service は revision、policy、Task 所属、Artifact 所属、排他を外部副作用の前に確認する。busy、未知 Provider / Model、policy 違反も同じ時点で拒否する。

## context と log の読み方

task.get_context は Task 要求と revision、Task state、利用可能な Provider / Model、availability、quota / reset、configured budget、usage、残予算、過去実績、直近 Attempt / operation、Artifact / diff 参照、Validation、review verdict、CodexDecision、PR / CI 要約を section ごとに返せる。

全履歴・全 raw log を既定で返さない。列挙する section は items と next cursor を持ち、cursor は同じ Task・section・snapshot 範囲でだけ使える。不正または期限切れなら invalid_cursor を返し、先頭から取り直す。operation.list_logs も stdout、stderr、diagnostic を指定し、cursor と limit で必要な範囲だけを返す。

観測不能、未集計、比較不能な値は unknown、または理由を持つ null とする。0、空、passed、利用可能へ変換してはならない。旧 Ledger の Attempt を返す場合は state semantics version を含め、旧 Succeeded が validation-coupled な意味であることを区別する。

## 成果物を使う操作の規則

attempt.run は provider、model、instruction、role を構造化 field で受け取る。Provider CLI 引数や監督Codexの会話状態を暗黙に継承しない。初回実行は repository と commit を明示した base から始め、修正は同じ Task 所属で利用可能な input artifact ID を指定する。Service は指定された Provider / Model を代替選定しない。

review は role=reviewer の通常の Attempt として実行する。Service は reviewer output が review schema に合う場合だけ、reviewer Attempt と対象 Artifact を結ぶ ReviewVerdict を観測事実として記録する。監督Codexは reviewer verdict を直接作成・上書きできない。

validation.run は artifact ID を必須とし、Validation を Attempt の状態遷移として扱わない。reviewer の approved と decision.record の accepted は別物である。decision.record は監督Codexの明示的な採否を記録するだけで、公開や完了を自動実行しない。

publication.publish は、指定 Artifact と policy が要求する Validation、CodexDecision、必要なら review が同じ Artifact を対象にしていることを検証してから始める。成果物が変われば、古い Validation、review、受入を新しい成果物の証拠に使えない。自動 merge はこの tool に含めない。

task.finish は、単一 Attempt や単一 Validation の成功だけでは Task を完了できない。指定 Artifact に対する accepted decision と、policy が要求する Validation、公開、CI の evidence を照合できた場合だけ completed に遷移する。監督Codex自身が workspace を編集した場合は Attempt を作らず、管理済み Artifact に対して検証、受入、公開、CI、完了を要求できる。

## 非同期、取消、失敗、回復

外部 Provider、Validator、GitHub 操作、CI 待機は、受付を永続化してから始める非同期 operation である。operation state は accepted、running、completed、failed、cancelling、cancelled、recovery_required のいずれかである。これは Task / Attempt の Domain state を置き換える固定 workflow phase ではない。

operation.cancel は一つの operation の停止を、task.cancel は Task と未終了 operation 全体の停止を要求する。どちらも即座に停止成功を意味しない。Service は停止を試み、その結果を記録し、停止を確認した後にだけ cancelled を返す。Task は全対象の停止を確認した後にだけ cancelled へ遷移する。一部が停止不能・観測不能なら Task を終端状態へ進めず recovery_required を返す。

timeout は policy に従って停止を試みた観測事実であり、Task を自動的に failed にしない。プロセス終了を確認できない、永続化後に結果を照合できない、外部副作用の完了可否を復元できない場合も recovery_required とする。成功、取消、未実行とは推測しない。

error response は次の形で返す。retryable は同じ request を再送してよい意味ではなく、監督Codexが context を再観測して次の操作を判断できることを示す。

~~~json
{
  "schema_version": "v1",
  "request_id": "req_01J...",
  "error": {
    "code": "stale_revision",
    "message": "task revision 12 is no longer current",
    "retryable": true,
    "current_task_revision": 13,
    "operation_id": null,
    "details_ref": null
  }
}
~~~

少なくとも invalid_request、unsupported_schema_version、task_not_found、stale_revision、idempotency_conflict、busy、unknown_provider、unknown_model、policy_denied、budget_exhausted、workspace_boundary_violation、artifact_not_found、artifact_task_mismatch、evidence_artifact_mismatch、invalid_state_transition、operation_not_found、not_cancellable、timeout、cancelled、interrupted、recovery_required、invalid_cursor、forbidden、internal_error を型付き code とする。Provider、Validation、review、CI の失敗は operation result と diagnostic reference に記録する。

## secret と log の境界

Service は secret、認証 header / token、環境変数値、Provider の raw credential output を context や log response に返してはならない。既知 secret は永続化前と返却前に redact し、log chunk には redacted=true と理由を残す。安全に redact できない raw log は、権限付き保存先への参照だけを残し、MCP で本文を返さない。

context の既定値は要約、ID、時刻、状態、usage / budget の観測値、Artifact / diagnostic の参照である。full diff、長い diagnostic、raw log は scoped tool と cursor を明示したときだけ取得する。権限不足や秘匿境界により観測できない値は unknown または forbidden とし、空や成功として扱わない。

## #45 実装時の適合条件

#45 は各 MCP tool の input schema と output schema を、本書の tool 名、field、enum、error code に一致させる。Gateway は request ID の生成、Provider / Model 選定、成果物受入、Task 完了を推測してはならない。JSON-RPC error は transport または schema 不正に限定し、Service が処理した業務上の拒否・失敗は型付き error response として返す。

少なくとも、同一 request の再送、stale revision、同一 request ID の payload 相違、取消、timeout、recovery、unknown 観測値、Artifact evidence の不一致、redaction を検証する。

## 対象外

MCP server、Operation Service、SQLite migration、Provider 固有 CLI、AI review Provider、workspace の再利用・cleanup、並列 workflow engine、自動 merge の実装は後続 Issue で扱う。
