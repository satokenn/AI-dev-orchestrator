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

CI observationでは、v2既存outputの必須fieldとenumを維持したまま、任意の詳細fieldを追加できる。これらはRequired Check集合や個別check状態の説明に使う。旧v2 consumerは未知のresponse fieldを無視できる。consumerは任意fieldがない場合、または値が`unknown`の場合にCI成功を推測してはならない。CI成功は、Required Check集合が既知かつ空でなく、その集合内の全checkが成功したときだけ表す。Required Check集合がunknownまたは既知の空集合なら、観測したcheckがすべて成功していても集約stateは`unknown`とする。

副作用を伴うrequestは`request_id: string`を必須とする。既存Taskを変更する場合はさらに`task_id: string`と`expected_revision: integer`を必須とする。読取requestは`request_id`と`expected_revision`を持たない。

冪等性keyの範囲は`caller + tool name + request_id`。`task.create`を含む全副作用requestに適用する。同じkey・同じnormalized payloadの再送は保存済みresponseを返し、副作用を繰り返さない。同じkeyでpayloadが異なる場合は`idempotency_conflict`。

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
| `occurred_at` | `string (format: date-time)` | 必須 | record時刻（RFC 3339 UTC） |
| `summary` | `string` | 必須 | 人が読める短い要約 |
| `references` | `array<Reference>` | 必須 | 関連record |
| `details` | `object` | 必須 | section固有の追加情報 |

section固有の`details`は次のfieldで構成する。省略可否は`Required`欄に従い、null可能なfieldはその型に明記する。

| Section | kind | Field | JSON type | Required | 意味 |
| --- | --- | --- | --- | --- | --- |
| `providers` | `provider` | `provider_id` | `string` | 必須 | Provider ID |
|  |  | `model_ids` | `array<string>` | 必須 | 利用可能なModel ID |
|  |  | `availability` | `enum(available, unavailable, unknown)` | 必須 | 観測した利用可否 |
|  |  | `observed_at` | `string (format: date-time)` | 必須 | Provider状態の観測時刻 |
|  |  | `diagnostic_ref` | `string \| null` | 必須 | 診断参照。なければnull |
| `usage` | `usage` | `name` | `string` | 必須 | 使用量指標名 |
|  |  | `value` | `number \| null` | 必須 | 観測値。不明ならnull |
|  |  | `unit` | `string` | 必須 | 値の単位 |
|  |  | `basis` | `enum(measured, configured, computed, estimated, unknown)` | 必須 | 値の根拠 |
|  |  | `observed_at` | `string \| null` | 必須 | 観測時刻。不明ならnull、非null値はRFC 3339 UTC |
| `attempts` | `attempt` | `requested_provider_id` | `string` | 必須 | 要求Provider ID |
|  |  | `requested_model` | `ModelChoice \| null` | 必須 | 要求Model。旧記録等で確認できなければnull |
|  |  | `observed_provider_id` | `string \| null` | 必須 | 実行されたと観測できたProvider。未知ならnull |
|  |  | `observed_model_id` | `string \| null` | 必須 | 実際に使用したと観測できたModel。未知ならnull |
|  |  | `role` | `enum(implementer, reviewer, explorer)` | 必須 | Attemptの役割 |
|  |  | `input_artifact_id` | `string \| null` | 必須 | 入力Artifact。初期baseから開始した場合はnull |
|  |  | `base_commit` | `string \| null` | 必須 | 開始時commit。なければnull |
|  |  | `output_artifact_id` | `string \| null` | 必須 | 出力Artifact。未作成ならnull |
|  |  | `diagnostic_ref` | `string \| null` | 必須 | 診断参照。なければnull |
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
|  |  | `observed_at` | `string (format: date-time)` | 任意 | check状態の観測時刻。未対応の旧producerでは省略 |
|  |  | `required_checks` | `RequiredCheckSet` | 任意 | Required Check集合。fieldがなければunknownとして読む |

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

次のobject型を使用する。fieldの省略可否は`Required`欄に従う。

| Object | Field | JSON type | Required | 意味 |
| --- | --- | --- | --- | --- |
| `Reference` | `kind` | `string` | 必須 | 参照先recordの種別 |
|  | `id` | `string` | 必須 | 参照先record ID |
| `ValidationCheckResult` | `name` | `string` | 必須 | check名 |
|  | `state` | `enum(passed, failed, unknown)` | 必須 | check結果 |
|  | `diagnostic_ref` | `string \| null` | 必須 | 診断参照。なければnull |
| `CiCheck` | `name` | `string` | 必須 | CI check名またはstatus context |
|  | `state` | `enum(pending, passed, failed, unknown)` | 必須 | 観測した状態 |
|  | `url` | `string \| null` | 必須 | check URL。なければnull |
|  | `completed_at` | `string \| null` | 必須 | 完了時刻。非nullならRFC 3339 UTC |
|  | `required` | `boolean` | 任意 | 既知のRequired Check集合に含まれるか。不明なら省略 |
|  | `detail_state` | `enum(not_registered, pending, passed, failed, cancelled, unavailable, unknown)` | 任意 | 個別checkの詳細状態。省略時は`state`だけから細分を推測しない |
|  | `app_id` | `integer` | 任意 | GitHub App由来checkを区別する識別子。情報源にない場合は省略 |
|  | `source` | `enum(github_check_runs, github_commit_statuses, unknown)` | 任意 | 個別状態を観測した情報源 |
| `RequiredCheck` | `name` | `string` | 必須 | Ruleset / 設定に宣言されたcheck名またはcontext |
|  | `app_id` | `integer` | 任意 | 宣言に含まれる場合のGitHub App ID |
| `RequiredCheckSet` | `state` | `enum(known, unknown)` | 必須 | Required Check集合を確定できたか |
|  | `checks` | `array<RequiredCheck>` | 必須 | 既知なら集合の全要素。不明なら空配列（空集合を意味しない） |
|  | `source` | `enum(github_ruleset, trusted_configuration, unknown)` | 必須 | 集合の根拠となる情報源 |
|  | `observed_at` | `string (format: date-time) \| null` | 必須 | 集合を取得した時刻。不明または取得不能ならnull |

`CiCheck`の既存`state`は互換性のため残し、`detail_state`は次の意味で使う。`not_registered`は既知のRequired Checkに対応するstatus/check runが対象SHAで見つからない状態、`unavailable`はGitHub API等からそのcheck状態を取得できない状態、`cancelled`はGitHubがcheckを取消済みと報告した状態である。`unknown`は取得情報だけではcheck状態を確定できない状態を表す。`cancelled`は既存`state`上では`failed`へ写像し、`not_registered`と`unavailable`は`unknown`へ写像する。新consumerは詳細を`detail_state`で読む。

`RequiredCheckSet.state=unknown`では`checks: []`は未知の集合を意味し、空のRequired Check集合ではない。この場合、取得できたcheckがすべて成功でもaggregate stateは`unknown`とする。逆に`state=known`かつ`checks: []`は、Required Checkが存在しないと確認できた空集合を意味する。空集合では成功を証明するcheckがないため、aggregate stateは同じく`unknown`とする。空集合に対して「全checkが成功」を空虚に真として`passed`にしてはならない。

集約stateは既存enumを保ち、次の順に判定する。

| 条件 | 集約`state` |
| --- | --- |
| Required Check集合が欠落 / `unknown`、または`known`だが空 | `unknown` |
| 非空の既知集合で、required checkに`failed`または`cancelled`がある | `failed` |
| 失敗がなく、required checkが`unavailable` / `unknown`、または対応を一意に確定できない | `unknown` |
| 上記がなく、required checkが`pending` / `not_registered` | `pending` |
| 非空の既知集合の全required checkが一意に対応し、すべて`passed` | `passed` |

この優先順により、既知の必須checkの失敗は他checkの未完了や観測不能より優先する。任意check（`required: false`）の状態は記録するが、集約stateの判定には使わない。

`not_registered`は既知のRequired Checkに対応するstatus/check runが対象SHAでまだ見つからない状態である。個別の互換`state`では`unknown`、`detail_state`では`not_registered`とし、集約では未完了のrequired checkとして`pending`に扱う。`unavailable`はcheckを照会できなかった状態であり、集約を成功または失敗と断定しない。失敗または取消済みのrequired checkが別に観測されている場合は`failed`を優先する。

`source`は集合の取得元であり、取得できない場合も試行した元を返す。元を特定できない場合は`unknown`とする。`source=github_ruleset`は対象PRのbase branchに適用されるactive RulesetのRequired Checkを表す。`app_id`がRequired Check宣言と観測checkの両方にある場合は照合に用いる。宣言と観測を一意に対応づけられないcheckは成功扱いせず、詳細stateを`unknown`とする。

外側のresponseにある`required_checks`自体は任意である。objectを返す場合は`state`、`checks`、`source`、`observed_at`をすべて含める。`state=known`なら`observed_at`はnullにせず、取得時刻を記録する。

個別checkの`source`はcheck状態の取得元、`RequiredCheckSet.source`は必須集合の取得元であり、両者を混同しない。GitHub API / CLIの取得失敗はArtifactやProvider実行の失敗ではなく、集合またはcheckを`unknown` / `unavailable`として記録する。各観測の`observed_at`はAPI応答を取得した時刻で、`CiCheck.completed_at`はGitHubが報告したcheck完了時刻である。

既存v2 producerとの互換性のため、これらのresponse fieldは任意である。#79準拠の新producerは`required_checks` object、`detail_state`、check状態の`source`を返す。context recordには`observed_at`も返す。Required Check集合が既知なら各`CiCheck.required`へtrue / falseを設定し、集合がunknownならこのfieldを省略する。GitHub App IDを取得できない場合は`app_id`を省略する。取得不能や情報不足はfieldの欠落や成功値で表さず、定義した`unknown` / `unavailable` stateで返す。consumerは旧producerから任意fieldが省略されていてもunknownとして扱える。この追加はresponseへのoptional fieldだけで、request、既存必須field、既存enum、`schema_version`を変更しない。

代表例: 既知で非空のRequired Checkがすべて成功した場合だけ`passed`となる。

~~~json
{
  "schema_version": "v2",
  "observation_id": "ci-001",
  "target": {
    "repository": "owner/repo",
    "pull_request_number": 42,
    "head_sha": "0123456789abcdef0123456789abcdef01234567"
  },
  "observed_at": "2026-09-25T03:00:00Z",
  "checks": [
    {
      "name": "build",
      "state": "passed",
      "url": null,
      "completed_at": "2026-09-25T02:59:00Z",
      "required": true,
      "detail_state": "passed",
      "app_id": 1234,
      "source": "github_check_runs"
    }
  ],
  "state": "passed",
  "required_checks": {
    "state": "known",
    "checks": [{"name": "build", "app_id": 1234}],
    "source": "github_ruleset",
    "observed_at": "2026-09-25T03:00:00Z"
  }
}
~~~

Rulesetを確認できた結果、Required Checkが一つもない場合は`state=known`と空の`checks`を返す。この空集合では`passed`と判断できないため、CI集約は`unknown`である。観測された任意checkが成功していても同じ規則を適用する。

~~~json
{
  "schema_version": "v2",
  "observation_id": "ci-002",
  "target": {
    "repository": "owner/repo",
    "pull_request_number": 42,
    "head_sha": "0123456789abcdef0123456789abcdef01234567"
  },
  "observed_at": "2026-09-25T03:00:00Z",
  "checks": [],
  "state": "unknown",
  "required_checks": {
    "state": "known",
    "checks": [],
    "source": "github_ruleset",
    "observed_at": "2026-09-25T03:00:00Z"
  }
}
~~~

Required Check集合そのものを取得できない場合も、観測されたcheckの状態から集合を推測しない。Required Checkであることが確認できない成功checkがあっても、aggregate stateは`unknown`である。

~~~json
{
  "schema_version": "v2",
  "observation_id": "ci-003",
  "target": {
    "repository": "owner/repo",
    "pull_request_number": 42,
    "head_sha": "0123456789abcdef0123456789abcdef01234567"
  },
  "observed_at": "2026-09-25T03:00:00Z",
  "checks": [
    {
      "name": "build",
      "state": "passed",
      "url": null,
      "completed_at": "2026-09-25T02:59:00Z",
      "source": "github_check_runs"
    }
  ],
  "state": "unknown",
  "required_checks": {
    "state": "unknown",
    "checks": [],
    "source": "github_ruleset",
    "observed_at": null
  }
}
~~~

### `ContextPage`

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `items` | `array<ContextItem>` | 必須 | sectionに属する履歴record。section固有の`details`型を使う |
| `next_cursor` | `string \| null` | 必須 | 次page取得用token。nullは現在のsnapshotに続きがない |

cursorは不透明であり、Task・section・page size・snapshot revisionに束縛される。次pageでは同じpage sizeと、同じsectionのcursorを使う。revision変更等で利用できないcursorは`invalid_cursor`。履歴は`occurred_at`降順、同時刻ならID降順。page境界に重複・欠落を作らない。

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
  "details": {"requested_provider_id": "provider-a", "requested_model": {"kind": "named", "model": "model-a"}, "observed_provider_id": "provider-a", "observed_model_id": null, "role": "implementer", "input_artifact_id": null, "base_commit": "abc123", "output_artifact_id": "artifact-01", "diagnostic_ref": null}
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
|  | `required_checks` | `RequiredCheckSet` | 任意 | Required Check集合。fieldがなければunknownとして読む |

`UsageMetric`と`CiTarget`のfieldはすべて必須。

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

成功outputは次のfieldを含み、省略可否は表の`Required`欄に従う。`passed`は観測対象SHAに限る。

| Field | JSON type | Required | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v2"` | 必須 | 契約version |
| `observation_id` | `string` | 必須 | observation ID |
| `target` | `CiTarget` | 必須 | 観測対象 |
| `observed_at` | `string (format: date-time)` | 必須 | 観測時刻 |
| `checks` | `array<CiCheck>` | 必須 | 個別check結果 |
| `state` | `enum(pending, passed, failed, unknown)` | 必須 | 集約結果 |
| `required_checks` | `RequiredCheckSet` | 任意 | Required Check集合。fieldがなければunknownとして読む |

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

secret、認証token、環境変数値、Provider認証情報をcontextやlogに返さない。既知secretは保存前と返却前にredactする。規則はArtifact本文、full diff、diagnostic本文、publication payloadにも適用する。redactできない本文は返さず、`forbidden` errorと必要な場合の権限付き参照を返す。redactionできない本文を空文字、`unknown`値、成功として偽装しない。log末尾やArtifact本文の取得量は要求範囲に限定する。

## ドメインの意味

Task、Attempt、Artifact、ValidationResult、ReviewVerdict、CodexDecisionの意味は[ドメインモデル](domain-model.md)を正本とする。とくにAttemptの`Succeeded`はProvider呼出しの正常終了のみであり、Validation成功、review承認、CodexDecisionのaccepted、publication、Task完了とは別の事実である。
