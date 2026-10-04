# MCP 操作契約 — wire reference

[概要](mcp-operation-contract.md)にある各toolの入力と出力を定める。toolの入力はMCP `tools/call` の`params.arguments`、成功時の構造化出力はcomplete tool resultの`structuredContent`に対応する。この文書はMCP request metadataやJSON-RPC/tool resultの外側のenvelopeを再定義しない。

toolのschema記述はJSON Schema 2020-12のobject、`properties`、`required`、`type`、`enum`に対応する。表の`Required`欄はfieldの省略可否を表し、`null`可否とは別である。`string | null` はfieldが必須で値にnullを許す。`Optional`はfield自体を省略できる。説明文や例の日本語はschema値ではない。

MCP toolsでは`inputSchema`が入力schemaを定め、任意の`outputSchema`が構造化出力を定める。[MCP Tools仕様（2026-07-28）](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)と[JSON Schema objectのrequired properties](https://json-schema.org/understanding-json-schema/reference/object#required-properties)に従い、#45はこの文書の型・必須field・enumと整合するschema validationを実装する。

## 型と共通規則

| 表記 | JSON型・制約 |
| --- | --- |
| `string` | JSON string。特記があれば`enum`、`format`、最大長等を適用 |
| `integer` | 整数値。revision、page size、連番等に使う |
| `number` | 数値。usage等に使う |
| `boolean` | `true`または`false` |
| `object` | JSON object。fieldの型・必須性は対応する表で定義 |
| `array<T>` | T型の要素を持つJSON array |
| `T \| null` | T型またはJSON `null`。field自体は省略できない |
| `enum(a, b)` | 記載したstring値のみ許可 |

すべてのtool requestの`params.arguments`は`schema_version: "v2"`を必須とする。未対応versionは`unsupported_schema_version`。不明なrequest fieldは`invalid_request`とし、副作用前に拒否する。成功するtool outputの`structuredContent`はすべて`schema_version: "v2"`を含む。業務errorはMCP tool execution error（`isError: true`）として返し、`content`に下記のtyped errorを含める。MCP request metadataやprotocol errorはこのアプリケーション契約の対象外。

副作用を伴うrequestは`request_id: string`を必須とする。既存Taskを変更する場合はさらに`task_id: string`と`expected_revision: integer`を必須とする。読取requestは`request_id`と`expected_revision`を持たない。

冪等性keyの範囲は`caller + tool name + request_id`。`task.create`を含む全副作用requestに適用する。同じkey・同じnormalized payloadの再送は保存済みresponseを返し、副作用を繰り返さない。同じkeyでpayloadが異なる場合は`idempotency_conflict`。Task作成のnormalized payloadは、設定済みSecretScannerが全ての要求テキストfieldをredactした後の値である。redaction結果は再適用で変化しない固定点でなければならず、Serviceはこれを確認し、固定点でない場合は保存・返却を拒否する。

IDはすべて不透明なstringとし、呼出側はIDの形式・連番・内部構造を解釈しない。RFC 3339日時は`string`として送受信し、UTC (`Z`) を使う。fieldがoptionalなら省略し、nullを使うのは型に`| null`と明記された場合だけ。

## 共通型

### `ModelChoice`

Providerへ要求するModelの指定。fieldの省略やnullではなく、Providerの既定値を使う場合も判別値で表す。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `kind` | `enum(named, provider_default)` | 必須 | Model指定方法 |
| `model` | `string` | `kind=named`なら必須 | Providerへ渡すModel識別子 |

`kind=named`では空でない`model`が必須で追加fieldを認めない。空文字のModel IDはProvider実行前に`invalid_request`として拒否する。`kind=provider_default`では`model`を認めない。

~~~json
{"kind":"named","model":"model-a"}
~~~

~~~json
{"kind":"provider_default"}
~~~

### `TaskRequest` / `TaskSnapshot`

| Object | Field | JSON type | Required | 意味 |
| --- | --- | --- | --- | --- |
| `TaskRequest` | `source` | `enum(issue, manual)` | 必須 | 要求の出所 |
|  | `title` | `string` | 必須 | Task title |
|  | `description` | `string` | 必須 | 要求本文 |
|  | `constraints` | `array<string>` | 必須 | 要求制約。制約なしは空配列 |
|  | `issue` | `IssueSnapshot \| null` | 必須 | `source=issue`ならobject、`source=manual`ならnull |
| `TaskSnapshot` | `task_id` | `string` | 必須 | Task ID |
|  | `revision` | `integer` | 必須 | Task更新番号 |
|  | `state` | `enum(pending, active, completed, failed, cancelled)` | 必須 | Task状態 |
|  | `request` | `TaskRequest` | 必須 | 作成時の要求snapshot |

`IssueSnapshot`は次のobjectとする。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `url` | `string (format: uri)` | 必須 | Issue URL |
| `number` | `integer (minimum: 1)` | 必須 | Issue番号 |
| `title` | `string` | 必須 | 取得時点のタイトル |
| `body` | `string` | 必須 | 取得時点の本文 |

### `EvidenceRef`

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `kind` | `enum(validation, review, decision, publication, ci)` | 必須 | 参照する記録の種別 |
| `id` | `string` | 必須 | Serviceが発行した記録ID |

evidenceは結果を申告するfieldではなく、保存済みrecordへの参照である。Serviceは存在、Task所属、対象Artifactを照合する。CI evidenceは対象Publicationのrepository、PR、head SHAが一致しなければならない。存在しないIDは`invalid_request`、別Artifactに属する記録は`evidence_artifact_mismatch`。

### `ContextItem`

各`task.get_context` sectionは次の共通itemを返す。`details`の型はsectionごとの表で定義する。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `id` | `string` | 必須 | 履歴record ID |
| `kind` | `string` | 必須 | record種別 |
| `state` | `string \| null` | 必須 | record種別に定義されたstate。stateを持たないrecordはnull |
| `occurred_at` | `string (format: date-time) \| null` | 必須 | record時刻（RFC 3339 UTC）。記録されていない場合はnull |
| `summary` | `string` | 必須 | 人が読める短い要約 |
| `references` | `array<Reference>` | 必須 | 関連record |
| `details` | `object` | 必須 | section固有の追加情報 |

section固有の`details`は次のfieldで構成する。列挙したfieldはすべて必須であり、null可能なfieldはその型に明記する。

| Section | kind | Field | JSON type | Required | 意味 |
| --- | --- | --- | --- | --- | --- |
| `providers` | `provider` | `provider_id` | `string` | 必須 | Provider ID |
|  |  | `model_ids` | `array<string>` | 必須 | authoritativeなsourceで列挙できたModel ID |
|  |  | `availability` | `enum(available, unavailable, unknown)` | 必須 | 観測した利用可否 |
|  |  | `observed_at` | `string (format: date-time)` | 必須 | Provider状態の観測時刻 |
|  |  | `diagnostic_ref` | `string \| null` | 必須 | 診断参照。なければnull |
|  |  | `availability_evidence` | `AvailabilityEvidence` | 必須 | 状態、unknown理由、観測時刻、情報源 |
|  |  | `authentication` | `AvailabilityEvidence` | 必須 | 認証状態。CLI確認だけではknownにならない |
|  |  | `cli_present` | `Evidence<boolean>` | 必須 | 設定CLIの存在観測 |
|  |  | `cli_version_check` | `Evidence<boolean>` | 必須 | `--version`確認。認証やModel利用権を示さない |
|  |  | `models` | `array<ModelAvailabilityObservation>` | 必須 | Modelごとの利用可否と根拠。未取得はunknown |
| `usage` | `usage` | `name` | `string` | 必須 | 使用量指標名 |
|  |  | `value` | `number \| null` | 必須 | 観測値。不明ならnull |
|  |  | `unit` | `string` | 必須 | 値の単位 |
|  |  | `basis` | `enum(measured, configured, computed, estimated, unknown)` | 必須 | 値の根拠 |
|  |  | `observed_at` | `string \| null` | 必須 | 観測時刻。不明ならnull、非null値はRFC 3339 UTC |
| `attempts` | `attempt` | `requested_provider_id` | `string` | 必須 | 要求Provider ID |
|  |  | `requested_model` | `ModelChoice \| null` | 必須 | 要求Model。旧記録等で確認できなければnull |
|  |  | `observed_provider_id` | `string \| null` | 必須 | 実行されたと観測できたProvider。未知ならnull |
|  |  | `observed_model_id` | `string \| null` | 必須 | 実際に使用したと観測できたModel。未知ならnull |
|  |  | `role` | `enum(implementer, reviewer, explorer) \| null` | 必須 | Attemptの役割。旧記録等で取得できなければnull |
|  |  | `input_artifact_id` | `string \| null` | 必須 | 入力Artifact。初期baseから開始した場合はnull |
|  |  | `base_commit` | `string \| null` | 必須 | 開始時commit。なければnull |
|  |  | `output_artifact_id` | `string \| null` | 必須 | 出力Artifact。未作成ならnull |
|  |  | `diagnostic_ref` | `string \| null` | 必須 | 診断参照。なければnull |
|  |  | `requested_provider_evidence` | `Evidence<string>` | 必須 | 要求Providerの値またはunknown理由、根拠、時刻 |
|  |  | `requested_model_evidence` | `Evidence<ModelChoice>` | 必須 | 要求Modelの値またはunknown理由、根拠、時刻 |
|  |  | `observed_provider_evidence` | `Evidence<string>` | 必須 | 観測Providerの値またはunknown理由、根拠、時刻 |
|  |  | `observed_model_evidence` | `Evidence<string>` | 必須 | 観測Modelの値またはunknown理由、根拠、時刻 |
|  |  | `timestamp_basis` | `enum(persisted, unknown)` | 必須 | occurred_atの元記録があるか |
| `artifacts` | `artifact` | `artifact_id` | `string` | 必須 | Artifact ID |
|  |  | `digest` | `string` | 必須 | Artifact内容のdigest |
|  |  | `source_attempt_id` | `string \| null` | 必須 | 作成元Attempt。なければnull |
|  |  | `base_commit` | `string \| null` | 必須 | 差分の基準commit。なければnull |
|  |  | `diff_ref` | `string \| null` | 必須 | 差分参照。なければnull |
| `validations` | `validation` | `validation_id` | `string` | 必須 | Validation ID |
|  |  | `artifact_id` | `string` | 必須 | 検証対象Artifact |
|  |  | `check_profile_id` | `string \| null` | 必須 | 適用profile。明示checksならnull |
|  |  | `checks` | `array<ValidationCheckResult>` | 必須 | 個別check結果 |
| `reviews` | `review_verdict` | `review_verdict_id` | `string` | 必須 | ReviewVerdict ID |
|  |  | `reviewer_attempt_id` | `string` | 必須 | Reviewer Attempt ID |
|  |  | `artifact_id` | `string` | 必須 | review対象Artifact |
|  |  | `verdict` | `enum(approved, changes_requested, inconclusive)` | 必須 | reviewerの結論 |
| `decisions` | `codex_decision` | `decision_id` | `string` | 必須 | CodexDecision ID |
|  |  | `artifact_id` | `string` | 必須 | 判断対象Artifact |
|  |  | `decision` | `enum(accepted, rejected, changes_requested)` | 必須 | 採否 |
|  |  | `reason` | `string` | 必須 | 判断理由 |
|  |  | `evidence` | `array<EvidenceRef>` | 必須 | 照合した証拠。なければ空配列 |
| `publication` | `publication` | `publication_id` | `string` | 必須 | Publication ID |
|  |  | `artifact_id` | `string` | 必須 | 公開したArtifact |
|  |  | `repository` | `string` | 必須 | repository |
|  |  | `head_sha` | `string` | 必須 | 公開commit |
|  |  | `pull_request_number` | `integer` | 必須 | PR番号 |
|  |  | `pull_request_url` | `string (format: uri)` | 必須 | PR URL |
| `ci` | `ci_observation` | `observation_id` | `string` | 必須 | CI observation ID |
|  |  | `repository` | `string` | 必須 | 対象repository |
|  |  | `pull_request_number` | `integer \| null` | 必須 | PR番号。commit targetならnull |
|  |  | `head_sha` | `string` | 必須 | 観測対象commit |
|  |  | `state` | `enum(pending, passed, failed, unknown)` | 必須 | 集約state |
|  |  | `checks` | `array<CiCheck>` | 必須 | 個別check結果 |

`ContextItem.state`の値は次のとおり。sectionごとに別のenumであり、一覧にないstateを使わない。

`Evidence<T>`は`status`で判別する。knownは`value`、`basis` (`measured`, `configured`, `computed`, `estimated`)、`assessed_at_ms`、`source`を持ち、unknownは`reason`、`assessed_at_ms`、`source`を持つ。`source`は`kind` (`provider_api`, `provider_cli`, `provider_adapter`, `execution_ledger`, `repository_config`) と`reference`を含む。`provider_adapter`はProvider adapter自身が返した観測を表す。`AvailabilityEvidence`は`status` enum (`available`、`unavailable`、`unknown`)、`observed_at_ms`、`source`を同じobjectに持つ。unavailable / unknownではnon-empty `reason`も同じobjectに置き、statusを入れ子にしない。`ModelAvailabilityObservation`は`model: ModelChoice`と`availability: AvailabilityEvidence`を持つ。Attempt itemで`occurred_at`を特定できない場合はnull、`timestamp_basis: unknown`を返す。旧roleを特定できない場合は`role: null`とする。

例えば未確認状態は`{"status":"unknown","reason":"authentication was not checked","observed_at_ms":1790115723000,"source":{"kind":"provider_cli","reference":"codex"}}`の形で返す。`status`自体を`{"status":"unknown"}`のようなobjectにしない。

`model_ids`には権威あるModel catalogで確認できたnamed Modelだけを含める。CLI起動状態からModel一覧・認証・利用権・quota・利用量・料金を推定しない。観測できない値はevidenceの`unknown`として理由・時刻・sourceを残す。Task Attemptのrequested値とobserved値は別々に保持し、unknown observed値をrequested値で埋めない。

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

| Object | Field | JSON type | Required | 意味 |
| --- | --- | --- | --- | --- |
| `Reference` | `kind` | `string` | 必須 | 参照先recordの種別 |
|  | `id` | `string` | 必須 | 参照先record ID |
| `ValidationCheckResult` | `name` | `string` | 必須 | check名 |
|  | `state` | `enum(passed, failed, unknown)` | 必須 | check結果 |
|  | `diagnostic_ref` | `string \| null` | 必須 | 診断参照。なければnull |
| `CiCheck` | `name` | `string` | 必須 | CI check名 |
|  | `state` | `enum(pending, passed, failed, unknown)` | 必須 | 観測した状態 |
|  | `url` | `string \| null` | 必須 | check URL。なければnull |
|  | `completed_at` | `string \| null` | 必須 | 完了時刻。非nullならRFC 3339 UTC |

### `ContextPage`

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `items` | `array<ContextItem>` | 必須 | sectionに属する履歴record。section固有の`details`型を使う |
| `next_cursor` | `string \| null` | 必須 | 次page取得用token。nullは現在のsnapshotに続きがない |

cursorは不透明であり、Task・section・page size・snapshot revisionに束縛される。次pageでは同じpage sizeと、同じsectionのcursorを使う。revision変更等で利用できないcursorは`invalid_cursor`。Usage / Attempt historyは`occurred_at`降順、同時刻ならID降順で並べる。`occurred_at: null`は全てのnon-null時刻より後に並べ、null同士はID降順にする。cursorはこの完全な順序キー（時刻の有無・時刻値・ID）で並んだsnapshot内の次位置を表し、同じ順序で続きから再開する。page境界に重複・欠落を作らない。

### `OperationAcceptance`

非同期toolが受け付けられたときの共通structured output。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 対応するrequest ID |
| `task_id` | `string` | 必須 | 対象Task ID |
| `revision` | `integer` | 必須 | 受付後のTask revision |
| `operation` | `OperationRef` | 必須 | 新規operation |
| `attempt_id` | `string` | 条件付き | Attemptを作るtoolが返すAttempt ID |

`attempt_id`は`attempt.run`で必須、ほかの非同期toolでは省略する。

`OperationRef`のfieldはすべて必須。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `operation_id` | `string` | 必須 | operation ID |
| `kind` | `enum(attempt.run, task.cancel, operation.cancel, validation.run, publication.publish, ci.wait)` | 必須 | 受付対象の操作 |
| `state` | `enum(accepted, running, completed, failed, cancelling, cancelled, recovery_required)` | 必須 | operation状態 |
| `submitted_at` | `string (format: date-time)` | 必須 | 受付時刻 |

受付は処理成功を意味しない。最終結果は`operation.get`で取得する。

### `ErrorPayload`

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string \| null` | 必須 | parse可能なら対象request ID。取得不能ならnull |
| `error` | `Error` | 必須 | 業務error |

`Error`の次のfieldはすべて必須。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `code` | `ErrorCode` | 必須 | 下記の業務error code |
| `message` | `string` | 必須 | 人が読める説明 |
| `retryable` | `boolean` | 必須 | 同じrequestを再送してよいかではなく、新しいrequestを作成して回復可能か |
| `current_task_revision` | `integer \| null` | 必須 | stale revision error時の現在値。それ以外はnull |
| `operation_id` | `string \| null` | 必須 | 関連operation。なければnull |
| `details_ref` | `string \| null` | 必須 | 追加診断参照。なければnull |

`ErrorCode`は`invalid_request`、`unsupported_schema_version`、`task_not_found`、`stale_revision`、`idempotency_conflict`、`busy`、`unknown_provider`、`unknown_model`、`policy_denied`、`budget_exhausted`、`workspace_boundary_violation`、`artifact_not_found`、`artifact_task_mismatch`、`evidence_artifact_mismatch`、`invalid_state_transition`、`operation_not_found`、`not_cancellable`、`timeout`、`cancelled`、`interrupted`、`recovery_required`、`invalid_cursor`、`forbidden`、`internal_error`のいずれか。

`Error.details_ref`は、追加のtyped recordを取得するためのopaque referenceである。`ci.wait`が期限切れになった場合は、最後に永続化した`CiObservation`のIDを指す。診断本文を直接errorへ埋め込まない。

`interrupted`はerror codeでありoperation stateではない。終了結果を確定できないinterrupted operationは`recovery_required`になり、復旧状態が確定するまで同じoperationのretryで副作用を再実行しない。再起動後もServiceはこの状態を保持し、`operation.get`で確認できる。復旧確認前の`operation.cancel`は`not_cancellable`で拒否する。

## Tool schemas

各request / response表はJSON Schemaの`properties`に相当するfieldを列挙する。`Required`は「必須」「任意」「条件付き」のいずれか。条件付きfieldの条件は表の直後に記す。未知request fieldは拒否し、未知response fieldは無視する。全成功outputには`schema_version: const "v2"`を含む。

### `task.create`

Taskを作成する。`request_id`は冪等性keyに含まれる。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 重複作成を防ぐrequest ID |
| `source` | `enum(issue, manual)` | 必須 | 要求の出所 |
| `title` | `string` | 必須 | Task title |
| `description` | `string` | 必須 | 要求本文 |
| `constraints` | `array<string>` | 必須 | 要求制約。なしなら空配列 |
| `issue` | `IssueSnapshot` | 条件付き | sourceが`issue`なら必須、`manual`なら省略 |

`issue`は前述の`IssueSnapshot`型を使う。title/bodyは作成時点のsnapshot。
OperationServiceは`task.create`の前にtitle、description、各constraint、IssueのURL/title/bodyをSecretScannerでredactし、再適用で変化しない固定点であることを確認してからredacted canonical payloadだけを冪等性記録とTask snapshotに保存する。同じ入力の再送も同じredacted payloadで照合する。SecretScannerが未設定、失敗、または固定点を作れない場合は`policy_denied`で拒否し、raw textを保存・返却しない。

成功output:

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 入力request IDのecho |
| `task_id` | `string` | 必須 | 新規Task ID |
| `revision` | `integer` | 必須 | 初期revision |
| `state` | `const "pending"` | 必須 | 初期Task state |
| `request` | `TaskRequest` | 必須 | 保存したsource・要求・制約・Issue snapshot |

例（tool `arguments`）:

~~~json
{
  "schema_version": "v2",
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

このRust Service sliceは`providers`、`usage`、`attempts`、`reviews`のContextPageを生成する。これはRust APIの実装状況であり、MCP tool / transportはIssue #45の対象として未実装。`reviews` itemはReviewVerdict ID、reviewer Attempt ID、Artifact ID、verdictを含む。`usage`はOperation Service Ledgerに保存されたProvider報告metricだけを返し、budgetやquotaは作らない。metricの`name`、`value`、`unit`は、Task snapshotと同じSecretScannerによるredactionと固定点検査を通してから保存する。scanner未設定・失敗・固定点不成立の場合、そのoperation自体はProvider結果どおり完了するがusage metricは一件も保存せず、固定diagnostic code `usage_redaction_unavailable`を記録する。既存Ledgerのmetricはcontext返却前にも再redactし、検査に失敗した場合は`policy_denied`とし、raw metricを返さない。保存値をJSON numberとして保持できない場合は`value:null`、`basis:"unknown"`とし、元の文字列値は返さない。redactionで値が変更された場合も数値を推定せず`value:null`、`basis:"unknown"`とする。観測時刻にはOperationの完了時刻を使い、未保存ならnullとする。
`reviews` sectionを要求した場合、保存済みsummaryもProvider観測より前に同じsafe redactionと固定点検査を通す。検査に失敗した場合は固定error `review redaction is unavailable` でContext取得を拒否し、raw summaryを返さない。Reviewsを要求しない取得ではsummaryを読み出さない。
内部の`ExecutionLedger::get_task` / `SqliteExecutionLedger::get_task`は保存内容を復元するだけで、redactionしないためMCP response sourceには使わない。`task.get_context`を含む外部応答はServiceのSecretScanner境界を通したデータだけから組み立てる。scanner未設定・失敗・redaction固定点不成立ならraw Task、Attempt、Usageを返さず、固定の業務errorでfail closedする。将来別のTask / Attempt直列化経路を追加する場合もscanner境界を通す。
保存済みTask snapshotの要求textはcontext返却前にもSecretScannerでredactし、固定点であることを確認する。これにより既存の未redacted snapshotもraw textを返さない。SecretScannerが未設定、redactionが失敗、または固定点を作れない場合はProvider probeより前に`policy_denied`とし、raw snapshotを含む応答を返さない。
`providers` sectionではprovider observationsの件数がpage_size以内なら同一snapshot内で全件を返し、page_sizeを超える場合はrequestを拒否する。UsageとAttempt historyは`occurred_at`降順、同時刻ならID降順でpage化する。`occurred_at: null`は全non-null時刻より後に置き、null同士はID降順とする。cursorはこの順序キー（時刻の有無・時刻値・ID）で並んだ同じTask snapshot内の次位置から再開し、Task、section、page size、Task revisionに束縛する。
Provider観測sourceがProvider一覧を列挙できない場合は、空配列として成功したように見せずcontext取得を失敗させる。

このRust Service sliceには、監督側が明示実行する任意の`OperationService::submit_artifact_review`もある。reviewはread-only reviewer Attemptとして実行し、成功時にReviewVerdictをArtifact ID/treeへ結び付ける。`OperationService::submit_attempt`はimplementerの`ArtifactInput`を、失敗またはretry可能な取消Attemptからの`RetryOf` / `EscalationOf`、または成功Artifactに対する同一Artifact/treeの成功reviewer `changes_requested` evidenceに基づく`ReworkFrom`として受け付ける。条件に合わないlineageや証拠は受付前に拒否する。`submit_artifact_review`内の`ArtifactInput`は対象Artifactをread-only reviewer Attemptへ渡す入力であり、review後の修正を開始しない。旧Ledgerのaccepted non-reviewer ArtifactInput operationはclaim後に`failed` / `artifact_input_evidence_stale`で終端し、Providerを起動しない。ReviewVerdictはTaskを完了させず、修正を自動決定しない。これらはRust Service APIであり、MCP transportは未実装。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `task_id` | `string` | 必須 | 読むTask |
| `sections` | `array<enum(providers, usage, attempts, artifacts, validations, reviews, decisions, publication, ci)>` | 必須 | 返す履歴section。重複不可 |
| `page_size` | `integer (minimum: 1, maximum: 100)` | 任意、既定20 | 各sectionから返す最大件数 |
| `cursors` | `object<string, string>` | 任意 | section名からそのsectionの次page cursorへのmap |

成功output:

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
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
  "details": {
    "requested_provider_id": "provider-a",
    "requested_model": {"kind": "named", "model": "model-a"},
    "observed_provider_id": "provider-a",
    "observed_model_id": null,
    "role": "implementer",
    "input_artifact_id": null,
    "base_commit": "abc123",
    "output_artifact_id": "artifact-01",
    "diagnostic_ref": null,
    "requested_provider_evidence": {"status":"known","value":"provider-a","basis":"configured","assessed_at_ms":1790115723000,"source":{"kind":"execution_ledger","reference":"attempt:attempt-01"}},
    "requested_model_evidence": {"status":"known","value":{"kind":"named","model":"model-a"},"basis":"configured","assessed_at_ms":1790115723000,"source":{"kind":"execution_ledger","reference":"attempt:attempt-01"}},
    "observed_provider_evidence": {"status":"known","value":"provider-a","basis":"measured","assessed_at_ms":1790115783000,"source":{"kind":"execution_ledger","reference":"attempt:attempt-01"}},
    "observed_model_evidence": {"status":"unknown","reason":"the Provider result did not record an observed Model","assessed_at_ms":1790115783000,"source":{"kind":"execution_ledger","reference":"attempt:attempt-01"}},
    "timestamp_basis": "persisted"
  }
}
~~~

### `attempt.run`

指定Provider / Modelを一回実行する。長時間処理。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `provider_id` | `string` | 必須 | 呼び出すProvider |
| `model_id` | `ModelChoice` | 必須 | named Modelまたは明示したProvider既定値 |
| `instruction` | `string` | 必須 | Providerへ渡す依頼 |
| `role` | `enum(implementer, reviewer, explorer)` | 必須 | Attemptのrole |
| `input` | `ArtifactInput \| BaseInput` | 必須 | 入力Artifactまたは初期baseのどちらか一方 |
| `timeout_ms` | `integer (minimum: 1)` | 任意 | timeout。省略時はService policy値 |

`ArtifactInput`と`BaseInput`の各fieldはすべて必須。

| Object | Field | JSON type | Required | 意味 |
| --- | --- | --- | --- | --- |
| `ArtifactInput` | `artifact_id` | `string` | 必須 | 入力Artifact ID |
| `BaseInput` | `repository` | `string` | 必須 | base commitのrepository |
|  | `commit` | `string` | 必須 | base commit SHA |

入力Artifactは同じTaskに属する必要がある。branch/pathだけのbase指定は認めない。

成功outputは`OperationAcceptance`。`attempt_id`を必須とする。受付時の`operation.state`は`accepted`。

#### 任意の意味レビューを依頼するRust API

Rust Serviceは監督側が明示的に呼び出す`OperationService::submit_artifact_review`を提供する。これは仕様上のreviewer Attemptを実行するAPIで、MCP toolやtransportではない。`ArtifactReviewRequest::new(request_id, task_id, expected_revision, provider_id, model_id, artifact_id, validation_ids, criteria)`で依頼を構成する。Validation IDとcriteriaは各1件以上、重複不可で、criteriaに空文字を含めない。

依頼時はTask revision、同一Taskで最新かつavailableなArtifact、そのArtifactを作成した成功implementer Attempt、同じArtifact ID/treeを参照する各Validationを確認する。Task要求、Artifact差分、Validation結果、criteriaはProviderへ渡す前にSecretScannerでredactし固定点を確認する。Scannerが未設定または検査不能なら受付を拒否する。Reviewer Providerはread-only workspaceを強制できる必要があり、実行後もworkspace treeが対象Artifactと一致することを確認する。成功Provider出力は`{"verdict":"approved|changes_requested|inconclusive","summary":"..."}`形式のJSONでなければならない。redacted summaryを保存し、ReviewVerdictをreviewer Attempt・Artifact ID/treeへ結び付ける。

Task要求、Validation事実、criteria、全diffを含む完成済みinstructionは16 KiB以下でなければならず、上限を超える依頼は受付前に拒否する。

`ArtifactInput`はこのAPIでは対象Artifactをreviewer Attemptへ渡す入力であり、review後の実装修正を開始しない。旧Ledgerに残るaccepted non-reviewer ArtifactInputはclaim後に`failed` / `artifact_input_evidence_stale`として終端し、Providerを起動しない。ReviewVerdictはreviewerの結論であり、Task完了や再作業を自動決定せず、監督側の採否判断を代替しない。

### `operation.get`

operation状態・結果を読む。読取専用。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `operation_id` | `string` | 必須 | 取得対象 |

成功output:

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `operation` | `Operation` | 必須 | operation記録 |

`Operation`のfieldはすべて必須。nullを許すfieldも省略せず、該当しない場合はnullを返す。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `operation_id` | `string` | 必須 | operation ID |
| `task_id` | `string` | 必須 | 対象Task ID |
| `kind` | `enum(attempt.run, task.cancel, operation.cancel, validation.run, publication.publish, ci.wait)` | 必須 | 実行したtool |
| `state` | `enum(accepted, running, completed, failed, cancelling, cancelled, recovery_required)` | 必須 | operation状態 |
| `submitted_at` | `string (format: date-time)` | 必須 | 受付時刻 |
| `started_at` | `string \| null` | 必須 | 開始時刻 |
| `completed_at` | `string \| null` | 必須 | 終了時刻 |
| `result` | `OperationResult \| null` | 必須 | 確定した結果 |
| `error` | `Error \| null` | 必須 | 失敗情報 |

`result`と`error`は同時に非nullにならない。`OperationResult`は`kind`に対応する次のobjectのいずれか。

| Operation kind | Result field | JSON type | Required | 意味 |
| --- | --- | --- | --- | --- |
| `attempt.run` | `attempt_id` | `string` | 必須 | 作成したAttempt ID |
|  | `attempt_state` | `enum(succeeded, failed, cancelled)` | 必須 | Provider実行の終端state |
|  | `requested_provider_id` | `string` | 必須 | 要求Provider ID |
|  | `model_id` | `ModelChoice` | 必須 | 要求したnamed ModelまたはProvider既定値 |
|  | `observed_provider_id` | `string \| null` | 必須 | 実行後に観測したProvider。未知ならnull |
|  | `observed_model_id` | `string \| null` | 必須 | 実行後に観測したModel。未知ならnull |
|  | `output_artifact_id` | `string \| null` | 必須 | 出力Artifact。生成されなければnull |
|  | `usage` | `array<UsageMetric>` | 必須 | Provider使用量 |
|  | `diagnostic_ref` | `string \| null` | 必須 | 診断参照。なければnull |
| `task.cancel` | `task_state` | `enum(cancelled, active)` | 必須 | 取消後のTask state |
|  | `cancelled_operation_ids` | `array<string>` | 必須 | 取消したoperation ID |
| `operation.cancel` | `target_operation_id` | `string` | 必須 | 取消対象operation |
|  | `target_state` | `enum(cancelled, running, recovery_required)` | 必須 | 対象operationの結果state |
| `validation.run` | `validation_id` | `string` | 必須 | Validation ID |
|  | `artifact_id` | `string` | 必須 | 検証対象Artifact |
|  | `state` | `enum(passed, failed, unknown)` | 必須 | 集約結果 |
|  | `checks` | `array<ValidationCheckResult>` | 必須 | 個別check結果 |
| `publication.publish` | `publication_id` | `string` | 必須 | Publication ID |
|  | `repository` | `string` | 必須 | repository |
|  | `head_sha` | `string` | 必須 | 公開commit |
|  | `pull_request_number` | `integer` | 必須 | 作成したPR番号 |
|  | `pull_request_url` | `string (format: uri)` | 必須 | PR URL |
| `ci.wait` | `observation_id` | `string` | 必須 | CI observation ID |
|  | `target` | `CiTarget` | 必須 | 観測対象 |
|  | `observed_at` | `string (format: date-time)` | 必須 | 観測時刻 |
|  | `state` | `enum(pending, passed, failed, unknown)` | 必須 | 集約結果 |
|  | `checks` | `array<CiCheck>` | 必須 | 個別check結果 |

`UsageMetric`と`CiTarget`のfieldはすべて必須。`attempt.run`が返すusage配列も同じredaction済み値だけを含む。usageを安全にredactできなければ配列を空にし、診断コード`usage_redaction_unavailable`を返す。`task.get_context`の`attempts` sectionはusage文字列を複製しないが、Attemptの他の履歴も含め、Serviceが`TaskSnapshot`、Usage、Attempt全体を組み立てる間にraw metricをcontextへ混入させない。

要求Modelはoperationの受理時点で保存し、observed targetはProvider結果を受け取った後に別fieldへ保存する。Provider出力からModelを確定できない場合は`observed_model_id: null`とし、`model_id`をコピーしない。既知のProvider error `unknown_model`は、選択ModelをProviderが受け付けない場合にも使う。

| Object | Field | JSON type | Required | 意味 |
| --- | --- | --- | --- | --- |
| `UsageMetric` | `name` | `string` | 必須 | 指標名 |
|  | `value` | `number \| null` | 必須 | 値。不明ならnull |
|  | `unit` | `string` | 必須 | 単位 |
|  | `basis` | `enum(measured, configured, computed, estimated, unknown)` | 必須 | 値の根拠 |
| `CiTarget` | `repository` | `string` | 必須 | repository |
|  | `pull_request_number` | `integer \| null` | 必須 | PR番号。commit targetならnull |
|  | `head_sha` | `string` | 必須 | 観測対象SHA |

### `operation.list_logs`

指定operationのログ範囲を読む。ログ本文はredacted済み。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `operation_id` | `string` | 必須 | 対象operation |
| `stream` | `enum(stdout, stderr, diagnostic)` | 必須 | 読むstream |
| `cursor` | `string` | 任意 | 次page cursor |
| `limit` | `integer (minimum: 1, maximum: 1000)` | 任意、既定100 | 最大chunk件数 |

成功outputは次のfieldをすべて含む。`LogChunk`のfieldもすべて必須。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `chunks` | `array<LogChunk>` | 必須 | 取得したlog chunk |
| `next_cursor` | `string \| null` | 必須 | 次page cursor。続きがなければnull |

| LogChunk field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `sequence` | `integer` | 必須 | operation内のchunk順 |
| `occurred_at` | `string (format: date-time)` | 必須 | log記録時刻 |
| `text` | `string` | 必須 | redacted済み本文 |
| `redacted` | `boolean` | 必須 | 本文内にredactionを行ったか |

cursorはoperation・stream・limitに束縛される。末尾到達はoperation完了を意味しない。

### `operation.cancel`

ひとつのoperationへの取消を要求する。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 対象operationのTask |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `operation_id` | `string` | 必須 | 取消対象 |

成功outputは`OperationAcceptance`。`operation`は取消要求を表す新operation。対象operationが停止するまでは取消完了ではない。

### `task.cancel`

Taskと未終了operationの取消を要求する。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 取消対象 |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `reason` | `string` | 任意 | 取消理由 |

成功outputは`OperationAcceptance`。operationの完了前にTaskの取消完了とは扱わない。

### `validation.run`

Artifactに機械検証を実行する。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `artifact_id` | `string` | 必須 | 検証対象 |
| `check_profile_id` | `string` | profile利用時に必須 | 登録済みcheck profile |
| `checks` | `array<ValidationCheck>` | 明示check利用時に必須 | 実行するcheck |

`check_profile_id`か`checks`のちょうど一方を指定する。`ValidationCheck`のfieldはすべて必須。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `name` | `string` | 必須 | check名 |
| `command` | `string` | 必須 | allowlist検査する実行command |
| `args` | `array<string>` | 必須 | command引数。なしなら空配列 |
| `timeout_ms` | `integer (minimum: 1)` | 必須 | check timeout |

commandとworkspaceは実行前にpolicy allowlistで検査する。

成功outputは`OperationAcceptance`の全fieldと、次の必須fieldを返す。最終結果の各fieldは`operation.get`の`validation.run` result schemaを参照。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `artifact_id` | `string` | 必須 | 検証対象Artifact |

### `decision.record`

監督Codexの判断をArtifactに記録する。同期処理。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `artifact_id` | `string` | 必須 | 判断対象 |
| `decision` | `enum(accepted, rejected, changes_requested)` | 必須 | 判断 |
| `reason` | `string` | 必須 | 判断理由 |
| `evidence` | `array<EvidenceRef>` | 任意 | 判断に参照した保存済み証拠 |

成功outputのfieldはすべて必須。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 入力request IDのecho |
| `task_id` | `string` | 必須 | 対象Task ID |
| `revision` | `integer` | 必須 | 記録後のTask revision |
| `decision` | `CodexDecision` | 必須 | 保存された判断record |

`CodexDecision`のfieldはすべて必須。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `decision_id` | `string` | 必須 | 判断record ID |
| `artifact_id` | `string` | 必須 | 判断対象Artifact |
| `decision` | `enum(accepted, rejected, changes_requested)` | 必須 | 採否 |
| `reason` | `string` | 必須 | 判断理由 |
| `evidence` | `array<EvidenceRef>` | 必須 | 判断に照合した証拠。なしなら空配列 |

### `publication.publish`

指定ArtifactをPull Requestとして公開する。Service policyが必要とする証拠がない場合は副作用前に拒否する。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `artifact_id` | `string` | 必須 | 公開するArtifact |
| `decision_id` | `string` | 必須 | このArtifactに対する保存済みaccepted CodexDecision |
| `base_branch` | `string` | 必須 | PR base branch |
| `head_branch` | `string` | 必須 | PR head branch |
| `title` | `string` | 必須 | PR title |
| `body` | `string` | 必須 | PR body |
| `evidence` | `array<EvidenceRef>` | 任意 | decision以外に追加で照合する証拠 |

公開前にServiceは`decision_id`で保存済みdecisionを検索し、decisionが`accepted`で、その`artifact_id`が公開対象と一致することを確認する。不在・不一致・非acceptedなら`invalid_state_transition`で拒否する。加えてServiceはArtifactとpublication payloadをconfigured secret-scan policyで検査する。secret検出または検査を安全に完了できない場合は`policy_denied`とし、commit・push・PR作成を開始しない。通常のValidation成功だけではsecret scan済みを意味しない。

push開始前に確定した検証・remote URL・push destinationの拒否は`failed`として記録する。push結果またはPR作成API結果が不明な場合は`recovery_required`とし、同じoperationを再実行しない。push成功後にPR list/parseなどのread-only観測が失敗し、既存matching PRの有無を確認できない場合も`recovery_required`とする。PR作成を始める前に確定したpayload/既存PR不一致は`failed`とするが、既に成功したpushは取り消さない。既存PRを再利用するにはstateが`OPEN`であり、Draft状態、head SHA、head/base branch、title/bodyが要求に一致しなければならない。`CLOSED` / `MERGED`のPRは成功結果として返さない。新規作成API応答も`open`状態を確認する。

成功outputは`OperationAcceptance`の全fieldと、次の必須fieldを返す。最終結果は`operation.get`の`publication.publish` result schemaを参照。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `artifact_id` | `string` | 必須 | 公開対象Artifact |

### `ci.get`

PR / commitのcheck状態を一度観測する。読取専用。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `publication_id` | `string` | `target`未指定時に必須 | Publicationの指定 |
| `target` | `PullRequestTarget \| CommitTarget` | `publication_id`未指定時に必須 | PRまたはcommitの指定 |

`PullRequestTarget`と`CommitTarget`のfieldはすべて必須。`publication_id`と`target`は同時に指定しない。

| Object | Field | JSON type | Required | 意味 |
| --- | --- | --- | --- | --- |
| `PullRequestTarget` | `repository` | `string` | 必須 | repository |
|  | `number` | `integer (minimum: 1)` | 必須 | PR番号 |
| `CommitTarget` | `repository` | `string` | 必須 | repository |
|  | `sha` | `string` | 必須 | commit SHA |

成功outputは次のfieldをすべて含む。`passed`は観測対象SHAに限る。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `observation_id` | `string` | 必須 | observation ID |
| `target` | `CiTarget` | 必須 | 観測対象 |
| `observed_at` | `string (format: date-time)` | 必須 | 観測時刻 |
| `checks` | `array<CiCheck>` | 必須 | 個別check結果 |
| `state` | `enum(pending, passed, failed, unknown)` | 必須 | 集約結果 |

### `ci.wait`

CI状態をdeadlineまで待つ。長時間処理。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `publication_id` | `string` | `target`未指定時に必須 | Publicationの指定 |
| `target` | `PullRequestTarget \| CommitTarget` | `publication_id`未指定時に必須 | PRまたはcommitの指定 |
| `deadline` | `string (format: date-time)` | 必須 | 待機期限（RFC 3339 UTC） |

`publication_id`と`target`は同時に指定しない。成功outputは`OperationAcceptance`。期限までにCIが確定しない場合、operationは`failed`、`result`はnull、`error.code`は`timeout`、`error.details_ref`は最後に永続化した`CiObservation` IDを返す。CI結果が未確定でもObservation自体を永続化できた場合はこのtimeout応答であり、成功扱いではない。Serviceがoperationの終了状態または最後の観測値の永続化を確定できない中断の場合は`recovery_required`とし、結果を推測しない。

### `task.finish`

Task完了を要求する。指定されたArtifact、accepted decision、policy必須の証拠を照合し、満たさなければ状態を変えない。

| Request field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 冪等性key |
| `task_id` | `string` | 必須 | 完了対象Task |
| `expected_revision` | `integer` | 必須 | 最後に観測したrevision |
| `artifact_id` | `string` | 必須 | 完了対象Artifact |
| `decision_id` | `string` | 必須 | 同じArtifactへのaccepted CodexDecision |
| `evidence` | `array<EvidenceRef>` | 任意 | 追加で照合する証拠 |

成功outputのfieldはすべて必須。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `request_id` | `string` | 必須 | 入力request IDのecho |
| `task_id` | `string` | 必須 | 完了したTask ID |
| `revision` | `integer` | 必須 | 完了後のTask revision |
| `state` | `const "completed"` | 必須 | 完了状態 |
| `artifact_id` | `string` | 必須 | 完了対象Artifact |
| `evidence` | `array<EvidenceRef>` | 必須 | 照合した証拠。なければ空配列 |

## Errorと復旧

`stale_revision`、`policy_denied`等の業務errorはMCP tool execution errorとして返し、`isError: true`にする。errorの詳細は`ErrorPayload`に従う。schema/JSON-RPC構造不正などMCP protocol errorと、業務上のtool errorを混同しない。

`retryable: true`は同じrequestの再送許可を意味しない。副作用requestを作り直す場合は、新しいrequest IDと最新revisionを使う。timeout / interruptionで実行結果を確定できない場合、Serviceは`recovery_required`を記録し、成功・失敗・未実行のいずれかを推測しない。状態確定までは同一operationの再実行をせず、`operation.get`で確認する。

## Artifact・diff・diagnostic・logの秘匿

secret、認証token、環境変数値、Provider認証情報をcontextやlogに返さない。既知secretは保存前と返却前にredactする。Usage metricのname/value/unitもこの規則の対象であり、redact済みfieldはredact結果で返す。valueがredactで変更された場合は数値へ推測変換せず`value:null`、`basis:"unknown"`とする。Usage metric全体を保存しないのはscanner未設定・失敗・固定点不成立の場合であり、その場合は安全な固定diagnostic codeを残す。規則はArtifact本文、full diff、diagnostic本文、publication payloadにも適用する。scanner未設定・失敗・redaction不能の場合は`policy_denied`で拒否し、本文を返さない。`forbidden`は権限境界で参照自体が許可されない場合に限る。redactionできない本文を空文字、`unknown`値、成功として偽装しない。log末尾やArtifact本文の取得量は要求範囲に限定する。

業務errorのmessageへProviderのstdout / stderrやdiagnostic本文を埋め込まない。診断はtyped referenceで返し、本文を取得する場合は同じredaction規則と権限境界を適用する。redaction機能が利用できない実装はraw本文を返却・永続化せず、本文を含まない固定errorを返す。

## ドメインの意味

Task、Attempt、Artifact、ValidationResult、ReviewVerdict、CodexDecisionの意味は[ドメインモデル](domain-model.md)を正本とする。とくにAttemptの`Succeeded`はProvider呼出しの正常終了のみであり、Validation成功、review承認、CodexDecisionのaccepted、publication、Task完了とは別の事実である。
