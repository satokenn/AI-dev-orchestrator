# 監督Codex向け MCP Operation Contract

この文書は、監督Codexが Rust の Operation Service を MCP 経由で操作する wire contract を定義する。#45 の stdio MCP Gateway はこの文書を実装仕様として使用する。Rust型、SQLite schema、MCP transport の実装、Provider 固有 CLI、自動 merge はこの文書の対象外である。

主経路は、監督Codexが要求・Provider / Model・再試行・review・成果物の採否を判断し、Operation Service が検証、実行、制約強制、永続化、観測結果の返却を担う反復である。Rust 内部の `CodexPlanner` を呼ぶことは、この契約の前提にも fallback にも含めない。worker として起動する Codex CLI は、監督Codexとは別の Provider / Model 実行である。

`Task`、`Attempt`、`Artifact`、Validation、review verdict、`CodexDecision` の意味は[ドメインモデル](domain-model.md)を正本とする。特に Attempt の `Succeeded` は Provider 呼び出しの正常終了だけを表す。Validation 成功、review の `approved`、監督Codexの `accepted`、公開、Task 完了は別の事実である。

## 共通 envelope と識別子

全 request と response は `schema_version: "v1"` を持つ。未知の必須 version は `unsupported_schema_version` として拒否し、旧 version の意味を新しい version へ黙って読み替えない。v1 に対する後方互換な任意 field の追加は許すが、必須 field・enum・既存 field の意味を変更するときは新 version を作る。

副作用を持つ request は、呼出し側が生成する不透明な `request_id` を必須とする。Operation Service は、認証済み caller、tool 名、正規化した payload を含む request fingerprint と、最初の response を永続化する。

- 同じ caller、tool、`request_id`、同一 payload の再送は、既存の response を返し副作用を再実行しない。
- 同じ caller、tool、`request_id` と異なる payload の再送は `idempotency_conflict` として拒否する。
- `request_id` は Task ID、Attempt ID、operation ID とは別物である。Task 内でだけでなく caller ごとに一意でなければならない。
- 既存 Task を変更する request は、対象 Task の `task_id` と、観測した `expected_revision` を必須とする。Task 作成は Task がまだないため `request_id` のみを使う。
- Service は revision と policy、Task 所属、Artifact 所属、排他を**外部副作用の前**に検証する。revision 不一致では `stale_revision` を返し、現在の revision と短い再観測指示を返す。部分的に実行しない。

Task revision は、Task に属する操作受付、Attempt / Artifact / Validation / verdict / decision / publication / CI の記録、または Task 状態を変えるたびに増える。観測だけの request は revision を増やさない。`operation_id` は副作用を伴う受付ごとに Service が生成する不透明 ID であり、実行の状態と結果を取得・cancel するために使う。Provider 呼び出しを受け付けた operation は、作成した `attempt_id` を返す。

```json
{
  "schema_version": "v1",
  "request_id": "req_01J...",
  "task": { "task_id": "task_01J...", "expected_revision": 12 }
}
```

成功 response の共通形は次である。副作用 request の response は対応する `request_id` を必ず返す。読取 request が caller request ID を持つ場合も同じ値を返してよい。`observation` は Service が観測した事実だけを返し、監督Codexの次の判断を代行しない。

```json
{
  "schema_version": "v1",
  "request_id": "req_01J...",
  "task": { "task_id": "task_01J...", "revision": 13, "state": "active" },
  "operation": {
    "operation_id": "op_01J...",
    "kind": "attempt.run",
    "state": "accepted",
    "submitted_at": "2026-09-21T12:00:00Z"
  },
  "observation": { "attempt": { "attempt_id": "attempt_01J...", "state": "queued" } }
}
```

## 読み取りと context

`task.get_context` は、監督Codexが次の操作を判断するための snapshot を返す同期 tool である。入力は `task_id`、必要な section（`task`、`providers`、`usage`、`attempts`、`artifacts`、`validations`、`reviews`、`decisions`、`publication`、`ci`）、各 section の page size、任意の cursor である。全 raw log や全履歴を既定で返さない。

response は Task 要求と revision、Task state、利用可能な Provider / Model と availability、quota / reset を含む観測値、configured budget と usage / remaining budget、Provider / Model の過去実績、直近 Attempt / operation、Artifact と diff の参照、Validation、review verdict、CodexDecision、PR / CI の要約を含められる。値が観測不能・未集計・比較不能なら `"unknown"`、又は理由を持つ `null` を返す。0、空、`passed`、利用可能へ変換してはならない。

各列挙 section は `{ "items": [...], "next_cursor": "..." }` を返す。cursor は同一 Task・section・snapshot 範囲にだけ使える。cursor 不正・期限切れでは `invalid_cursor` を返し、先頭からの取得を要求する。response には `observed_at` と、旧 Ledger 記録を含む場合の `state_semantics_version` を含めるため、`Succeeded` の旧来の validation-coupled な意味を v1 の Provider 成功と混同しない。

`operation.get` は `operation_id` の受付、開始、終了、取消、error、関連する Attempt / Artifact / Validation / publication / CI 参照を同期で返す。`operation.list_logs` は `operation_id`、`cursor`、`limit`、`stream`（`stdout`、`stderr`、`diagnostic`）で必要な範囲だけを返す。どちらも Task revision を変更しない。

## 操作 tools

次の tool 名、入力、出力を v1 の最小集合とする。`operation` を返す tool は、完了を待たずに永続化済みの受付結果を返す。短時間で完了済みの場合も同じ形で `state: "completed"` と結果参照を返してよい。監督Codexは `operation.get`、必要なら `operation.list_logs` 又は `task.get_context` を使って次の操作を明示的に判断する。

| Tool | 同期性 | request の主要 field | response の主要 field |
| --- | --- | --- | --- |
| `task.create` | 同期 | `request_id`、`source`（`issue` または `manual`）、要求本文、constraints、任意の Issue URL / snapshot | 新しい `task_id`、revision、正規化した要求 snapshot |
| `task.get_context` | 同期・読取 | `task_id`、sections、cursor | context snapshot、cursor |
| `task.cancel` | 非同期受付 | 共通 envelope、取消理由 | `operation_id`、Task と未終了 operation の取消要求状態 |
| `attempt.run` | 非同期受付 | 共通 envelope、`provider`、`model`、`instruction`、`role`、`input_artifact_id` 又は明示的 base、timeout / budget 内の実行設定 | `operation_id`、`attempt_id`、queued / terminal 状態、出力 Artifact 参照（確定後） |
| `operation.get` | 同期・読取 | `operation_id` | operation 状態、結果又は error、関連参照 |
| `operation.list_logs` | 同期・読取 | `operation_id`、stream、cursor、limit | redacted log chunk、cursor |
| `operation.cancel` | 非同期受付 | 共通 envelope、`operation_id` | cancel operation、対象 operation の取消要求状態 |
| `validation.run` | 非同期受付 | 共通 envelope、`artifact_id`、check profile 又は明示的 checks | `operation_id`、対象 Artifact、ValidationResult 参照 |
| `decision.record` | 同期 | 共通 envelope、`artifact_id`、`decision`、理由、参照 evidence | 保存済み CodexDecision と revision |
| `publication.publish` | 非同期受付 | 共通 envelope、`artifact_id`、base branch、必要な validation / decision / review / policy evidence 参照 | `operation_id`、publication / PR 参照（確定後） |
| `ci.get` | 同期・読取 | `publication_id` 又は PR / SHA 参照 | check 要約、`observed_at`、unknown を含む状態 |
| `ci.wait` | 非同期受付 | 共通 envelope、`publication_id` 又は PR / SHA、wait deadline | `operation_id`、CI 観測結果又は timeout 参照 |
| `task.finish` | 同期 | 共通 envelope、`artifact_id`、受入 decision、必要な validation / publication / CI evidence 参照 | `completed` Task と確定に用いた evidence |

`attempt.run` の `provider`、`model`、`instruction` は構造化 field であり、Provider CLI 引数や監督Codexの会話状態を暗黙に継承しない。`input_artifact_id` を指定したときは同じ Task 所属で利用可能な Artifact だけを許す。初回実行の base は repository / commit を明示する。Service は指定された Provider / Model の availability、policy、budget、workspace 境界、Task 排他を検証し、Provider / Model を代替選定しない。review も `role: "reviewer"` の通常の Attempt として実行する。Service は reviewer output を review schema に照合できた場合だけ、対象 Artifact と reviewer Attempt を結ぶ `ReviewVerdict` を観測事実として記録する。監督Codexは reviewer verdict を直接作成・上書きできない。

`validation.run` は Artifact を必須とし、Validation を Attempt の状態遷移として扱わない。reviewer の `approved` と `decision.record` の `accepted` は別物である。`decision.record` は監督Codexの明示的な採否だけを永続化し、公開や完了を自動実行しない。

`publication.publish` は指定 Artifact と、policy が要求する Validation、CodexDecision、必要なら review の対象 Artifact が一致することを確認してから副作用を開始する。自動 merge はこの tool の結果にも policy にも含めない。`task.finish` は単一 Attempt や単一 Validation の成功を根拠にできず、指定 Artifact に対する `accepted` decision と policy が要求する evidence を照合してのみ `completed` に遷移できる。監督Codex自身が workspace を編集した場合は Attempt を作らず、管理済み Artifact に対して Validation、decision、publication、CI、`task.finish` を要求できる。

## 非同期、取消、失敗、回復

外部 Provider、Validator、GitHub 操作、CI 待機は、受付を durable に保存してから開始する非同期 operation である。受付 response は成功・失敗を予測しない。operation state は `accepted`、`running`、`completed`、`failed`、`cancelling`、`cancelled`、`recovery_required` のいずれかであり、Task / Attempt の Domain state を置き換えない固定 workflow phase ではない。

`operation.cancel` は取消を要求するだけで、即座に停止成功を意味しない。Service は対象が cancel 可能かを検証し、停止を試み、その結果を operation と必要なら Attempt に記録する。停止確認後にだけ `cancelled` を返す。timeout は Service が policy に従って停止を試みた観測事実であり、`timeout` response は Task を自動的に `failed` にしない。プロセス終了を確認できない、永続化後に結果を照合できない、外部副作用の完了可否を復元できない場合は `recovery_required` とし、成功・取消・未実行と推測しない。

`task.cancel` は Task と同じ Task に属する未終了 operation の取消を要求する。全対象の停止を確認した後にだけ Task は `cancelled` へ遷移する。一部が停止不能・観測不能なら Task を終端状態へ進めず `recovery_required` を返すため、Task / Attempt の状態遷移を MCP tool が迂回できない。

error response は次の形で返す。`retryable` は同じ request を再送してよいことではなく、監督Codexが context を再観測して次の操作を判断できることを示す。

```json
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
```

少なくとも `invalid_request`、`unsupported_schema_version`、`task_not_found`、`stale_revision`、`idempotency_conflict`、`busy`、`unknown_provider`、`unknown_model`、`policy_denied`、`budget_exhausted`、`workspace_boundary_violation`、`artifact_not_found`、`artifact_task_mismatch`、`evidence_artifact_mismatch`、`invalid_state_transition`、`operation_not_found`、`not_cancellable`、`timeout`、`cancelled`、`interrupted`、`recovery_required`、`invalid_cursor`、`forbidden`、`internal_error` を型付き code とする。Provider、Validation、review、CI の失敗は operation result と diagnostic reference に記録し、次の retry / rework / review / publish / finish を自動選択しない。

## 秘匿情報とログの境界

Service は secret、認証 header / token、環境変数値、Provider の raw credential output を context や log response に返してはならない。永続化前と返却前に既知 secret を redaction し、log chunk には `redacted: true` と理由を残す。安全に redact できない raw log は保存先への権限付き参照だけを残し、MCP で本文を返さない。

`task.get_context` は要約、ID、時刻、状態、usage / budget の観測値、Artifact / diagnostic の参照を既定とする。full diff、長い diagnostic、raw logs は明示した scoped tool と cursor でのみ取得する。権限不足または秘匿境界により観測できない値は `unknown` / `forbidden` として返し、空や成功として扱わない。

## #45 実装時の適合条件

#45 は各 MCP tool の input schema と output schema を本書の tool 名・field・enum・error code に一致させる。Gateway は request ID の生成や Provider / Model 選定、成果物受入、Task 完了を推測してはならない。MCP の JSON-RPC error は transport / schema 不正に限定し、Service が処理した業務上の拒否・失敗は上記の型付き error response として返す。実装は、同一 request の再送、stale revision、同一 request ID の payload 相違、取消 / timeout / recovery、unknown 観測値、Artifact evidence の不一致、redaction を検証する。
