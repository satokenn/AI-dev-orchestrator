# MCP操作の入力・出力詳細仕様

## 読者・前提・適用する版

まず[全体像](architecture.md)と[操作の概要](mcp-operation-contract.md)を読む。本書はその操作の入力項目・出力項目・エラーを実装や照合に使う参照資料である。目的や分担を確認するレビューで、本書を先頭から読む必要はない。

**ここに記載する形式はv1である**。 [統合設計の適用版](operation-service-integration-design.md#根拠と適用版)は承認済みPR #84のモデル指定変更とPR #88のCI観測変更を適用したv2を対象にする。本PRはその変更を本書の表へ取り込んだり、v2の実装を提供したりするものではない。両者を同じ版の入力として混ぜない。モデル選定用の別仕様にある整数`schema_version: 1`とも区別する。

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

入力・出力の形式はJSON Schema 2020-12に対応する。各表の「省略できるか」には、入力する側・応答する側がその項目を含める必要があるかを記す。「必須」は省略できない項目、「任意」は省略できる項目、「条件付き」は指定された条件に応じて必要になる項目を表す。

項目の省略と、値として`null`を渡すことは異なる。たとえば、必須項目の型が`string | null`なら、その項目を含めた上で、文字列または`null`を入れる。説明文の日本語は送受信する値ではない。

MCPの`inputSchema`が入力形式、任意の`outputSchema`が構造化された出力形式を定める。[MCP Tools仕様（2026-07-28）](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)と[JSON Schemaの必須項目](https://json-schema.org/understanding-json-schema/reference/object#required-properties)に従い、#45の実装は本書の型・必須項目・列挙した値に合わせて入力を検査する。

## 型と共通規則

| 表記 | JSON型・制約 |
| --- | --- |
| `string` | JSON 文字列。特記があれば`enum`、`format`、最大長等を適用 |
| `integer` | 整数値。更新番号、取得件数、連番等に使う |
| `number` | 数値。使用量等に使う |
| `boolean` | `true`または`false` |
| `object` | JSONオブジェクト。項目の型と省略できる条件は対応する表で定義 |
| `array<T>` | T型の要素を持つJSON 配列 |
| `T \| null` | T型またはJSON `null`。項目自体は省略できない |
| `enum(a, b)` | 記載した文字列値のみ許可 |

すべての操作で、入力の`params.arguments`は`schema_version: "v1"`を必須とする。未対応版は`unsupported_schema_version`。不明な入力項目は`invalid_request`とし、副作用前に拒否する。成功する操作の出力の`structuredContent`はすべて`schema_version: "v1"`を含む。業務エラーはMCPの操作実行エラー（`isError: true`）として返し、`content`に下記の種類を識別できるエラーを含める。MCP入力の付随情報や通信規約上のエラーはこのアプリケーション契約の対象外。

DBの更新や外部処理の実行を伴う入力は`request_id: string`を必須とする。既存Taskを変更する場合はさらに`task_id: string`と`expected_revision: integer`を必須とする。読取入力は`request_id`と`expected_revision`を持たない。

通信が切れて依頼への応答を受け取れなかった場合、呼出側は同じ依頼を送り直せる。Serviceは再送で作業を二重に作ったり、モデルを二重に起動したりしないよう、次の3項目の組合せで受付済みの依頼を探す。この組合せを「冪等性キー」と呼ぶ。`+`は文字列の連結ではなく、3項目を組み合わせて識別するという意味である。

| 項目 | 誰が用意し、何を表すか |
| --- | --- |
| `caller` | MCP接続側が確定してServiceへ渡す呼出元の識別子。操作の入力JSONで自己申告する項目ではない |
| `tool name` | `task.create`や`attempt.run`など、依頼した操作の名前 |
| `request_id` | 呼出側が依頼に付ける識別子。同じ依頼を再送するときは同じ値を使う |

3項目がすべて同じで、比較用に形式を揃えた入力内容も一致する場合は、保存済みの応答を返す。DB更新や外部処理は繰り返さない。同じ3項目で入力内容が異なる場合は、同じ依頼のIDを別の内容へ使い回したものとして`idempotency_conflict`で拒否する。呼出元や操作名が異なれば、`request_id`だけが同じでも別の依頼として扱う。

たとえば、`task.create`の応答が届かず、同じ呼出元が同じ操作名・`request_id`・入力内容で再送した場合、最初に作成したTaskの受付結果を返す。別のTaskは作らない。この重複防止は、`task.create`を含め、DBを書き換える操作や外部処理を行う操作すべてに適用する。読取専用の操作には適用しない。

入力内容を比較するときに形式を揃えることを、統合設計では「正規化」と呼ぶ。ただし、省略と`null`を同一とするかなど、何を同じ内容とみなすかの細則は未確定である。[統合設計の残る仕様確認](operation-service-integration-design.md#説明の補足と仕様の確定を分ける)を参照する。

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

### この文書で区別する確認処理

従来「検査」とまとめていた処理には、次の違いがある。入力の形式や権限を確認することと、成果物に対してテストを実行することは、別の処理である。

| 処理 | 対象と動作 | 結果の扱い |
| --- | --- | --- |
| 入力・実行条件の確認 | MCP接続処理とServiceが、項目の型、更新番号、対象の所属、権限、予算、許可されたコマンドや作業場所を確認する | 条件違反なら、該当するDB更新や外部処理を始める前に拒否する |
| 成果物の検証（Validation） | Validatorが、指定した保存済み成果物を対象に、ビルド・テスト・静的解析などのコマンドを実行する | コマンドごとの結果と、検証全体の結果を記録する。成果物の採用判断とは別 |
| 秘密情報の検出・伏せ字化 | SecretScannerが、保存・返却・公開する内容に既知の秘密情報が含まれるか確認し、必要な箇所を伏せ字にする | 秘密情報を含む公開や、安全に確認できない公開は拒否する。コマンドそのものが伏せ字化で変わる場合も実行しない |
| CI結果の取得 | GitHub・CI接続処理が、指定したPRやコミットについて外部で実行されたCIの状態を取得する | `CiCheck`として記録する。`ci.get`や`ci.wait`がテストを新たに実行するわけではない |

Validationでいう一つの「検査」（check）は、名前・実行コマンド・引数・制限時間を指定した一回のコマンド実行を指す。入力は`ValidationCheck`、その結果は`ValidationCheckResult`で表す。「検査セット」（profile）は、こうしたコマンドの組合せに、起動側が名前を付けて登録したものである。

たとえば、起動側が`rust-checks`という名前にfmt・clippy・testを登録した場合、監督Codexはその名前を指定して実行を依頼できる。結果には、各コマンドの名前、`passed`（成功）・`failed`（失敗）・`unknown`（結果不明）のいずれか、診断への参照が入る。`rust-checks`は説明用の名前であり、組込みの予約名ではない。

`diagnostic_ref`は、エラーなどを調べる診断記録を指す値であり、診断本文そのものではない。呼出側は値の内部構造を解釈しない。長時間処理の出力ログを読む操作は`operation.list_logs`である。同操作の`diagnostic`出力と、この参照値を使う取得方法との対応は、[残る仕様確認](operation-service-integration-design.md#説明の補足と仕様の確定を分ける)に記す。

コマンドや作業場所を許可する設定は[統合設計のValidator設定](operation-service-integration-design.md#validatorの許可設定)、秘密情報を含む入力の扱いは[秘密情報の確認手順](operation-service-integration-design.md#validation要求に含まれる値の秘密情報検査)に従う。

### `TaskRequest` / `TaskSnapshot`

`TaskRequest`は、作業作成時に受け取った依頼内容の記録である。`source`、タイトル、本文、制約と、Issueを元にした場合のIssue情報を保存する。`TaskSnapshot`は、この依頼内容に作業ID・現在の更新番号・現在の状態を添えて返す形式である。

`source`は依頼内容の出所を表し、次の二つを区別する。実装を人間とモデルのどちらが行うかを指定する値ではない。

| `source`の値 | どのように作業を登録するか |
| --- | --- |
| `issue` | 呼出側が取得したGitHub Issueの情報を、依頼内容とともに渡す |
| `manual` | Issueを出所とせず、タイトル・依頼本文・制約を直接渡す。監督Codexから渡す場合も含む |

| 型名 | 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- | --- |
| `TaskRequest` | `source` | `enum(issue, manual)` | 必須 | 依頼内容の出所。Issue情報を伴う`issue`か、依頼を直接渡す`manual` |
|  | `title` | `string` | 必須 | 作業のタイトル |
|  | `description` | `string` | 必須 | 要求本文 |
|  | `constraints` | `array<string>` | 必須 | 要求制約。制約なしは空配列 |
|  | `issue` | `IssueSnapshot \| null` | 必須 | `source=issue`ならJSONオブジェクト、`source=manual`ならnull |
| `TaskSnapshot` | `task_id` | `string` | 必須 | Task ID |
|  | `revision` | `integer` | 必須 | Task更新番号 |
|  | `state` | `enum(pending, active, completed, failed, cancelled)` | 必須 | Task状態 |
|  | `request` | `TaskRequest` | 必須 | 作業作成時に保存した依頼内容。出所、タイトル、本文、制約、Issue情報を含む |

ここで依頼内容を「固定する」とは、作業作成時に受け取った値を、その時点の依頼として保存することである。後から取得したIssueの内容で、この作成時の記録を置き換えない。現在のTaskの状態や更新番号とは分けて保持する。

`task.create`の入力では、`manual`なら`issue`項目を省略する。一方、保存結果として返す`TaskRequest`には`issue`項目が必ずあり、`manual`なら値は`null`となる。`issue`の場合は、入力にも保存結果にも`IssueSnapshot`を含める。

`IssueSnapshot`は次のJSONオブジェクトとする。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `url` | `string (format: uri)` | 必須 | Issue URL |
| `number` | `integer (minimum: 1)` | 必須 | Issue番号 |
| `title` | `string` | 必須 | 取得時点のタイトル |
| `body` | `string` | 必須 | 取得時点の本文 |

### `EvidenceRef`

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `kind` | `enum(validation, review, decision, publication, ci)` | 必須 | 参照する記録の種別 |
| `id` | `string` | 必須 | Serviceが発行した記録ID |

根拠として指定するのは、結果の申告ではなく、保存済み記録への参照である。Serviceは、指定した記録が存在するか、対象Taskに属するか、どのArtifactを対象にしているかを照合する。CIの根拠となる記録は対象Publicationのリポジトリ、PR、公開先コミットの識別子が一致しなければならない。存在しないIDは`invalid_request`、別Artifactに属する記録は`evidence_artifact_mismatch`。

### `ContextItem`

`task.get_context`では、モデル実行の履歴を`attempts`、成果物の検証履歴を`validations`などの分類に分けて取得する。各分類の一覧には、1件ずつの記録を`ContextItem`として返す。

各記録に共通するID・状態・要約などを次の表に示す。共通項目だけでは表せない内容は`details`に入る。たとえば、モデル実行の記録には実行先やモデル名、検証の記録には対象成果物や各コマンドの結果を入れる。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `id` | `string` | 必須 | 履歴記録ID |
| `kind` | `string` | 必須 | この1件の記録の種類。例: `attempt`はモデル実行、`validation`は成果物の検証 |
| `state` | `string \| null` | 必須 | 記録種別に定義された状態。状態を持たない記録はnull |
| `occurred_at` | `string (format: date-time)` | 必須 | 記録時刻（RFC 3339 UTC） |
| `summary` | `string` | 必須 | 人が読める短い要約 |
| `references` | `array<Reference>` | 必須 | 関連記録 |
| `details` | `object` | 必須 | この種類の記録に必要な具体的な項目。モデル実行なら実行先、検証なら対象成果物と各コマンドの結果など |

次の表は、どの分類にどの種類の記録が入り、`details`に何を含めるかを示す。たとえば分類`validations`には`kind: "validation"`の記録が入り、その`details`に`validation_id`、`artifact_id`、`check_profile_id`、`checks`を含める。列挙した項目はすべて必須であり、`null`を許す項目はその型に明記する。

| 情報の分類 | 種類 | 項目 | JSON型 | 省略できるか | 意味 |
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
|  |  | `check_profile_id` | `string \| null` | 必須 | 実行したコマンドの組合せを起動側が登録した名前。`checks`でコマンドを直接指定した場合はnull |
|  |  | `checks` | `array<ValidationCheckResult>` | 必須 | 実行対象の各コマンドについて、名前・成功か失敗か不明か・診断への参照を返す一覧 |
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
|  |  | `checks` | `array<CiCheck>` | 必須 | GitHubから取得したCIチェックごとの名前・状態・URL・完了時刻の一覧 |

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

| 型名 | 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- | --- |
| `Reference` | `kind` | `string` | 必須 | 参照先記録の種別 |
|  | `id` | `string` | 必須 | 参照先記録ID |
| `ValidationCheckResult` | `name` | `string` | 必須 | このコマンド実行に付けた名前。例: `test` |
|  | `state` | `enum(passed, failed, unknown)` | 必須 | このコマンド実行の結果。成功・失敗・結果不明を区別する |
|  | `diagnostic_ref` | `string \| null` | 必須 | 診断参照。なければnull |
| `CiCheck` | `name` | `string` | 必須 | GitHub側で実行されたCIチェックの名前 |
|  | `state` | `enum(pending, passed, failed, unknown)` | 必須 | 観測した状態 |
|  | `url` | `string \| null` | 必須 | 検査 URL。なければnull |
|  | `completed_at` | `string \| null` | 必須 | 完了時刻。非nullならRFC 3339 UTC |

### `ContextPage`

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `items` | `array<ContextItem>` | 必須 | 指定した分類の記録一覧。各記録の`details`は、前述の分類別の表に従う |
| `next_cursor` | `string \| null` | 必須 | 続きの取得時に渡す値。nullなら、この一覧の続きをすべて返し終えている |

続きの取得位置は中身を解釈せずに使い、Task・情報の分類・取得件数・一覧を固定した時点の更新番号に結び付ける。次ページでは同じ取得件数と、同じ情報の分類の続きの取得位置を使う。更新番号の変更等で続きの取得位置を利用できない場合は`invalid_cursor`。履歴は`occurred_at`降順、同時刻ならID降順。ページ境界に重複・欠落を作らない。

### `OperationAcceptance`

長時間処理を受け付けたときの共通出力。処理は応答後にも進むため、受付時点では最終結果を返さない。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 対応する要求ID |
| `task_id` | `string` | 必須 | 対象Task ID |
| `revision` | `integer` | 必須 | 受付後のTaskの更新番号 |
| `operation` | `OperationRef` | 必須 | 新規operation |
| `attempt_id` | `string` | 条件付き | Attemptを作る操作が返すAttempt ID |

`attempt_id`は`attempt.run`で必須、ほかの非同期操作では省略する。

`OperationRef`の項目はすべて必須。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `operation_id` | `string` | 必須 | 処理ID |
| `kind` | `enum(attempt.run, task.cancel, operation.cancel, validation.run, publication.publish, ci.wait)` | 必須 | 受付対象の操作 |
| `state` | `enum(accepted, running, completed, failed, cancelling, cancelled, recovery_required)` | 必須 | operation状態 |
| `submitted_at` | `string (format: date-time)` | 必須 | 受付時刻 |

受付は処理成功を意味しない。最終結果は`operation.get`で取得する。

### `ErrorPayload`

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string \| null` | 必須 | 読み取れるなら対象要求ID。取得不能ならnull |
| `error` | `Error` | 必須 | 業務エラー |

`Error`の次の項目はすべて必須。

| 項目 | JSON型 | 省略できるか | 意味 |
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

各入力 / 出力表はJSON Schemaの`properties`に相当する項目を列挙する。表の「省略できるか」は「必須」「任意」「条件付き」のいずれか。条件付き項目の条件は表の直後に記す。未知入力項目は拒否し、未知出力項目は無視する。全成功出力には`schema_version: const "v1"`を含む。

### `task.create`

Taskを作成する。`request_id`は再送を識別するキーに含まれる。

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 重複作成を防ぐ要求ID |
| `source` | `enum(issue, manual)` | 必須 | 要求の出所 |
| `title` | `string` | 必須 | 作業のタイトル |
| `description` | `string` | 必須 | 要求本文 |
| `constraints` | `array<string>` | 必須 | 要求制約。なしなら空配列 |
| `issue` | `IssueSnapshot` | 条件付き | 要求の出所が`issue`なら必須、`manual`なら省略 |

`source`の`issue`と`manual`は、[依頼内容の記録](#taskrequest--tasksnapshot)で定義した出所を表す。`issue`項目には、呼出側が取得した時点のIssueのURL・番号・タイトル・本文を渡す。Serviceはこれを作成時の依頼として保存する。

成功出力

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 入力で受け取った要求IDをそのまま返す |
| `task_id` | `string` | 必須 | 新規Task ID |
| `revision` | `integer` | 必須 | 初期更新番号 |
| `state` | `const "pending"` | 必須 | 初期作業の状態 |
| `request` | `TaskRequest` | 必須 | 作業作成時に受け取って保存した出所・タイトル・依頼本文・制約・Issue情報 |

例（操作 `arguments`）

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

上の例は、Issueを使わず「区切り文字のエスケープに対応する」と直接依頼する入力である。`issue`項目は省略している。返る`request`には、出所・タイトル・依頼本文・制約と`issue: null`を含む。通信上の`schema_version`と`request_id`は、`request`内の項目ではない。

同じ呼出元が`task.create`を同じ`request_id`・同じ入力で再送した場合は、同じ作成結果を返す。入力内容を変えて再利用すると`idempotency_conflict`となる。

### `task.get_context`

Taskと、選択した分類の情報を、記録した時点の内容で読む。読取専用。

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `task_id` | `string` | 必須 | 読むTask |
| `sections` | `array<enum(providers, usage, attempts, artifacts, validations, reviews, decisions, publication, ci)>` | 必須 | 返す履歴情報の分類。重複不可 |
| `page_size` | `integer (minimum: 1, maximum: 100)` | 任意、既定20 | 各情報の分類から返す最大件数 |
| `cursors` | `object<string, string>` | 任意 | 情報の分類名と、その分類の次ページ用の続きの取得位置の対応表 |

成功出力

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `task` | `TaskSnapshot` | 必須 | 作業ID、現在の更新番号と状態、作業作成時に保存した依頼内容 |
| `sections` | `object<string, ContextPage>` | 必須 | 要求された情報の分類ごとのページ |
| `observed_at` | `string (format: date-time)` | 必須 | この応答で返す作業情報と履歴を取得した時刻 |

例（`sections.attempts.items`の一部）

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

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 呼出側が依頼に付けるID。同じ依頼を再送するときは同じ値 |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `provider_id` | `string` | 必須 | 呼び出すProvider |
| `model_id` | `string` | 必須 | 呼び出すModel |
| `instruction` | `string` | 必須 | Providerへ渡す依頼 |
| `role` | `enum(implementer, reviewer, explorer)` | 必須 | Attemptの担当 |
| `input` | `ArtifactInput \| BaseInput` | 必須 | 入力Artifactまたは開始時点のコードのどちらか一方 |
| `timeout_ms` | `integer (minimum: 1)` | 任意 | 実行の制限時間。省略時はServiceの実行許可設定で定めた値 |

`ArtifactInput`と`BaseInput`の各項目はすべて必須。

| 型名 | 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- | --- |
| `ArtifactInput` | `artifact_id` | `string` | 必須 | 入力Artifact ID |
| `BaseInput` | `repository` | `string` | 必須 | 開始時コミットのリポジトリ |
|  | `commit` | `string` | 必須 | 開始時コミット SHA |

入力Artifactは同じTaskに属する必要がある。ブランチ名やパスだけの開始時点のコード指定は認めない。

成功出力は`OperationAcceptance`。`attempt_id`を必須とする。受付時の`operation.state`は`accepted`。

### `operation.get`

operation状態・結果を読む。読取専用。

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `operation_id` | `string` | 必須 | 取得対象 |

成功出力

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `operation` | `Operation` | 必須 | operation記録 |

`Operation`の項目はすべて必須。nullを許す項目も省略せず、該当しない場合はnullを返す。

| 項目 | JSON型 | 省略できるか | 意味 |
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

| 操作の種類 | 結果項目 | JSON型 | 省略できるか | 意味 |
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
|  | `checks` | `array<ValidationCheckResult>` | 必須 | コマンドごとの名前・状態・診断への参照の一覧 |
| `publication.publish` | `publication_id` | `string` | 必須 | Publication ID |
|  | `repository` | `string` | 必須 | リポジトリ |
|  | `head_sha` | `string` | 必須 | 公開コミット |
|  | `pull_request_number` | `integer` | 必須 | 作成したPR番号 |
|  | `pull_request_url` | `string (format: uri)` | 必須 | PR URL |
| `ci.wait` | `observation_id` | `string` | 必須 | CI 観測記録ID |
|  | `target` | `CiTarget` | 必須 | 観測対象 |
|  | `observed_at` | `string (format: date-time)` | 必須 | 観測時刻 |
|  | `state` | `enum(pending, passed, failed, unknown)` | 必須 | 集約結果 |
|  | `checks` | `array<CiCheck>` | 必須 | GitHubから取得したCIチェックごとの名前・状態・URL・完了時刻の一覧 |

`UsageMetric`と`CiTarget`の項目はすべて必須。

| 型名 | 項目 | JSON型 | 省略できるか | 意味 |
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

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `operation_id` | `string` | 必須 | 対象operation |
| `stream` | `enum(stdout, stderr, diagnostic)` | 必須 | 読む出力の種類 |
| `cursor` | `string` | 任意 | 次ページ用の続きの取得位置 |
| `limit` | `integer (minimum: 1, maximum: 1000)` | 任意、既定100 | 取得するログの一片の最大件数 |

成功出力は次の項目をすべて含む。`LogChunk`の項目もすべて必須。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `chunks` | `array<LogChunk>` | 必須 | 取得したログの一片 |
| `next_cursor` | `string \| null` | 必須 | 次ページ用の続きの取得位置。続きがなければnull |

| ログ一片の項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `sequence` | `integer` | 必須 | operation内のログの一片順 |
| `occurred_at` | `string (format: date-time)` | 必須 | ログ記録時刻 |
| `text` | `string` | 必須 | 伏せ字化した本文 |
| `redacted` | `boolean` | 必須 | 本文内に伏せ字化を行ったか |

続きの取得位置はoperation・出力の種類・limitに束縛される。末尾到達はoperation完了を意味しない。

### `operation.cancel`

ひとつのoperationへの取消を要求する。

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 呼出側が依頼に付けるID。同じ依頼を再送するときは同じ値 |
| `task_id` | `string` | 必須 | 対象operationのTask |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `operation_id` | `string` | 必須 | 取消対象 |

成功出力は`OperationAcceptance`。`operation`は取消要求を表す新operation。対象operationが停止するまでは取消完了ではない。

### `task.cancel`

Taskと未終了operationの取消を要求する。

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 呼出側が依頼に付けるID。同じ依頼を再送するときは同じ値 |
| `task_id` | `string` | 必須 | 取消対象 |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `reason` | `string` | 任意 | 取消理由 |

成功出力は`OperationAcceptance`。operationの完了前にTaskの取消完了とは扱わない。

### `validation.run`

保存済み成果物を対象に、ビルド・テスト・静的解析などのコマンドを実行する。起動側がコマンドの組合せに付けた名前（`check_profile_id`）を指定するか、実行するコマンドを`checks`に直接渡す。

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 呼出側が依頼に付けるID。同じ依頼を再送するときは同じ値 |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `artifact_id` | `string` | 必須 | 検証対象 |
| `check_profile_id` | `string` | 登録名で指定する場合は必須 | 起動側が登録したコマンドの組合せの名前。例: `rust-checks` |
| `checks` | `array<ValidationCheck>` | コマンドを直接指定する場合は必須 | 実行するコマンドの一覧。各要素に名前・コマンド・引数・制限時間を入れる |

`check_profile_id`か`checks`のちょうど一方を指定する。`ValidationCheck`の項目はすべて必須。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `name` | `string` | 必須 | 検査名 |
| `command` | `string` | 必須 | 許可一覧と照合する実行コマンド |
| `args` | `array<string>` | 必須 | コマンドの引数。なしなら空配列 |
| `timeout_ms` | `integer (minimum: 1)` | 必須 | 検査の制限時間 |

Serviceは、コマンドと作業場所が起動側の許可一覧に含まれるか確認してからValidatorへ渡す。登録名を使う場合も、実際に実行するコマンドをその登録内容から取得し、受付時の設定内容を保存する。これにより、後で設定が変わっても、何を実行する依頼だったかを確認できる。

受付応答は実行完了を意味しない。終了後に`operation.get`を呼ぶと、対象成果物、検証全体の状態、コマンドごとの結果を取得できる。`checks`の空配列を成功とは扱わず、結果不明として記録する。複数コマンドの結果から全体の状態を決める細則は、[残る仕様確認](operation-service-integration-design.md#説明の補足と仕様の確定を分ける)を参照する。

成功出力は`OperationAcceptance`の全項目と、次の必須項目を返す。最終結果の各項目は`operation.get`の`validation.run` 結果のデータ形式を参照。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `artifact_id` | `string` | 必須 | 検証対象Artifact |

### `decision.record`

監督Codexの判断をArtifactに記録する。同期処理。

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 呼出側が依頼に付けるID。同じ依頼を再送するときは同じ値 |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `artifact_id` | `string` | 必須 | 判断対象 |
| `decision` | `enum(accepted, rejected, changes_requested)` | 必須 | 判断 |
| `reason` | `string` | 必須 | 判断理由 |
| `evidence` | `array<EvidenceRef>` | 任意 | 判断に参照した保存済み証拠 |

成功出力の項目はすべて必須。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 入力で受け取った要求IDをそのまま返す |
| `task_id` | `string` | 必須 | 対象Task ID |
| `revision` | `integer` | 必須 | 記録後のTaskの更新番号 |
| `decision` | `CodexDecision` | 必須 | 保存された判断記録 |

`CodexDecision`の項目はすべて必須。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `decision_id` | `string` | 必須 | 判断記録ID |
| `artifact_id` | `string` | 必須 | 判断対象Artifact |
| `decision` | `enum(accepted, rejected, changes_requested)` | 必須 | 採否 |
| `reason` | `string` | 必須 | 判断理由 |
| `evidence` | `array<EvidenceRef>` | 必須 | 判断に照合した証拠。なしなら空配列 |

### `publication.publish`

指定した成果物をPRとして公開する。起動側が設定した許可条件に必要な根拠記録がなければ、公開を始める前に拒否する。

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 呼出側が依頼に付けるID。同じ依頼を再送するときは同じ値 |
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

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `artifact_id` | `string` | 必須 | 公開対象Artifact |

### `ci.get`

PR / コミットの検査状態を一度観測する。読取専用。

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `publication_id` | `string` | `target`未指定時に必須 | Publicationの指定 |
| `target` | `PullRequestTarget \| CommitTarget` | `publication_id`未指定時に必須 | PRまたはコミットの指定 |

`PullRequestTarget`と`CommitTarget`の項目はすべて必須。`publication_id`と`target`は同時に指定しない。

| 型名 | 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- | --- |
| `PullRequestTarget` | `repository` | `string` | 必須 | リポジトリ |
|  | `number` | `integer (minimum: 1)` | 必須 | PR番号 |
| `CommitTarget` | `repository` | `string` | 必須 | リポジトリ |
|  | `sha` | `string` | 必須 | コミット SHA |

成功出力は次の項目をすべて含む。`passed`は観測対象SHAに限る。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `observation_id` | `string` | 必須 | 観測記録ID |
| `target` | `CiTarget` | 必須 | 観測対象 |
| `observed_at` | `string (format: date-time)` | 必須 | 観測時刻 |
| `checks` | `array<CiCheck>` | 必須 | GitHubから取得したCIチェックごとの名前・状態・URL・完了時刻の一覧 |
| `state` | `enum(pending, passed, failed, unknown)` | 必須 | 集約結果 |

### `ci.wait`

CI状態を待機期限まで待つ。長時間処理。

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 呼出側が依頼に付けるID。同じ依頼を再送するときは同じ値 |
| `task_id` | `string` | 必須 | 対象Task |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `publication_id` | `string` | `target`未指定時に必須 | Publicationの指定 |
| `target` | `PullRequestTarget \| CommitTarget` | `publication_id`未指定時に必須 | PRまたはコミットの指定 |
| `deadline` | `string (format: date-time)` | 必須 | 待機期限（RFC 3339 UTC） |

`publication_id`と`target`は同時に指定しない。成功出力は`OperationAcceptance`。期限までにCIが確定しない場合、operationは`failed`、`result`はnull、`error.code`は`timeout`、`error.details_ref`は最後に永続化したCI観測記録（`CiObservation`）のIDを返す。CI結果が未確定でも観測記録自体を永続化できた場合はこの時間切れ応答であり、成功扱いではない。Serviceがoperationの終了状態または最後の観測値の永続化を確定できない中断の場合は`recovery_required`とし、結果を推測しない。

### `task.finish`

Task完了を要求する。指定されたArtifact、採用とした判断、起動側の許可規則で必須とする根拠記録を照合し、満たさなければ状態を変えない。

| 入力項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const "v1"` | 必須 | 形式の版 |
| `request_id` | `string` | 必須 | 呼出側が依頼に付けるID。同じ依頼を再送するときは同じ値 |
| `task_id` | `string` | 必須 | 完了対象Task |
| `expected_revision` | `integer` | 必須 | 最後に確認した更新番号 |
| `artifact_id` | `string` | 必須 | 完了対象Artifact |
| `decision_id` | `string` | 必須 | 同じArtifactへの採用判断（`accepted`のCodexDecision） |
| `evidence` | `array<EvidenceRef>` | 任意 | 追加で照合する証拠 |

成功出力の項目はすべて必須。

| 項目 | JSON型 | 省略できるか | 意味 |
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

`retryable: true`は同じ入力の再送許可を意味しない。DB更新や外部処理を伴う入力を作り直す場合は、新しい要求IDと最新更新番号を使う。時間切れや中断で実行結果を確定できない場合、Serviceは`recovery_required`を記録し、成功・失敗・未実行のいずれかを推測しない。状態確定までは同一operationを再実行せず、`operation.get`で確認する。

## 秘密情報の扱い

秘密情報、認証トークン、環境変数の値、実行先の認証情報を履歴やログに返さない。既知の秘密情報は保存前と返却前に伏せ字化する。この規則は成果物の本文、差分全体、診断本文、公開する操作データにも適用する。伏せ字化できない本文は返さず、`forbidden`エラーと、必要な場合は権限付きの参照を返す。返せない本文を空文字列や`unknown`、成功として偽装しない。ログや成果物本文の取得量は、依頼された範囲に限定する。

## ドメインの意味

Task、Attempt、Artifact、ValidationResult、ReviewVerdict、CodexDecisionの意味は[ドメインモデル](domain-model.md)を正本とする。とくにAttemptの`Succeeded`はProvider呼出しの正常終了のみであり、Validation成功、レビュー承認、CodexDecisionのaccepted、公開、Task完了とは別の事実である。
