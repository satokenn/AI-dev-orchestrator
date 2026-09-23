# MCP 操作契約 — wire reference

[概要](mcp-operation-contract.md)にある各toolの入力と出力を定める。toolの入力はMCP `tools/call` の `arguments`、成功時の構造化出力は `CallToolResult.structuredContent` に対応する。この文書はJSON-RPC/MCPの外側のenvelopeを再定義しない。

toolのschema記述はJSON Schema 2020-12のobject、`properties`、`required`、`type`、`enum`に対応する。表の`Required`欄はfieldの省略可否を表し、`null`可否とは別である。`string | null` はfieldが必須で値にnullを許す。`Optional`はfield自体を省略できる。説明文や例の日本語はschema値ではない。

MCP toolsでは`inputSchema`が入力schemaを定め、`outputSchema`は構造化出力のschemaとして指定できる。[MCP Tools仕様](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)と[JSON Schema objectのrequired properties](https://json-schema.org/understanding-json-schema/reference/object#required-properties)に従い、#45はこの文書の型・必須field・enumと整合するschema validationを実装する。

## 型と共通規則

| 表記 | JSON型・制約 |
| --- | --- |
| `string` | JSON string。特記があれば`enum`、`format`、最大長等を適用 |
| `integer` | 整数値。revision、page size、連番等に使う |
| `number` | 数値。usage等に使う |
| `boolean` | `true`または`false` |
| `object` | JSON object。fieldの型・必須性は対応する表で定義 |
| `array<T>` | T型の要素を持つJSON array |
| `T | null` | T型またはJSON `null`。field自体は省略できない |
| `enum(a, b)` | 記載したstring値のみ許可 |

すべてのtool requestは`schema_version: "v1"`を必須とする。未対応versionは`unsupported_schema_version`。不明なrequest fieldは`invalid_request`とし、副作用前に拒否する。成功するtool outputはすべて`schema_version: "v1"`を含む。MCP側のtool実行errorは`CallToolResult.isError: true`とし、本文に下記のtyped errorを含める。

副作用を伴うrequestは`request_id: string`を必須とする。既存Taskを変更する場合はさらに`task_id: string`と`expected_revision: integer`を必須とする。読取requestは`request_id`と`expected_revision`を持たない。

冪等性keyの範囲は`caller + tool name + request_id`。`task.create`を含む全副作用requestに適用する。同じkey・同じnormalized payloadの再送は保存済みresponseを返し、副作用を繰り返さない。同じkeyでpayloadが異なる場合は`idempotency_conflict`。

IDはすべて不透明なstringとし、呼出側はIDの形式・連番・内部構造を解釈しない。RFC 3339日時は`string`として送受信し、UTC (`Z`) を使う。fieldがoptionalなら省略し、nullを使うのは型に`| null`と明記された場合だけ。

## 共通型

### `TaskRequest` / `TaskSnapshot`

| Type | Field | Type | Required |
| --- | --- | --- | --- |
| `TaskRequest` | `source` | `enum(issue, manual)` | 必須 |
|  | `title` | `string` | 必須 |
|  | `description` | `string` | 必須 |
|  | `constraints` | `array<string>` | 必須 |
|  | `issue` | `IssueSnapshot | null` | 必須。`source=issue`ならobject、`source=manual`ならnull |
| `TaskSnapshot` | `task_id` | `string` | 必須 |
|  | `revision` | `integer` | 必須 |
|  | `state` | `enum(pending, active, completed, failed, cancelled)` | 必須 |
|  | `request` | `TaskRequest` | 必須 |

`IssueSnapshot`は次のobjectとする。

| Field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `url` | `string (format: uri)` | 必須 | Issue URL |
| `number` | `integer (minimum: 1)` | 必須 | Issue番号 |
| `title` | `string` | 必須 | 取得時点のタイトル |
| `body` | `string` | 必須 | 取得時点の本文 |

### `EvidenceRef`

| Field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `kind` | `enum(validation, review, decision, publication, ci)` | 必須 | 参照する記録の種別 |
| `id` | `string` | 必須 | Serviceが発行した記録ID |

evidenceは結果を申告するfieldではなく、保存済みrecordへの参照である。Serviceは存在、Task所属、対象Artifactを照合する。CI evidenceは対象Publicationのrepository、PR、head SHAが一致しなければならない。存在しないIDは`invalid_request`、別Artifactに属する記録は`evidence_artifact_mismatch`。

### `ContextItem`

各`task.get_context` sectionは次の共通itemを返す。`details`の型はsectionごとの表で定義する。

| Field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `id` | `string` | 必須 | 履歴record ID |
| `kind` | `string` | 必須 | record種別 |
| `state` | `string | null` | 必須 | record種別に定義されたstate。stateを持たないrecordはnull |
| `occurred_at` | `string (format: date-time)` | 必須 | record時刻（RFC 3339 UTC） |
| `summary` | `string` | 必須 | 人が読める短い要約 |
| `references` | `array<Reference>` | 必須 | 関連record |
| `details` | `object` | 必須 | section固有の追加情報 |

| Section | `kind` | `details` field（すべて必須） |
| --- | --- | --- |
| `providers` | `provider` | `provider_id: string`; `model_ids: array<string>`; `availability: enum(available, unavailable, unknown)`; `observed_at: string (format: date-time)`; `diagnostic_ref: string | null` |
| `usage` | `usage` | `name: string`; `value: number | null`; `unit: string`; `basis: enum(measured, configured, computed, estimated, unknown)`; `observed_at: string | null` (nonnull値は`format: date-time`) |
| `attempts` | `attempt` | `provider_id: string`; `model_id: string`; `role: enum(implementer, reviewer, explorer)`; `input_artifact_id: string | null`; `base_commit: string | null`; `output_artifact_id: string | null`; `diagnostic_ref: string | null` |
| `artifacts` | `artifact` | `artifact_id: string`; `digest: string`; `source_attempt_id: string | null`; `base_commit: string | null`; `diff_ref: string | null` |
| `validations` | `validation` | `validation_id: string`; `artifact_id: string`; `check_profile_id: string | null`; `checks: array<ValidationCheckResult>` |
| `reviews` | `review_verdict` | `review_verdict_id: string`; `reviewer_attempt_id: string`; `artifact_id: string`; `verdict: enum(approved, changes_requested, inconclusive)` |
| `decisions` | `codex_decision` | `decision_id: string`; `artifact_id: string`; `decision: enum(accepted, rejected, changes_requested)`; `reason: string`; `evidence: array<EvidenceRef>` |
| `publication` | `publication` | `publication_id: string`; `artifact_id: string`; `repository: string`; `head_sha: string`; `pull_request_number: integer`; `pull_request_url: string (format: uri)` |
| `ci` | `ci_observation` | `observation_id: string`; `repository: string`; `pull_request_number: integer | null`; `head_sha: string`; `state: enum(pending, passed, failed, unknown)`; `checks: array<CiCheck>` |

`ContextItem.state`の値は次のとおり。sectionごとに別のenumであり、一覧にないstateを使わない。

| Section | `state` type |
| --- | --- |
| `providers` | `enum(available, unavailable, unknown)` |
| `usage` | `null` |
| `attempts` | `enum(queued, running, succeeded, failed, cancelled)` |
| `artifacts` | `null` |
| `validations` | `enum(passed, failed, unknown)` |
| `reviews` | `enum(approved, changes_requested, inconclusive)` |
| `decisions` | `enum(accepted, rejected, changes_requested)` |
| `publication` | `null` |
| `ci` | `enum(pending, passed, failed, unknown)` |

次のobject型を使用する。記載したfieldはすべて必須。

| Type | Field | Type | Meaning |
| --- | --- | --- | --- |
| `Reference` | `kind` | `string` | 参照先recordの種別 |
|  | `id` | `string` | 参照先record ID |
| `ValidationCheckResult` | `name` | `string` | check名 |
|  | `state` | `enum(passed, failed, unknown)` | check結果 |
|  | `diagnostic_ref` | `string | null` | 診断参照。なければnull |
| `CiCheck` | `name` | `string` | CI check名 |
|  | `state` | `enum(pending, passed, failed, unknown)` | 観測した状態 |
|  | `url` | `string | null` | check URL。なければnull |
|  | `completed_at` | `string | null` | 完了時刻。非nullならRFC 3339 UTC |

### `ContextPage`

| Field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `items` | `array<ContextItem>` | 必須 | sectionに属する履歴record。section固有の`details`型を使う |
| `next_cursor` | `string | null` | 必須 | 次page取得用token。nullは現在のsnapshotに続きがない |

cursorは不透明であり、Task・section・page size・snapshot revisionに束縛される。次pageでは同じpage sizeと、同じsectionのcursorを使う。revision変更等で利用できないcursorは`invalid_cursor`。履歴は`occurred_at`降順、同時刻ならID降順。page境界に重複・欠落を作らない。

### `OperationAcceptance`

非同期toolが受け付けられたときの共通structured output。

| Field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 対応するrequest ID |
| `task_id` | `string` | 必須 | 対象Task ID |
| `revision` | `integer` | 必須 | 受付後のTask revision |
| `operation` | `OperationRef` | 必須 | 新規operation |
| `attempt_id` | `string` | Attemptを作るtoolで必須 | 作成したAttempt ID |

`OperationRef`のfieldはすべて必須。

| Field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `operation_id` | `string` | 必須 | operation ID |
| `kind` | `enum(attempt.run, task.cancel, operation.cancel, validation.run, publication.publish, ci.wait)` | 必須 | 受付対象の操作 |
| `state` | `enum(accepted, running, completed, failed, cancelling, cancelled, recovery_required)` | 必須 | operation状態 |
| `submitted_at` | `string (format: date-time)` | 必須 | 受付時刻 |

受付は処理成功を意味しない。最終結果は`operation.get`で取得する。

### `ErrorPayload`

| Field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `request_id` | `string | null` | 必須 | parse可能なら対象request ID。取得不能ならnull |
| `error` | `Error` | 必須 | 業務error |

`Error`の次のfieldはすべて必須。

| Field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `code` | `ErrorCode` | 必須 | 下記の業務error code |
| `message` | `string` | 必須 | 人が読める説明 |
| `retryable` | `boolean` | 必須 | 同じrequestを再送してよいかではなく、新しいrequestを作成して回復可能か |
| `current_task_revision` | `integer | null` | 必須 | stale revision error時の現在値。それ以外はnull |
| `operation_id` | `string | null` | 必須 | 関連operation。なければnull |
| `details_ref` | `string | null` | 必須 | 追加診断参照。なければnull |

`ErrorCode`は`invalid_request`、`unsupported_schema_version`、`task_not_found`、`stale_revision`、`idempotency_conflict`、`busy`、`unknown_provider`、`unknown_model`、`policy_denied`、`budget_exhausted`、`workspace_boundary_violation`、`artifact_not_found`、`artifact_task_mismatch`、`evidence_artifact_mismatch`、`invalid_state_transition`、`operation_not_found`、`not_cancellable`、`timeout`、`cancelled`、`interrupted`、`recovery_required`、`invalid_cursor`、`forbidden`、`internal_error`のいずれか。

`interrupted`はerror codeでありoperation stateではない。終了結果を確定できないinterrupted operationは`recovery_required`になり、復旧状態が確定するまで同じoperationのretryで副作用を再実行しない。再起動後もServiceはこの状態を保持し、`operation.get`で確認できる。復旧確認前の`operation.cancel`は`not_cancellable`で拒否する。

## Tool schemas

各request / response表はJSON Schemaの`properties`に相当するfieldを列挙する。`Required`は「必須」「任意」「条件付き」のいずれか。条件付きfieldの条件は表の直後に記す。未知request fieldは拒否し、未知response fieldは無視する。全成功outputには`schema_version: const "v1"`を含む。

### `task.create`

Taskを作成する。`request_id`は冪等性keyに含まれる。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 重複作成を防ぐrequest ID |
| `source` | `enum(issue, manual)` | 必須 | 要求の出所 |
| `title` | `string` | 必須 | Task title |
| `description` | `string` | 必須 | 要求本文 |
| `constraints` | `array<string>` | 必須 | 要求制約。なしなら空配列 |
| `issue` | `IssueSnapshot` | 条件付き | sourceが`issue`なら必須、`manual`なら省略 |

`IssueSnapshot`は`url: string (format: uri)`、`number: integer (minimum: 1)`、`title: string`、`body: string`をすべて必須とする。issue title/bodyは作成時点のsnapshot。

成功output:

| Field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 入力request IDのecho |
| `task_id` | `string` | 必須 | 新規Task ID |
| `revision` | `integer` | 必須 | 初期revision |
| `state` | `const "pending"` | 必須 | 初期Task state |
| `request` | `TaskRequest` | 必須 | 保存したsource・要求・制約・Issue snapshot |

例（tool `arguments`）:

~~~json
{
  "schema_version": "v1",
  "request_id": "req-01",
  "source": "manual",
  "title": "Update parser",
  "description": "Support escaped delimiters",
  "constraints": []
}
~~~

同じ`request_id`・同じ入力は同じ作成結果を返す。payload違いの再利用は`idempotency_conflict`。

### `task.get_context`

Taskと選択したsectionのsnapshotを読む。読取専用。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `task_id` | `string` | 必須 | 読むTask |
| `sections` | `array<enum(providers, usage, attempts, artifacts, validations, reviews, decisions, publication, ci)>` | 必須 | 返す履歴section。重複不可 |
| `page_size` | `integer (minimum: 1, maximum: 100)` | 任意、既定20 | 各sectionから返す最大件数 |
| `cursors` | `object<string, string>` | 任意 | section名からそのsectionの次page cursorへのmap |

成功output:

| Field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `task` | `TaskSnapshot` | 必須 | ID、revision、state、要求snapshot |
| `sections` | `object<string, ContextPage>` | 必須 | 要求されたsectionごとのpage |
| `observed_at` | `string (format: date-time)` | 必須 | snapshot観測時刻 |

例（`sections.attempts.items`の一部）:

~~~json
{
  "id": "attempt-01",
  "kind": "attempt",
  "state": "succeeded",
  "occurred_at": "2026-09-23T01:02:03Z",
  "summary": "Provider call completed",
  "references": [{"kind": "artifact", "id": "artifact-01"}],
  "details": {"provider_id": "provider-a", "model_id": "model-a", "role": "implementer", "input_artifact_id": null, "base_commit": "abc123", "output_artifact_id": "artifact-01", "diagnostic_ref": null}
}
~~~

### `attempt.run`

指定Provider / Modelを一回実行する。長時間処理。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `provider_id` | `string` | 必須 | 呼び出すProvider |
| `model_id` | `string` | 必須 | 呼び出すModel |
| `instruction` | `string` | 必須 | Providerへ渡す依頼 |
| `role` | `enum(implementer, reviewer, explorer)` | 必須 | Attemptのrole |
| `input` | `ArtifactInput | BaseInput` | 必須 | 入力Artifactまたは初期baseのどちらか一方 |
| `timeout_ms` | `integer (minimum: 1)` | 任意 | timeout。省略時はService policy値 |

`ArtifactInput`は`artifact_id: string`のみ、`BaseInput`は`repository: string`と`commit: string`を持つobject。入力Artifactは同じTaskに属する必要がある。branch/pathだけのbase指定は認めない。

成功outputは`OperationAcceptance`。`attempt_id`を必須とする。受付時の`operation.state`は`accepted`。

### `operation.get`

operation状態・結果を読む。読取専用。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `operation_id` | `string` | 必須 | 取得対象 |

成功output:

| Field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `operation` | `Operation` | 必須 | operation記録 |

`Operation`のfieldはすべて必須。nullを許すfieldも省略せず、該当しない場合はnullを返す。

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `operation_id` | `string` | 必須 | operation ID |
| `task_id` | `string` | 必須 | 対象Task ID |
| `kind` | `enum(attempt.run, task.cancel, operation.cancel, validation.run, publication.publish, ci.wait)` | 必須 | 実行したtool |
| `state` | `enum(accepted, running, completed, failed, cancelling, cancelled, recovery_required)` | 必須 | operation状態 |
| `submitted_at` | `string (format: date-time)` | 必須 | 受付時刻 |
| `started_at` | `string | null` | 必須 | 開始時刻 |
| `completed_at` | `string | null` | 必須 | 終了時刻 |
| `result` | `OperationResult | null` | 必須 | 確定した結果 |
| `error` | `Error | null` | 必須 | 失敗情報 |

`result`と`error`は同時に非nullにならない。`OperationResult`は`kind`に対応する次のobjectのいずれか。

| Operation kind | Result field | Type | Required |
| --- | --- | --- | --- |
| `attempt.run` | `attempt_id` | `string` | 必須 |
|  | `attempt_state` | `enum(succeeded, failed, cancelled)` | 必須 |
|  | `output_artifact_id` | `string | null` | 必須 |
|  | `usage` | `array<UsageMetric>` | 必須 |
|  | `diagnostic_ref` | `string | null` | 必須 |
| `task.cancel` | `task_state` | `enum(cancelled, active)` | 必須 |
|  | `cancelled_operation_ids` | `array<string>` | 必須 |
| `operation.cancel` | `target_operation_id` | `string` | 必須 |
|  | `target_state` | `enum(cancelled, running, recovery_required)` | 必須 |
| `validation.run` | `validation_id` | `string` | 必須 |
|  | `artifact_id` | `string` | 必須 |
|  | `state` | `enum(passed, failed, unknown)` | 必須 |
|  | `checks` | `array<ValidationCheckResult>` | 必須 |
| `publication.publish` | `publication_id` | `string` | 必須 |
|  | `repository` | `string` | 必須 |
|  | `head_sha` | `string` | 必須 |
|  | `pull_request_number` | `integer` | 必須 |
|  | `pull_request_url` | `string (format: uri)` | 必須 |
| `ci.wait` | `observation_id` | `string` | 必須 |
|  | `target` | `CiTarget` | 必須 |
|  | `observed_at` | `string (format: date-time)` | 必須 |
|  | `state` | `enum(pending, passed, failed, unknown)` | 必須 |
|  | `checks` | `array<CiCheck>` | 必須 |

`UsageMetric`と`CiTarget`のfieldはすべて必須。

| Type | Field | Type | Meaning |
| --- | --- | --- | --- |
| `UsageMetric` | `name` | `string` | 指標名 |
|  | `value` | `number | null` | 値。不明ならnull |
|  | `unit` | `string` | 単位 |
|  | `basis` | `enum(measured, configured, computed, estimated, unknown)` | 値の根拠 |
| `CiTarget` | `repository` | `string` | repository |
|  | `pull_request_number` | `integer | null` | PR番号。commit targetならnull |
|  | `head_sha` | `string` | 観測対象SHA |

### `operation.list_logs`

指定operationのログ範囲を読む。ログ本文はredacted済み。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `operation_id` | `string` | 必須 | 対象operation |
| `stream` | `enum(stdout, stderr, diagnostic)` | 必須 | 読むstream |
| `cursor` | `string` | 任意 | 次page cursor |
| `limit` | `integer (minimum: 1, maximum: 1000)` | 任意、既定100 | 最大chunk件数 |

成功outputは次のfieldをすべて含む。`LogChunk`のfieldもすべて必須。

| Field | Type | Required |
| --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 |
| `chunks` | `array<LogChunk>` | 必須 |
| `next_cursor` | `string | null` | 必須 |

| LogChunk field | Type | Required |
| --- | --- | --- |
| `sequence` | `integer` | 必須 |
| `occurred_at` | `string (format: date-time)` | 必須 |
| `text` | `string` | 必須 |
| `redacted` | `boolean` | 必須 |

cursorはoperation・stream・limitに束縛される。末尾到達はoperation完了を意味しない。

### `operation.cancel`

ひとつのoperationへの取消を要求する。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 対象operationのTask |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `operation_id` | `string` | 必須 | 取消対象 |

成功outputは`OperationAcceptance`。`operation`は取消要求を表す新operation。対象operationが停止するまでは取消完了ではない。

### `task.cancel`

Taskと未終了operationの取消を要求する。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 取消対象 |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `reason` | `string` | 任意 | 取消理由 |

成功outputは`OperationAcceptance`。operationの完了前にTaskの取消完了とは扱わない。

### `validation.run`

Artifactに機械検証を実行する。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `artifact_id` | `string` | 必須 | 検証対象 |
| `check_profile_id` | `string` | profile利用時に必須 | 登録済みcheck profile |
| `checks` | `array<ValidationCheck>` | 明示check利用時に必須 | 実行するcheck |

`check_profile_id`か`checks`のちょうど一方を指定する。`ValidationCheck`は`name: string`、`command: string`、`args: array<string>`、`timeout_ms: integer (minimum: 1)`をすべて必須とする。commandとworkspaceは実行前にpolicy allowlistで検査する。

成功outputは`OperationAcceptance`に`artifact_id: string`を加える。最終結果はoperation resultに`validation_id: string`と各checkの`state: enum(passed, failed, unknown)`を含む。

### `decision.record`

監督Codexの判断をArtifactに記録する。同期処理。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `artifact_id` | `string` | 必須 | 判断対象 |
| `decision` | `enum(accepted, rejected, changes_requested)` | 必須 | 判断 |
| `reason` | `string` | 必須 | 判断理由 |
| `evidence` | `array<EvidenceRef>` | 任意 | 判断に参照した保存済み証拠 |

成功outputは`schema_version: const "v1"`、`request_id: string`、`task_id: string`、`revision: integer`、`decision: CodexDecision`を必須とする。`CodexDecision`のfieldはすべて必須。

| Field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `decision_id` | `string` | 必須 | 判断record ID |
| `artifact_id` | `string` | 必須 | 判断対象Artifact |
| `decision` | `enum(accepted, rejected, changes_requested)` | 必須 | 採否 |
| `reason` | `string` | 必須 | 判断理由 |
| `evidence` | `array<EvidenceRef>` | 必須 | 判断に照合した証拠。なしなら空配列 |

### `publication.publish`

指定ArtifactをPull Requestとして公開する。Service policyが必要とする証拠がない場合は副作用前に拒否する。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `artifact_id` | `string` | 必須 | 公開するArtifact |
| `base_branch` | `string` | 必須 | PR base branch |
| `head_branch` | `string` | 必須 | PR head branch |
| `title` | `string` | 必須 | PR title |
| `body` | `string` | 必須 | PR body |
| `evidence` | `array<EvidenceRef>` | 任意 | 追加で照合する証拠 |

公開前にServiceはArtifactとpublication payloadをconfigured secret-scan policyで検査する。secret検出または検査を安全に完了できない場合は`policy_denied`とし、commit・push・PR作成を開始しない。通常のValidation成功だけではsecret scan済みを意味しない。

成功outputは`OperationAcceptance`に`artifact_id: string`を加える。operation完了時のresultは`publication_id: string`、`repository: string`、`head_sha: string`、`pull_request_number: integer`、`pull_request_url: string (format: uri)`を含む。

### `ci.get`

PR / commitのcheck状態を一度観測する。読取専用。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `publication_id` | `string` | `target`未指定時に必須 | Publicationの指定 |
| `target` | `PullRequestTarget | CommitTarget` | `publication_id`未指定時に必須 | PRまたはcommitの指定 |

`PullRequestTarget`と`CommitTarget`のfieldはすべて必須。`publication_id`と`target`は同時に指定しない。

| Type | Field | Type | Required |
| --- | --- | --- | --- |
| `PullRequestTarget` | `repository` | `string` | 必須 |
|  | `number` | `integer (minimum: 1)` | 必須 |
| `CommitTarget` | `repository` | `string` | 必須 |
|  | `sha` | `string` | 必須 |

成功outputは次のfieldをすべて含む。`passed`は観測対象SHAに限る。

| Field | Type | Required |
| --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 |
| `observation_id` | `string` | 必須 |
| `target` | `CiTarget` | 必須 |
| `observed_at` | `string (format: date-time)` | 必須 |
| `checks` | `array<CiCheck>` | 必須 |
| `state` | `enum(pending, passed, failed, unknown)` | 必須 |

### `ci.wait`

CI状態をdeadlineまで待つ。長時間処理。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `publication_id` | `string` | `target`未指定時に必須 | Publicationの指定 |
| `target` | `PullRequestTarget | CommitTarget` | `publication_id`未指定時に必須 | PRまたはcommitの指定 |
| `deadline` | `string (format: date-time)` | 必須 | 待機期限（RFC 3339 UTC） |

`publication_id`と`target`は同時に指定しない。成功outputは`OperationAcceptance`。期限までにCIが確定しない場合、operation result/errorにtimeoutと最後の観測値を返す。

### `task.finish`

Task完了を要求する。指定されたArtifact、accepted decision、policy必須の証拠を照合し、満たさなければ状態を変えない。

| Request field | Type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 完了対象Task |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `artifact_id` | `string` | 必須 | 完了対象Artifact |
| `decision_id` | `string` | 必須 | 同じArtifactへのaccepted CodexDecision |
| `evidence` | `array<EvidenceRef>` | 任意 | 追加で照合する証拠 |

成功outputは`schema_version: const "v1"`、`request_id: string`、`task_id: string`、`revision: integer`、`state: const "completed"`、`artifact_id: string`、`evidence: array<EvidenceRef>`を必須とする。

## Errorと復旧

`stale_revision`、`policy_denied`等の業務errorはMCP tool execution errorとして返し、`isError: true`にする。errorの詳細は`ErrorPayload`に従う。schema/JSON-RPC構造不正などMCP protocol errorと、業務上のtool errorを混同しない。

`retryable: true`は同じrequestの再送許可を意味しない。副作用requestを作り直す場合は、新しいrequest IDと最新revisionを使う。timeout / interruptionで実行結果を確定できない場合、Serviceは`recovery_required`を記録し、成功・失敗・未実行のいずれかを推測しない。状態確定までは同一operationの再実行をせず、`operation.get`で確認する。

## Artifact・diff・diagnostic・logの秘匿

secret、認証token、環境変数値、Provider認証情報をcontextやlogに返さない。既知secretは保存前と返却前にredactする。規則はArtifact本文、full diff、diagnostic本文、publication payloadにも適用する。redactできない本文は返さず、`forbidden` errorと必要な場合の権限付き参照を返す。redactionできない本文を空文字、`unknown`値、成功として偽装しない。log末尾やArtifact本文の取得量は要求範囲に限定する。

## ドメインの意味

Task、Attempt、Artifact、ValidationResult、ReviewVerdict、CodexDecisionの意味は[ドメインモデル](domain-model.md)を正本とする。とくにAttemptの`Succeeded`はProvider呼出しの正常終了のみであり、Validation成功、review承認、CodexDecisionのaccepted、publication、Task完了とは別の事実である。
