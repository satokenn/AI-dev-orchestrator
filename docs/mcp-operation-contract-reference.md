# MCP操作の入力・出力詳細仕様

## 読者・前提・適用する版

まず[全体像](architecture.md)と[操作の概要](mcp-operation-contract.md)を読む。本書は、その操作の入力項目・出力項目・エラーを実装や照合に使う参照資料である。目的や分担を確認するレビューで、本書を先頭から読む必要はない。

**ここに記載する形式はv1である。** [統合設計の適用版](operation-service-integration-design.md#根拠と適用版)は、承認済みPR #84のモデル指定変更とPR #88のCI観測変更を適用したv2を対象にする。本PRはその変更を本書の表へ取り込んだり、v2の実装を提供したりするものではない。両者を同じ版の入力として混ぜない。モデル選定用の別仕様にある整数`schema_version: 1`とも区別する。

## この資料の使い方

1. どの操作を使うかは[操作一覧](mcp-operation-contract.md#依頼できる操作)で確認する。
2. 下の「型と共通規則」で、必須項目、同じ依頼の再送、更新番号の意味を確認する。
3. 「共通型」と、必要な操作の節で正確な入力・出力を確認する。
4. 失敗や結果不明の場合は末尾の「エラーと復旧」「秘密情報の扱い」を確認する。

表の英語はコードやJSONの項目名であり、そのまま送受信する値である。説明文では「入力」「出力」「項目」「操作」という日本語を使う。`operation`は受付済みの長時間処理を追跡する記録である。

| 確認する場面 | 読み方 |
| --- | --- |
| 長時間処理を依頼した直後 | `OperationAcceptance`は受付を示す。成功したとは判断しない |
| 長時間処理の結果を読む | `operation.get`の状態と結果を確認する |
| 検証・判断を根拠に使う | `EvidenceRef`で保存済みの記録を指定する。別の成果物の結果を使わない |
| 一覧の続きを読む | `ContextPage.next_cursor`をそのまま次の取得へ渡す |

## 通信形式との対応

操作の入力はMCP `tools/call`の`params.arguments`、成功時の構造化された出力は操作結果の`structuredContent`に対応する。MCPの付随情報や、JSON-RPCと操作結果の外側の包み方は、本書では再定義しない。

入力・出力の形式はJSON Schema 2020-12に対応する。各表の「必須性」は項目の省略可否を示し、値に`null`を使えるかとは別である。たとえば`string | null`は必須項目に文字列か`null`を入れることを意味し、「任意」は項目自体を省略できることを意味する。説明文の日本語は送受信する値ではない。

MCPの`inputSchema`が入力形式、任意の`outputSchema`が構造化された出力形式を定める。[MCP Tools仕様（2026-07-28）](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)と[JSON Schemaの必須項目](https://json-schema.org/understanding-json-schema/reference/object#required-properties)に従い、#45の実装は本書の型・必須項目・列挙した値に合わせて入力を検査する。

## 型と共通規則

| 表記 | JSON型・制約 |
| --- | --- |
| `string` | JSON 文字列。特記があれば`enum`、`format`、最大長等を適用 |
| `integer` | 整数値。更新番号、取得件数、連番等に使う |
| `number` | 数値。使用量等に使う |
| `boolean` | `true`または`false` |
| `object` | JSONオブジェクト。項目の型・必須性は対応する表で定義 |
| `array<T>` | T型の要素を持つJSON 配列 |
| `T \| null` | T型またはJSON `null`。項目自体は省略できない |
| `enum(a, b)` | 記載した文字列値のみ許可 |

すべての操作の入力の`params.arguments`は`schema_version: "v1"`を必須とする。未対応版は`unsupported_schema_version`。不明な入力項目は`invalid_request`とし、副作用前に拒否する。成功する操作の出力の`structuredContent`はすべて`schema_version: "v1"`を含む。業務エラーはMCPの操作実行エラー（`isError: true`）として返し、`content`に下記の種類を識別できるエラーを含める。MCP入力の付随情報や通信規約上のエラーはこのアプリケーション契約の対象外。

DBの更新や外部処理の実行を伴う入力は`request_id: string`を必須とする。既存Taskを変更する場合はさらに`task_id: string`と`expected_revision: integer`を必須とする。読取入力は`request_id`と`expected_revision`を持たない。

同じ依頼の再送を識別するキー（冪等性キー）の範囲は`caller + tool name + request_id`。`task.create`を含むDB更新や外部処理を伴うすべての入力に適用する。同じキーで、形式を揃えた入力内容も同じ場合は保存済みの応答を返し、DB更新や外部処理を繰り返さない。同じキーで入力内容が異なる場合は`idempotency_conflict`で拒否する。

IDはすべて文字列として扱い、呼出側はIDの形式・連番・内部構造を解釈しない。日時はRFC 3339形式の文字列（例: `2026-10-10T15:00:00Z`）として送受信し、UTCを示す`Z`を使う。項目が任意なら省略し、nullを使うのは型に`| null`と明記された場合だけ。

### 状態名を読むときの対応

| 値 | 意味 |
| --- | --- |
| `accepted` / `queued` | 受付済み / モデル呼び出し待ち |
| `running` | 実行中 |
| `completed` / `succeeded` | 処理完了 / モデル呼び出し正常終了。何の状態かで区別する |
| `failed` | 処理失敗 |
| `cancelling` / `cancelled` | 取消要求を処理中 / 停止確認済み |
| `recovery_required` | 結果を確定できず、復旧確認が必要 |
| `passed` / `unknown` | 検査成功 / 確認不能 |
| `approved` / `inconclusive` | レビュー担当の承認 / 判断不能 |
| `accepted`（採否判断） / `rejected` / `changes_requested` | 監督Codexの採用 / 不採用 / 修正要求 |

同じ文字列でも記録の種類が違う場合がある。各表に列挙した値だけを使い、受付・実行成功・検証成功・採用判断を置き換えない。

## 共通型

### `TaskRequest` / `TaskSnapshot`

| 型名 | 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- | --- |
| `TaskRequest` | `source` | `enum(issue, manual)` | 必須 | 要求の出所 |
|  | `title` | `string` | 必須 | 作業のタイトル |
|  | `description` | `string` | 必須 | 要求本文 |
|  | `constraints` | `array<string>` | 必須 | 要求制約。制約なしは空配列 |
|  | `issue` | `IssueSnapshot \| null` | 必須 | `source=issue`ならJSONオブジェクト、`source=manual`ならnull |
| `TaskSnapshot` | `task_id` | `string` | 必須 | Task ID |
|  | `revision` | `integer` | 必須 | Task更新番号 |
|  | `state` | `enum(pending, active, completed, failed, cancelled)` | 必須 | Task状態 |
|  | `request` | `TaskRequest` | 必須 | 作成時点で固定した要求 |

`IssueSnapshot`は次のJSONオブジェクトとする。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `url` | `string (format: uri)` | 必須 | Issue URL |
| `number` | `integer (minimum: 1)` | 必須 | Issue番号 |
| `title` | `string` | 必須 | 取得時点のタイトル |
| `body` | `string` | 必須 | 取得時点の本文 |

### `EvidenceRef`

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `kind` | `enum(validation, review, decision, publication, ci)` | 必須 | 参照する記録の種別 |
| `id` | `string` | 必須 | Serviceが発行した記録ID |

根拠となる記録は結果を申告する項目ではなく、保存済み記録への参照である。Serviceは存在、Task所属、対象Artifactを照合する。CI 根拠となる記録は対象Publicationのリポジトリ、PR、公開先コミットの識別子が一致しなければならない。存在しないIDは`invalid_request`、別Artifactに属する記録は`evidence_artifact_mismatch`。

### `ContextItem`

各`task.get_context` 情報の分類は次の共通項目を返す。`details`の型は情報の分類ごとの表で定義する。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `id` | `string` | 必須 | 履歴記録ID |
| `kind` | `string` | 必須 | 記録種別 |
| `state` | `string \| null` | 必須 | 記録種別に定義された状態。状態を持たない記録はnull |
| `occurred_at` | `string (format: date-time)` | 必須 | 記録時刻（RFC 3339 UTC） |
| `summary` | `string` | 必須 | 人が読める短い要約 |
| `references` | `array<Reference>` | 必須 | 関連記録 |
| `details` | `object` | 必須 | 情報の分類固有の追加情報 |

情報の分類固有の`details`は次の項目で構成する。列挙した項目はすべて必須であり、null可能な項目はその型に明記する。

| 情報の分類 | 種類 | 項目 | JSON型 | 必須性 | 意味 |
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
| `attempts` | `attempt` | `provider_id` | `string` | 必須 | Provider ID |
|  |  | `model_id` | `string` | 必須 | Model ID |
|  |  | `role` | `enum(implementer, reviewer, explorer)` | 必須 | Attemptの役割 |
|  |  | `input_artifact_id` | `string \| null` | 必須 | 入力Artifact。開始時点のコードから開始した場合はnull |
|  |  | `base_commit` | `string \| null` | 必須 | 開始時コミット。なければnull |
|  |  | `output_artifact_id` | `string \| null` | 必須 | 出力Artifact。未作成ならnull |
|  |  | `diagnostic_ref` | `string \| null` | 必須 | 診断参照。なければnull |
| `artifacts` | `artifact` | `artifact_id` | `string` | 必須 | Artifact ID |
|  |  | `digest` | `string` | 必須 | Artifact内容のdigest |
|  |  | `source_attempt_id` | `string \| null` | 必須 | 作成元Attempt。なければnull |
|  |  | `base_commit` | `string \| null` | 必須 | 差分の基準コミット。なければnull |
|  |  | `diff_ref` | `string \| null` | 必須 | 差分参照。なければnull |
| `validations` | `validation` | `validation_id` | `string` | 必須 | Validation ID |
|  |  | `artifact_id` | `string` | 必須 | 検証対象Artifact |
|  |  | `check_profile_id` | `string \| null` | 必須 | 適用検査セット。明示checksならnull |
|  |  | `checks` | `array<ValidationCheckResult>` | 必須 | 個別検査結果 |
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
|  |  | `repository` | `string` | 必須 | リポジトリ |
|  |  | `head_sha` | `string` | 必須 | 公開コミット |
|  |  | `pull_request_number` | `integer` | 必須 | PR番号 |
|  |  | `pull_request_url` | `string (format: uri)` | 必須 | PR URL |
| `ci` | `ci_observation` | `observation_id` | `string` | 必須 | CI 観測記録ID |
|  |  | `repository` | `string` | 必須 | 対象リポジトリ |
|  |  | `pull_request_number` | `integer \| null` | 必須 | PR番号。コミット指定ならnull |
|  |  | `head_sha` | `string` | 必須 | 観測対象コミット |
|  |  | `state` | `enum(pending, passed, failed, unknown)` | 必須 | 集約状態 |
|  |  | `checks` | `array<CiCheck>` | 必須 | 個別検査結果 |

`ContextItem.state`の値は次のとおり。情報の分類ごとに別のenumであり、一覧にない状態を使わない。

| 情報の分類 | `state` type |
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

次のJSONオブジェクト型を使用する。記載した項目はすべて必須。

| 型名 | 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- | --- |
| `Reference` | `kind` | `string` | 必須 | 参照先記録の種別 |
|  | `id` | `string` | 必須 | 参照先記録ID |
| `ValidationCheckResult` | `name` | `string` | 必須 | 検査名 |
|  | `state` | `enum(passed, failed, unknown)` | 必須 | 検査結果 |
|  | `diagnostic_ref` | `string \| null` | 必須 | 診断参照。なければnull |
| `CiCheck` | `name` | `string` | 必須 | CIの検査名 |
|  | `state` | `enum(pending, passed, failed, unknown)` | 必須 | 観測した状態 |
|  | `url` | `string \| null` | 必須 | 検査 URL。なければnull |
|  | `completed_at` | `string \| null` | 必須 | 完了時刻。非nullならRFC 3339 UTC |

### `ContextPage`

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `items` | `array<ContextItem>` | 必須 | 情報の分類に属する履歴記録。情報の分類固有の`details`型を使う |
| `next_cursor` | `string \| null` | 必須 | 次ページ取得用トークン。nullは現在の時点固定記録に続きがない |

続きの取得位置（続きの取得位置）は中身を解釈せずに使い、Task・情報の分類・取得件数・一覧を固定した時点の更新番号に結び付ける。次ページでは同じ取得件数と、同じ情報の分類の続きの取得位置を使う。更新番号変更等で利用できない続きの取得位置は`invalid_cursor`。履歴は`occurred_at`降順、同時刻ならID降順。ページ境界に重複・欠落を作らない。

### `OperationAcceptance`

長時間処理を受け付けたときの共通出力。処理は応答後にも進むため、受付時点では最終結果を返さない。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 対応する要求ID |
| `task_id` | `string` | 必須 | 対象Task ID |
| `revision` | `integer` | 必須 | 受付後のTaskの更新番号 |
| `operation` | `OperationRef` | 必須 | 新規operation |
| `attempt_id` | `string` | 条件付き | Attemptを作る操作が返すAttempt ID |

`attempt_id`は`attempt.run`で必須、ほかの非同期操作では省略する。

`OperationRef`の項目はすべて必須。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `operation_id` | `string` | 必須 | 処理ID |
| `kind` | `enum(attempt.run, task.cancel, operation.cancel, validation.run, publication.publish, ci.wait)` | 必須 | 受付対象の操作 |
| `state` | `enum(accepted, running, completed, failed, cancelling, cancelled, recovery_required)` | 必須 | operation状態 |
| `submitted_at` | `string (format: date-time)` | 必須 | 受付時刻 |

受付は処理成功を意味しない。最終結果は`operation.get`で取得する。

### `ErrorPayload`

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string \| null` | 必須 | 読み取れるなら対象要求ID。取得不能ならnull |
| `error` | `Error` | 必須 | 業務エラー |

`Error`の次の項目はすべて必須。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `code` | `ErrorCode` | 必須 | 下記の業務エラーコード |
| `message` | `string` | 必須 | 人が読める説明 |
| `retryable` | `boolean` | 必須 | 同じ入力を再送してよいかではなく、新しい入力を作成して回復可能か |
| `current_task_revision` | `integer \| null` | 必須 | 更新番号の不一致エラー時の現在値。それ以外はnull |
| `operation_id` | `string \| null` | 必須 | 関連operation。なければnull |
| `details_ref` | `string \| null` | 必須 | 追加診断参照。なければnull |

`ErrorCode`は`invalid_request`、`unsupported_schema_version`、`task_not_found`、`stale_revision`、`idempotency_conflict`、`busy`、`unknown_provider`、`unknown_model`、`policy_denied`、`budget_exhausted`、`workspace_boundary_violation`、`artifact_not_found`、`artifact_task_mismatch`、`evidence_artifact_mismatch`、`invalid_state_transition`、`operation_not_found`、`not_cancellable`、`timeout`、`cancelled`、`interrupted`、`recovery_required`、`invalid_cursor`、`forbidden`、`internal_error`のいずれか。

`Error.details_ref`は、種類が定まった追加の記録を取得するための参照である。参照文字列の中身を呼出側で解釈しない。`ci.wait`が期限切れになった場合は、最後に永続化した`CiObservation`のIDを指す。診断本文を直接エラーへ埋め込まない。

`interrupted`はエラーコードでありoperationの状態ではない。中断して終了結果を確定できない処理は`recovery_required`になり、復旧状態が確定するまで同じ処理を再実行して外部処理を重ねない。再起動後もServiceはこの状態を保持し、`operation.get`で確認できる。復旧確認前の`operation.cancel`は`not_cancellable`で拒否する。

## 操作ごとの入力・出力

各入力 / 出力表はJSON Schemaの`properties`に相当する項目を列挙する。表の「必須性」は「必須」「任意」「条件付き」のいずれか。条件付き項目の条件は表の直後に記す。未知入力項目は拒否し、未知出力項目は無視する。全成功出力には`schema_version: const "v1"`を含む。

### `task.create`

Taskを作成する。`request_id`は再送を識別するキーに含まれる。

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 重複作成を防ぐ要求ID |
| `source` | `enum(issue, manual)` | 必須 | 要求の出所 |
| `title` | `string` | 必須 | 作業のタイトル |
| `description` | `string` | 必須 | 要求本文 |
| `constraints` | `array<string>` | 必須 | 要求制約。なしなら空配列 |
| `issue` | `IssueSnapshot` | 条件付き | 要求の出所が`issue`なら必須、`manual`なら省略 |

`issue`は前述の`IssueSnapshot`型を使う。タイトルと本文は作成時点で固定した記録。

成功出力:

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 入力で受け取った要求IDをそのまま返す |
| `task_id` | `string` | 必須 | 新規Task ID |
| `revision` | `integer` | 必須 | 初期更新番号 |
| `state` | `const "pending"` | 必須 | 初期作業の状態 |
| `request` | `TaskRequest` | 必須 | 保存したsource・要求・制約・Issue 時点固定記録 |

例（操作 `arguments`）:

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

同じ`request_id`・同じ入力は同じ作成結果を返す。入力内容違いの再利用は`idempotency_conflict`。

### `task.get_context`

Taskと選択した情報の分類の時点固定記録を読む。読取専用。

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `task_id` | `string` | 必須 | 読むTask |
| `sections` | `array<enum(providers, usage, attempts, artifacts, validations, reviews, decisions, publication, ci)>` | 必須 | 返す履歴情報の分類。重複不可 |
| `page_size` | `integer (minimum: 1, maximum: 100)` | 任意、既定20 | 各情報の分類から返す最大件数 |
| `cursors` | `object<string, string>` | 任意 | 情報の分類名と、その分類の次ページ用の続きの取得位置の対応表 |

成功出力:

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `task` | `TaskSnapshot` | 必須 | ID、更新番号、状態、要求時点固定記録 |
| `sections` | `object<string, ContextPage>` | 必須 | 要求された情報の分類ごとのページ |
| `observed_at` | `string (format: date-time)` | 必須 | 時点固定記録観測時刻 |

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

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 再送を識別するキー |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `provider_id` | `string` | 必須 | 呼び出すProvider |
| `model_id` | `string` | 必須 | 呼び出すModel |
| `instruction` | `string` | 必須 | Providerへ渡す依頼 |
| `role` | `enum(implementer, reviewer, explorer)` | 必須 | Attemptの担当 |
| `input` | `ArtifactInput \| BaseInput` | 必須 | 入力Artifactまたは開始時点のコードのどちらか一方 |
| `timeout_ms` | `integer (minimum: 1)` | 任意 | 実行の制限時間。省略時はServiceの実行許可設定で定めた値 |

`ArtifactInput`と`BaseInput`の各項目はすべて必須。

| 型名 | 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- | --- |
| `ArtifactInput` | `artifact_id` | `string` | 必須 | 入力Artifact ID |
| `BaseInput` | `repository` | `string` | 必須 | 開始時コミットのリポジトリ |
|  | `commit` | `string` | 必須 | 開始時コミット SHA |

入力Artifactは同じTaskに属する必要がある。ブランチ名やパスだけの開始時点のコード指定は認めない。

成功出力は`OperationAcceptance`。`attempt_id`を必須とする。受付時の`operation.state`は`accepted`。

### `operation.get`

operation状態・結果を読む。読取専用。

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `operation_id` | `string` | 必須 | 取得対象 |

成功出力:

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `operation` | `Operation` | 必須 | operation記録 |

`Operation`の項目はすべて必須。nullを許す項目も省略せず、該当しない場合はnullを返す。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `operation_id` | `string` | 必須 | 処理ID |
| `task_id` | `string` | 必須 | 対象Task ID |
| `kind` | `enum(attempt.run, task.cancel, operation.cancel, validation.run, publication.publish, ci.wait)` | 必須 | 実行した操作 |
| `state` | `enum(accepted, running, completed, failed, cancelling, cancelled, recovery_required)` | 必須 | operation状態 |
| `submitted_at` | `string (format: date-time)` | 必須 | 受付時刻 |
| `started_at` | `string \| null` | 必須 | 開始時刻 |
| `completed_at` | `string \| null` | 必須 | 終了時刻 |
| `result` | `OperationResult \| null` | 必須 | 確定した結果 |
| `error` | `Error \| null` | 必須 | 失敗情報 |

`result`と`error`は同時に非nullにならない。`OperationResult`は`kind`に対応する次のJSONオブジェクトのいずれか。

| 操作の種類 | 結果項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- | --- |
| `attempt.run` | `attempt_id` | `string` | 必須 | 作成したAttempt ID |
|  | `attempt_state` | `enum(succeeded, failed, cancelled)` | 必須 | Provider実行の終端状態 |
|  | `output_artifact_id` | `string \| null` | 必須 | 出力Artifact。生成されなければnull |
|  | `usage` | `array<UsageMetric>` | 必須 | Provider使用量 |
|  | `diagnostic_ref` | `string \| null` | 必須 | 診断参照。なければnull |
| `task.cancel` | `task_state` | `enum(cancelled, active)` | 必須 | 取消後の作業の状態 |
|  | `cancelled_operation_ids` | `array<string>` | 必須 | 取消した処理ID |
| `operation.cancel` | `target_operation_id` | `string` | 必須 | 取消対象operation |
|  | `target_state` | `enum(cancelled, running, recovery_required)` | 必須 | 対象operationの結果状態 |
| `validation.run` | `validation_id` | `string` | 必須 | Validation ID |
|  | `artifact_id` | `string` | 必須 | 検証対象Artifact |
|  | `state` | `enum(passed, failed, unknown)` | 必須 | 集約結果 |
|  | `checks` | `array<ValidationCheckResult>` | 必須 | 個別検査結果 |
| `publication.publish` | `publication_id` | `string` | 必須 | Publication ID |
|  | `repository` | `string` | 必須 | リポジトリ |
|  | `head_sha` | `string` | 必須 | 公開コミット |
|  | `pull_request_number` | `integer` | 必須 | 作成したPR番号 |
|  | `pull_request_url` | `string (format: uri)` | 必須 | PR URL |
| `ci.wait` | `observation_id` | `string` | 必須 | CI 観測記録ID |
|  | `target` | `CiTarget` | 必須 | 観測対象 |
|  | `observed_at` | `string (format: date-time)` | 必須 | 観測時刻 |
|  | `state` | `enum(pending, passed, failed, unknown)` | 必須 | 集約結果 |
|  | `checks` | `array<CiCheck>` | 必須 | 個別検査結果 |

`UsageMetric`と`CiTarget`の項目はすべて必須。

| 型名 | 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- | --- |
| `UsageMetric` | `name` | `string` | 必須 | 指標名 |
|  | `value` | `number \| null` | 必須 | 値。不明ならnull |
|  | `unit` | `string` | 必須 | 単位 |
|  | `basis` | `enum(measured, configured, computed, estimated, unknown)` | 必須 | 値の根拠 |
| `CiTarget` | `repository` | `string` | 必須 | リポジトリ |
|  | `pull_request_number` | `integer \| null` | 必須 | PR番号。コミット指定ならnull |
|  | `head_sha` | `string` | 必須 | 観測対象SHA |

### `operation.list_logs`

指定operationのログ範囲を読む。ログ本文は伏せ字化済み。

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `operation_id` | `string` | 必須 | 対象operation |
| `stream` | `enum(stdout, stderr, diagnostic)` | 必須 | 読む出力の種類 |
| `cursor` | `string` | 任意 | 次ページ用の続きの取得位置 |
| `limit` | `integer (minimum: 1, maximum: 1000)` | 任意、既定100 | 取得するログの一片の最大件数 |

成功出力は次の項目をすべて含む。`LogChunk`の項目もすべて必須。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `chunks` | `array<LogChunk>` | 必須 | 取得したログの一片 |
| `next_cursor` | `string \| null` | 必須 | 次ページ用の続きの取得位置。続きがなければnull |

| ログ一片の項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `sequence` | `integer` | 必須 | operation内のログの一片順 |
| `occurred_at` | `string (format: date-time)` | 必須 | ログ記録時刻 |
| `text` | `string` | 必須 | 伏せ字化した本文 |
| `redacted` | `boolean` | 必須 | 本文内に伏せ字化を行ったか |

続きの取得位置はoperation・出力の種類・limitに束縛される。末尾到達はoperation完了を意味しない。

### `operation.cancel`

ひとつのoperationへの取消を要求する。

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 再送を識別するキー |
| `task_id` | `string` | 必須 | 対象operationのTask |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `operation_id` | `string` | 必須 | 取消対象 |

成功出力は`OperationAcceptance`。`operation`は取消要求を表す新operation。対象operationが停止するまでは取消完了ではない。

### `task.cancel`

Taskと未終了operationの取消を要求する。

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 再送を識別するキー |
| `task_id` | `string` | 必須 | 取消対象 |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `reason` | `string` | 任意 | 取消理由 |

成功出力は`OperationAcceptance`。operationの完了前にTaskの取消完了とは扱わない。

### `validation.run`

保存済み成果物に機械検証を実行する。登録済みの検査セットを識別名で指定するか、検査コマンドを直接指定する。

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 再送を識別するキー |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `artifact_id` | `string` | 必須 | 検証対象 |
| `check_profile_id` | `string` | 検査セット利用時に必須 | 登録済み検査セット |
| `checks` | `array<ValidationCheck>` | 明示検査利用時に必須 | 実行する検査 |

`check_profile_id`か`checks`のちょうど一方を指定する。`ValidationCheck`の項目はすべて必須。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `name` | `string` | 必須 | 検査名 |
| `command` | `string` | 必須 | 許可一覧と照合する実行コマンド |
| `args` | `array<string>` | 必須 | コマンドの引数。なしなら空配列 |
| `timeout_ms` | `integer (minimum: 1)` | 必須 | 検査の制限時間 |

実行コマンドと作業場所は、起動側が設定した許可一覧を使って実行前に検査する。

成功出力は`OperationAcceptance`の全項目と、次の必須項目を返す。最終結果の各項目は`operation.get`の`validation.run` 結果のデータ形式を参照。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `artifact_id` | `string` | 必須 | 検証対象Artifact |

### `decision.record`

監督Codexの判断をArtifactに記録する。同期処理。

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 再送を識別するキー |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `artifact_id` | `string` | 必須 | 判断対象 |
| `decision` | `enum(accepted, rejected, changes_requested)` | 必須 | 判断 |
| `reason` | `string` | 必須 | 判断理由 |
| `evidence` | `array<EvidenceRef>` | 任意 | 判断に参照した保存済み証拠 |

成功出力の項目はすべて必須。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 入力で受け取った要求IDをそのまま返す |
| `task_id` | `string` | 必須 | 対象Task ID |
| `revision` | `integer` | 必須 | 記録後のTaskの更新番号 |
| `decision` | `CodexDecision` | 必須 | 保存された判断記録 |

`CodexDecision`の項目はすべて必須。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `decision_id` | `string` | 必須 | 判断記録ID |
| `artifact_id` | `string` | 必須 | 判断対象Artifact |
| `decision` | `enum(accepted, rejected, changes_requested)` | 必須 | 採否 |
| `reason` | `string` | 必須 | 判断理由 |
| `evidence` | `array<EvidenceRef>` | 必須 | 判断に照合した証拠。なしなら空配列 |

### `publication.publish`

指定した成果物をPRとして公開する。起動側が設定した許可条件に必要な根拠記録がなければ、公開を始める前に拒否する。

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 再送を識別するキー |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `artifact_id` | `string` | 必須 | 公開するArtifact |
| `decision_id` | `string` | 必須 | このArtifactに対する保存済みの採用判断（`accepted`のCodexDecision） |
| `base_branch` | `string` | 必須 | PRのマージ先ブランチ |
| `head_branch` | `string` | 必須 | PRの変更元ブランチ |
| `title` | `string` | 必須 | PRのタイトル |
| `body` | `string` | 必須 | PRの本文 |
| `evidence` | `array<EvidenceRef>` | 任意 | 採用判断以外に追加で照合する証拠 |

公開前にServiceは`decision_id`で保存済み採用判断を検索し、採用判断が`accepted`で、その`artifact_id`が公開対象と一致することを確認する。不在・不一致・採用以外の判断なら`invalid_state_transition`で拒否する。加えてServiceはArtifactと公開時に渡す内容を起動側が設定した秘密情報検査の規則で検査する。秘密情報検出または検査を安全に完了できない場合は`policy_denied`とし、コミット・push・PR作成を開始しない。通常のValidation成功だけでは秘密情報検査済みを意味しない。

成功出力は`OperationAcceptance`の全項目と、次の必須項目を返す。最終結果は`operation.get`の`publication.publish` 結果のデータ形式を参照。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `artifact_id` | `string` | 必須 | 公開対象Artifact |

### `ci.get`

PR / コミットの検査状態を一度観測する。読取専用。

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `publication_id` | `string` | `target`未指定時に必須 | Publicationの指定 |
| `target` | `PullRequestTarget \| CommitTarget` | `publication_id`未指定時に必須 | PRまたはコミットの指定 |

`PullRequestTarget`と`CommitTarget`の項目はすべて必須。`publication_id`と`target`は同時に指定しない。

| 型名 | 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- | --- |
| `PullRequestTarget` | `repository` | `string` | 必須 | リポジトリ |
|  | `number` | `integer (minimum: 1)` | 必須 | PR番号 |
| `CommitTarget` | `repository` | `string` | 必須 | リポジトリ |
|  | `sha` | `string` | 必須 | コミット SHA |

成功出力は次の項目をすべて含む。`passed`は観測対象SHAに限る。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `observation_id` | `string` | 必須 | 観測記録ID |
| `target` | `CiTarget` | 必須 | 観測対象 |
| `observed_at` | `string (format: date-time)` | 必須 | 観測時刻 |
| `checks` | `array<CiCheck>` | 必須 | 個別検査結果 |
| `state` | `enum(pending, passed, failed, unknown)` | 必須 | 集約結果 |

### `ci.wait`

CI状態を待機期限まで待つ。長時間処理。

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 再送を識別するキー |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `publication_id` | `string` | `target`未指定時に必須 | Publicationの指定 |
| `target` | `PullRequestTarget \| CommitTarget` | `publication_id`未指定時に必須 | PRまたはコミットの指定 |
| `deadline` | `string (format: date-time)` | 必須 | 待機期限（RFC 3339 UTC） |

`publication_id`と`target`は同時に指定しない。成功出力は`OperationAcceptance`。期限までにCIが確定しない場合、operationは`failed`、`result`はnull、`error.code`は`timeout`、`error.details_ref`は最後に永続化したCI観測記録（`CiObservation`）のIDを返す。CI結果が未確定でも観測記録自体を永続化できた場合はこの時間切れ応答であり、成功扱いではない。Serviceがoperationの終了状態または最後の観測値の永続化を確定できない中断の場合は`recovery_required`とし、結果を推測しない。

### `task.finish`

Task完了を要求する。指定されたArtifact、採用とした判断、起動側の許可規則で必須とする根拠記録を照合し、満たさなければ状態を変えない。

| 入力項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 再送を識別するキー |
| `task_id` | `string` | 必須 | 完了対象Task |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `artifact_id` | `string` | 必須 | 完了対象Artifact |
| `decision_id` | `string` | 必須 | 同じArtifactへの採用判断（`accepted`のCodexDecision） |
| `evidence` | `array<EvidenceRef>` | 任意 | 追加で照合する証拠 |

成功出力の項目はすべて必須。

| 項目 | JSON型 | 必須性 | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 入力で受け取った要求IDをそのまま返す |
| `task_id` | `string` | 必須 | 完了したTask ID |
| `revision` | `integer` | 必須 | 完了後のTaskの更新番号 |
| `state` | `const "completed"` | 必須 | 完了状態 |
| `artifact_id` | `string` | 必須 | 完了対象Artifact |
| `evidence` | `array<EvidenceRef>` | 必須 | 照合した証拠。なければ空配列 |

## エラーと復旧

`stale_revision`、`policy_denied`等の業務エラーはMCPの操作実行エラーとして返し、`isError: true`にする。エラーの詳細は`ErrorPayload`に従う。データ形式/JSON-RPC構造不正などMCP通信規約上のエラーと、業務上の操作エラーを混同しない。

`retryable: true`は同じ入力の再送許可を意味しない。DB更新や外部処理を伴う入力を作り直す場合は、新しい要求IDと最新更新番号を使う。時間切れや中断で実行結果を確定できない場合、Serviceは`recovery_required`を記録し、成功・失敗・未実行のいずれかを推測しない。状態確定までは同一operationの再実行をせず、`operation.get`で確認する。

## 秘密情報の扱い

秘密情報、認証トークン、環境変数の値、実行先の認証情報を履歴やログに返さない。既知の秘密情報は保存前と返却前に伏せ字化する。この規則は成果物の本文、差分全体、診断本文、公開する操作データにも適用する。伏せ字化できない本文は返さず、`forbidden`エラーと、必要な場合は権限付きの参照を返す。返せない本文を空文字列や`unknown`、成功として偽装しない。ログや成果物本文の取得量は、依頼された範囲に限定する。

## ドメインの意味

Task、Attempt、Artifact、ValidationResult、ReviewVerdict、CodexDecisionの意味は[ドメインモデル](domain-model.md)を正本とする。とくにAttemptの`Succeeded`はProvider呼出しの正常終了のみであり、Validation成功、レビュー承認、CodexDecisionのaccepted、公開、Task完了とは別の事実である。
