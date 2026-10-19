# モデル選定の入力・出力詳細仕様

## 読者・目的・適用する版

[モデル選定の概要](model-selection-spec.md)を読んだ実装担当者とレビュー担当者が、Rustから監督Codexへ渡すJSON、監督CodexからRustへ返すJSON、受け取った結果の確認条件を照合するための資料である。モデルを選ぶ目的と判断の流れは概要に置き、本書では項目・型・値・省略条件を扱う。

ここで扱う選定用形式は`schema_version: 1`である。MCP操作の`schema_version: "v1"`や`"v2"`とは別の形式であり、混ぜない。CLIやAPIから情報を取得する方法、MCP操作への接続実装、DBへの保存方法を新たに定める文書ではない。

省略と`null`、状態値、結果不明の検証要約には[未確定事項](#未確定の送信形式)がある。構造が列挙されていても、送信仕様が完成したとは扱わない。

## この資料の使い方

1. 「共通規則」と「共通のデータ形式」で、値の読み方と取得根拠を確認する。
2. 「入力の定義」「出力の定義」で、最上位から入れ子の項目へ進む。
3. 入力・出力それぞれのJSON例と照合する。
4. 「Rustが行う検証」と「未確定の送信形式」で、拒否条件と残る仕様不足を確認する。

各項目表はMCPの詳細仕様と同じく「項目・JSON型・省略できるか・意味」で読む。`string`は文字列、`integer`は整数、`boolean`は`true`または`false`、`object`は項目を持つオブジェクト、`array<T>`はT型の配列、`const`は固定値、`enum`は列挙した値を表す。「必須」は省略不可、「未確定」は既存仕様から省略条件を確定できないことを示す。値に`null`を入れることと項目の省略は別である。

## 共通規則

- 入力と出力の形式の版は、両方とも`schema_version`で示す。この選定形式のv1は整数`1`であり、MCP操作の文字列`"v1"`とは別の形式である。
- Rust側が選定要求ごとに`request_id`を生成する。監督Codexは同じ値を返し、別の要求の選定結果を混ぜない。
- 実行先（Provider）とモデル（Model）は別の識別子にし、実行対象には両方を指定する。
- モデル指定は、名前を指定する`named`か、実行先の既定モデルを使う`provider_default`のどちらかにする。省略や空文字列で代用しない。
- 情報を取得できない場合は`unknown`と理由を残す。ゼロ・空文字列・推定値で補わない。
- 金額・トークン数・要求回数などは値と単位を組にする。異なる単位を一つの得点へ換算しない。
- 推定値は比較材料として使えるが、利用可否や強制する上限・予算の検査を上書きしない。
- 監督Codexが返す選定結果には、作業・実行の状態、利用可否・使用量・制限、履歴を含めない。Rust側が観測した事実を選定結果から書き換えないためである。

## 共通のデータ形式

### 値を得た方法

利用量や残予算等の値には、数値だけでなく、その値をどのように得たかを付ける。

| 表現 | 意味 | 例 |
| --- | --- | --- |
| `measured` | 実行先のAPI等から実測した | 現在までのトークン使用量 |
| `configured` | 利用者やリポジトリ側が設定した | 1日の予算上限 |
| `computed` | 他の既知の値から計算した | 上限から使用量を引いた残量 |
| `estimated` | 過去実績等から見積もった | 次の1実行にかかる費用 |
| `unknown` | 取得または計算できない | 実行先が残量を返さない |

`unknown`を`0`や推定値へ置き換えず、取得できない理由と確認時刻を保持します。

### モデルの指定

候補と選定結果には、次のモデル指定を使う。名前を指定する形式と、実行先の既定モデルを使う形式を区別する。後述の過去の実行履歴では、実際に使ったモデルが分からない場合を別に表す。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `kind` | `enum(named, provider_default)` | 必須 | モデル名を指定するか、実行先の既定モデルを使うか |
| `model` | `string` | 条件付き | `kind=named`でモデル名を指定する。`provider_default`はこの項目を持たない |

### 利用可否

実行先とモデル候補の`availability`は、同じ形式を使う。`status`を別のオブジェクトへ入れ子にしない。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `status` | `enum(available, unavailable, unknown)` | 必須 | 利用可能・利用不可・確認不能の区別 |
| `observed_at_ms` | `integer` | 必須 | 利用可否を確認したUnix時刻。単位はミリ秒 |
| `source` | `object` | 必須 | 下記「情報の取得元」の形式 |
| `reason` | `string`（空文字列不可） | 条件付き | `unavailable`または`unknown`の場合に必要な理由 |

### 分かる値と分からない値

使用量・金額・残量・件数などには、値と取得根拠を一緒に渡す。Rust型案の`Evidence<T>`に対応し、`T`は各項目に入る値の型である。

#### 値が分かる場合

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `status` | `const "known"` | 必須 | 値が分かることを示す |
| `value` | 項目に応じた型 | 必須 | 量なら「量と単位」のオブジェクト、件数なら非負整数、時刻なら整数。履歴の利用量は計測項目の配列 |
| `basis` | `enum(measured, configured, computed, estimated)` | 必須 | 実測・設定値・計算値・推定値の区別 |
| `assessed_at_ms` | `integer` | 必須 | 値を取得または評価したUnix時刻。単位はミリ秒 |
| `source` | `object` | 必須 | 下記「情報の取得元」の形式 |

#### 値が分からない場合

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `status` | `const "unknown"` | 必須 | 値を取得または計算できなかったことを示す |
| `reason` | `string` | 必須 | 取得できない理由 |
| `assessed_at_ms` | `integer` | 必須 | 取得または評価を行ったUnix時刻。単位はミリ秒 |
| `source` | `object` | 必須 | 下記「情報の取得元」の形式 |

### 情報の取得元

利用可否と値の取得根拠の`source`に入れる項目である。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `kind` | `enum(provider_api, provider_cli, execution_ledger, repository_config)` | 必須 | API・CLI・記録DB・リポジトリ設定のどれから得たか |
| `reference` | `string` | 必須 | 取得元や計算元を追うための情報 |

### 量と単位

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `amount` | `string` | 必須 | 金額・トークン数などの量を十進数の文字列で表す |
| `unit` | `string` | 必須 | `input_token`、`output_token`、`request`、通貨コードなどの単位 |

### 期間

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `starts_at_ms` | `integer` | 必須 | 期間の開始。Unix時刻のミリ秒 |
| `ends_at_ms` | `integer` | 必須 | 期間の終了。Unix時刻のミリ秒 |

### 利用制限

実行先全体とモデル固有の`limits`に入れる各要素である。同じ事実を両方に複製しない。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `name` | `string` | 必須 | 要求回数など、制限する項目の名前 |
| `scope` | `string` | 必須 | アカウントやプロジェクトなど、制限が適用される範囲 |
| `enforcement` | `enum(hard, advisory)` | 必須 | 実行許可の条件か、比較の参考情報か |
| `window` | `object` | 未確定 | 上記「期間」の形式。期間なしの場合の省略・nullの区別は未確定 |
| `limit` | `object` | 必須 | 上限と取得根拠。分かる場合のvalueは「量と単位」の形式 |
| `used` | `object` | 必須 | 使用量と取得根拠。分かる場合のvalueは「量と単位」の形式 |
| `remaining` | `object` | 必須 | 残量と取得根拠。分かる場合のvalueは「量と単位」の形式 |
| `resets_at_ms` | `object` | 必須 | リセット時刻と取得根拠。分かる場合のvalueはUnix時刻のミリ秒を表す整数 |

### API使用量

実行先とモデル候補の`api_usage`が値を持つ場合の形式である。対象外の場合だけ値なしを許し、その省略可否は未確定である。対象だが取得できない値は、各計測項目の`unknown`で表す。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `scope` | `string` | 必須 | アカウント・組織・プロジェクト・リポジトリ予算など、値の適用範囲 |
| `window` | `object` | 必須 | 上記「期間」の形式 |
| `actual_usage` | `array<object>` | 必須 | 下記「計測項目」の形式。実使用量 |
| `configured_budget` | `array<object>` | 必須 | 下記「予算項目」の形式。設定済みの予算 |
| `remaining_budget` | `array<object>` | 必須 | 下記「予算項目」の形式。残予算 |

### 計測項目

実使用量と次回実行の推定値の各要素である。過去の実行の利用量が分かる場合も、この形式の配列を使う。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `name` | `string` | 必須 | 費用やトークン数など、何の値かを示す名前 |
| `value` | `object` | 必須 | 分かる値または分からない値と、その取得根拠。分かる値には「量と単位」の形式を使う |

### 予算項目

設定予算と残予算の各要素である。未設定の予算を架空の要素として追加しない。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `name` | `string` | 必須 | 予算の対象となる計測項目の名前 |
| `enforcement` | `enum(hard, advisory)` | 必須 | 実行許可の条件か、比較の参考情報か |
| `value` | `object` | 必須 | 予算の量と取得根拠。分かる値には「量と単位」の形式を使う |

### 過去実績

実行先またはモデル候補の`performance`に入れる各要素である。件数の各項目には、値が分かるか不明かと取得根拠を渡す。分かる場合のvalueは非負整数である。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `role` | `string`（空文字列不可） | 未確定 | 担当別に集計する場合の役割。全担当をまとめた場合の省略・nullの区別は未確定 |
| `window` | `object` | 必須 | 集計対象の期間 |
| `attempts` | `object` | 必須 | モデル実行の件数と取得根拠 |
| `succeeded` | `object` | 必須 | モデル実行が成功した件数と取得根拠 |
| `failed` | `object` | 必須 | モデル実行が失敗した件数と取得根拠 |
| `cancelled` | `object` | 必須 | モデル実行を取り消した件数と取得根拠 |
| `validation_passed` | `object` | 必須 | 成果物の検証が成功した件数と取得根拠 |
| `validation_failed` | `object` | 必須 | 成果物の検証が失敗した件数と取得根拠 |
| `review_approved` | `object` | 必須 | レビューで承認された件数と取得根拠 |
| `review_changes_requested` | `object` | 必須 | レビューで修正を求められた件数と取得根拠 |
| `review_inconclusive` | `object` | 必須 | レビューで判断できなかった件数と取得根拠 |
| `retries` | `object` | 必須 | 再試行の件数と取得根拠 |

集計方法と値の取得方法は[過去実績と現在の実行履歴](#過去実績と現在の実行履歴)に従う。

## 入力の定義

ここでは、JSON例やRustの型定義を読まずに、Rustが何をどこに入れて渡すか確認できるよう、各オブジェクトの項目を列挙する。項目名と構造は既存の本文・型案に対応する。各項目表の「必須」は省略できないことを示す。配列の各要素は、対応する表の構造を持つ。

一方、元の型案で`Option`となっている項目は、値がないことを表せても、それをJSONで省略するか`null`にするかが一律には決まっていない。そのような箇所は表で未確定と示す。[未確定の送信形式](#未確定の送信形式)が残るため、この文書を実装可能な完成仕様とは扱わない。

### 最上位の6項目

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const 1` | 必須 | 選定用の入出力形式のバージョン |
| `request_id` | `string` | 必須 | Rustが今回の選定依頼に付けるID。監督Codexは出力にも同じ値を入れる |
| `captured_at_ms` | `integer` | 必須 | Rustがこの入力を作成したUnix時刻。単位はミリ秒 |
| `task` | `object` | 必須 | 下記「作業と選定する担当」の構造 |
| `providers` | `array<object>` | 必須 | 下記「実行先とモデル候補」の構造。各要素にその実行先の`models`を含める |
| `current_attempts` | `array<object>` | 必須 | 同じTaskの実行を`sequence`順に並べる。各要素は下記「現在の実行履歴」の構造 |

### 作業と選定する担当

`task`に入れる項目は次のとおり。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `task_id` | `string` | 必須 | どの作業のモデルを選ぶか |
| `objective` | `string` | 必須 | 作業の目的 |
| `constraints` | `array<string>` | 必須 | 利用者が指定した制約 |
| `state` | 未確定。状態を表す値 | 必須 | Taskの状態。例は`"active"`。全状態のJSON表記は未確定 |
| `issue` | `object` | 未確定 | Issue起点の場合の情報。Issueを使わない場合の省略・`null`の区別は未確定 |
| `requested_roles` | `array<object>` | 必須 | 今回、どの担当のモデルを選ぶか |

`task.requested_roles`の各要素に入れる項目:

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `role` | `string`（空文字列不可） | 必須 | 選定する担当。例は`implementer`、`reviewer` |
| `instruction` | `string` | 必須 | その担当へ依頼する仕事 |
| `required_capabilities` | `array<string>`（各要素は空文字列不可） | 必須 | その担当に必要な能力の識別名。例は`workspace_write`、`code_review` |

#### Issue情報

`task.issue`が値を持つ場合の項目である。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `repository` | `string` | 必須 | Issueがあるリポジトリ |
| `number` | `integer`（0以上） | 必須 | Issue番号 |
| `url` | `string` | 必須 | IssueのURL |
| `title` | `string` | 必須 | 選定時点のIssueタイトル |
| `body` | `string` | 必須 | 選定時点のIssue本文 |
| `labels` | `array<string>` | 必須 | ラベルの一覧 |
| `comments` | `array<object>` | 必須 | 下記のコメント一覧。順序を保つ |

#### Issueのコメント

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `url` | `string` | 必須 | コメントのURL |
| `author` | `string` | 必須 | コメントの投稿者 |
| `created_at_ms` | `integer` | 必須 | 投稿時刻。Unix時刻のミリ秒 |
| `body` | `string` | 必須 | コメントの本文 |

### 実行先とモデル候補

`providers`の各要素に入れる項目:

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `provider` | `string` | 必須 | 実行先の識別名 |
| `availability` | `object` | 必須 | この実行先を使えるか。構造は「共通のデータ形式」の「利用可否と値の取得根拠」 |
| `limits` | `array<object>` | 必須 | この実行先全体に適用される制限 |
| `api_usage` | `object` | 未確定 | API使用量が対象外の場合だけ値なしを許す。例では`null`だが、省略も許すかは未確定 |
| `performance` | `array<object>` | 必須 | この実行先全体の過去実績 |
| `models` | `array<object>` | 必須 | この実行先で選べる候補 |

各`providers[].models[]`に入れる項目:

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `model` | `object` | 必須 | 下記の2形式のどちらか |
| `availability` | `object` | 必須 | このモデルを使えるか |
| `limits` | `array<object>` | 必須 | このモデルだけに適用される制限 |
| `api_usage` | `object` | 未確定 | 実行先と同じく、対象外の場合だけ値なしを許す。JSONでの省略可否は未確定 |
| `capabilities` | `array<string>`（各要素は空文字列不可） | 必須 | この候補が対応する能力の識別名 |
| `performance` | `array<object>` | 必須 | このモデルの過去実績 |
| `estimated_execution` | `array<object>` | 必須 | 次の一回の実行についての推定値 |

モデル指定には、[共通のモデル指定](#モデルの指定)を使う。候補に`unknown`は使わず、過去の実行履歴で実際に使ったモデルが不明な場合とは区別する。

### 現在の実行履歴

`current_attempts`の各要素に入れる項目:

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `attempt_id` | `string` | 必須 | 過去のモデル実行のID |
| `sequence` | `integer`（0以上） | 必須 | Task内での実行順。履歴番号の条件は履歴設計に従う |
| `role` | `string`（空文字列不可） | 条件付き | 担当を復元できない旧履歴では省略する |
| `relation` | `object` | 未確定 | 関係の種類と、必要な参照先。値なしの場合のJSON表現は未確定 |
| `target` | `object` | 必須 | `provider`は文字列、`model`はモデル指定と取得根拠を含むオブジェクト。実際に使ったモデルが不明なら`status: "unknown"`にする |
| `state` | 未確定。状態を表す値 | 必須 | Attemptの状態。新旧の状態の意味とJSON表記の対応は未確定 |
| `failure` | `object` | 未確定 | `category`（分類の文字列）、`retryable`（真偽値）、`summary`（短い説明の文字列）。失敗情報がない場合の省略・`null`の区別は未確定 |
| `validation` | `array<object>` | 必須 | 各要素は`name`（文字列）、`passed`（真偽値）、`summary`（文字列）。結果不明をこの真偽値で表す方法は未確定 |
| `review` | `object` | 条件付き | `outcome`（レビューの結論）と`summary`（文字列）を持つ。旧履歴で復元できない場合は省略する |
| `usage` | `object` | 必須 | 分かる場合の`value`は、`name`と`value`を持つ計測項目の配列 |
| `started_at_ms` | `integer` | 未確定 | 開始のUnix時刻をミリ秒で表す。時刻がない場合の省略・`null`の区別は未確定 |
| `finished_at_ms` | `integer` | 未確定 | 終了のUnix時刻をミリ秒で表す。時刻がない場合の省略・`null`の区別は未確定 |

#### 過去に使った実行先とモデル

履歴の`target`に入れる項目である。選定候補と異なり、モデル情報そのものに取得根拠を添える。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `provider` | `string` | 必須 | 実行先の識別名 |
| `model` | `object` | 必須 | モデル指定と取得根拠。分かる場合のvalueは共通のモデル指定、不明ならstatusをunknownにする |

#### 失敗情報

履歴の`failure`が値を持つ場合の項目である。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `category` | `string` | 必須 | 失敗の分類 |
| `retryable` | `boolean` | 必須 | 再試行できるかを示す値 |
| `summary` | `string` | 必須 | 失敗内容の短い説明 |

#### 検証結果の要約

履歴の`validation`の各要素である。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `name` | `string` | 必須 | 検証の名前 |
| `passed` | `boolean` | 必須 | 検証が成功したか。結果不明を表す方法は未確定 |
| `summary` | `string` | 必須 | 検証結果の短い説明 |

#### レビュー結果の要約

履歴の`review`が値を持つ場合の項目である。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `outcome` | 未確定 | 必須 | 承認・修正要求・判断不能の区別。Rust型案のReviewOutcomeとJSON文字列の対応は未確定 |
| `summary` | `string` | 必須 | レビュー結果の短い説明 |

#### 履歴上の関係

履歴の`relation`が値を持つ場合の項目である。分類と参照条件は[履歴設計](implementation-review-model.md#守る規則)に従い、選定入力を作る際に再分類しない。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `kind` | `enum(initial, retry_of, escalation_of, review_of, rework_from, specified_input, legacy_unspecified)` | 必須 | 保存済みの関係の種類 |
| `attempt_id` | `string`または`null` | 条件付き | retry_of・escalation_of・review_ofでは参照Attempt ID。specified_inputでは作成元Attempt ID、参照先がなければnull |
| `review_attempt_id` | `string` | 条件付き | rework_fromの場合の参照レビューAttempt ID |

`initial`と`legacy_unspecified`は`kind`だけを持つ。`specified_input`の参照先を推測して補わない。

## 出力の定義

### 最上位の3項目

出力は監督CodexからRustへ返す一つのJSONオブジェクトである。入力と同様に、最上位の項目から定義する。外側に`output`という項目は付けない。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `schema_version` | `const 1` | 必須 | 選定用の入出力形式のバージョン |
| `request_id` | `string` | 必須 | 対応する入力と同じ選定要求ID |
| `assignments` | `array<object>` | 必須 | 入力で要求された担当ごとの選定結果 |

### 担当ごとの選定結果

`assignments`の各要素には次の項目を含める。要求された担当を一回ずつ含め、未知・重複・欠落を認めない。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `role` | `string`（空文字列不可） | 必須 | この選定結果が担当する役割 |
| `target` | `object` | 必須 | 下記の実行先とモデルの指定 |
| `reason` | `string`（空白だけは不可） | 必須 | この候補を選んだ理由 |

`target`は次の項目を持つ。

| 項目 | JSON型 | 省略できるか | 意味 |
| --- | --- | --- | --- |
| `provider` | `string` | 必須 | 入力候補から選んだ実行先の識別名 |
| `model` | `object` | 必須 | `named`または`provider_default`のモデル指定。入力候補にある組合せに限る |

モデル指定の形式は[共通のモデル指定](#モデルの指定)に従い、選定結果には`unknown`を使わない。実際に使われたモデルが不明という過去の記録とは区別する。

入力・出力を対応させた[JSON例](#入力と出力の詳細なjson例)で、両方向の実際のオブジェクトを確認できる。

## 各項目に適用する規則

### 作業の要求と必要な担当

`task`には、目的、利用者の制約、現在の状態、今回選ぶ担当を含めます。GitHub Issueを起点にする場合は、選定時点のタイトル・本文・ラベル・コメントを順序を保って含める。コメントに要求変更や設計判断が含まれる場合があるためです。

Issueを使わず依頼内容を直接登録した作業でも、目的と制約を渡します。MCPの作業登録では、この出所を`source: "manual"`と呼びます。人間が実装するという意味ではありません。[作業登録時の出所と保存内容](mcp-operation-contract-reference.md#taskrequest--tasksnapshot)を参照してください。

Rust側は、入力を固定する前に秘密情報と認証情報を取り除き、伏せ字化していない実行先のログを除外します。

`requested_roles`は「今回どの担当を選ぶか」を示します。出力では、各担当に一つずつ割り当てます。実装担当とレビュー担当を同時に要求できるが、必ず異なるモデルを選ぶという規則ではない。必要な能力と実行許可の規則をRust側が提示し、監督Codexがその範囲で判断する。

### 実行先とモデルの候補

`ProviderSnapshot`には、認証、利用契約、APIアカウントなど、実行先全体に関わる事実を記録します。`ModelSnapshot`には、その実行先の個別モデル、または候補として指定された既定モデルを記録します。実行対象は、実行先とモデルの組で特定します。

候補として扱うのは、実行先とモデルの両方が`available`で、担当に必要な能力をすべて満たす組だけです。どちらかが`unavailable`（利用不可）や`unknown`（確認不能）なら実行しない。監督Codexが理由を付けても、この拒否を解除できない。

モデル一覧を取得できない実行先は、空の`models`を渡して「候補なし」と見せかけない。既定モデルの実行を許す場合は、`ModelChoice::ProviderDefault`に対応する候補を一件渡す。

### 利用制限と使用量

`limits`は、要求回数・トークン数・クレジット・同時実行数などの制限を表す。名前、適用範囲、期間、上限、使用量、残量、リセット時刻を別々に保持する。実行先全体の制限は`ProviderSnapshot.limits`、モデル固有の制限は`ModelSnapshot.limits`に置く。同じ事実を両方へ複製しない。

制約の適用方法（`ConstraintEnforcement`）は、Rust側の実行設定（`ExecutionPolicy`）で決めます。`hard`は実行を許可する条件、`advisory`は比較の参考情報である。監督Codexが変更するものではない。

予算制限を設定していない計測項目は、`configured_budget`や`remaining_budget`に項目自体を作らない。「予算不明」という架空の項目も作らない。同じ適用範囲・名前の設定予算と残予算では、制約の適用方法を一致させる。不一致の入力はRust側が拒否する。

| 情報 | 保存先 | 値の取得方法 |
| --- | --- | --- |
| APIの実使用量 | `actual_usage` | `measured`（実測） |
| 設定された予算 | `configured_budget` | `configured`（設定値） |
| 実使用量と設定予算から算出した残予算 | `remaining_budget` | `computed`（計算値）。`source.reference`で計算元を追えるようにする |
| 次の一回の実行のトークン数・費用の見込み | `estimated_execution` | `estimated`（推定値） |
| 取得不能な利用枠・残量・価格 | 対応する項目 | `Evidence::Unknown`。他社の価格や過去平均を実測値の代わりにしない |

`ApiUsageSnapshot.scope`は、アカウント・組織・プロジェクト・リポジトリ予算など、その値が適用される範囲である。`api_usage: None`はAPI利用状況が対象外のときだけに使う。対象だが取得できない場合は、各項目を`unknown`とする。

単位（`MetricValue.unit`）には`input_token`、`output_token`、`request`、ISO 4217の通貨コードなどを使う。異なる単位はそのまま渡し、Rust側で暗黙に換算・合算しない。

### 過去実績と現在の実行履歴

`performance`は、記録用DB（Ledger）から集計した期間付きの実績です。実行先・モデルと、必要なら担当ごとに集計する。成功率だけで母数を隠さず、モデル実行・終了状態・機械検証・レビュー・再試行の件数を渡す。

集計値は`computed`、取得元は`execution_ledger`とする。レビュー結果がまだ保存されていない場合は、その集計値を`unknown`とする。保存済みの`ReviewOutcome`は、`Approved`を`review_approved`、`ChangesRequested`を`review_changes_requested`、`Inconclusive`を`review_inconclusive`へそれぞれ一件加算する。一件を複数分類へ加算せず、どの結果も集計から除外しない。

`current_attempts`は同じTaskに属するすべてのAttemptを、履歴番号（`sequence`）順に渡す。直前の失敗だけでなく、モデルの切替・検証・レビュー・累積の再試行を次の判断に使えるようにする。

伏せ字化していない標準出力・標準エラー出力や機密情報は渡さない。選定に必要な分類と短い要約だけを渡す。過去の実行記録を新しい選定結果で上書きしない。

## 入力と出力の詳細なJSON例

以下は、Rustから監督Codexへ渡す入力と、監督CodexからRustへ返す出力を、実際に受け渡すJSONオブジェクトごとに分けた例です。外側に`input`や`output`という項目を付けて一つのJSONにまとめる形式ではありません。

例には、同じ実行先でモデル名を指定する候補と既定モデルを使う候補、実測値・推定値・不明な値、実装担当とレビュー担当の割当を含めます。実行先・モデル名・利用量・金額はすべて架空です。実在するモデルの利用可否や価格は示しません。

### Rustから監督Codexへ渡すJSON

Rustは、作業の目的と制約、今回選ぶ担当、候補ごとの利用可否・使用量・制約・実績、現在の実行履歴を一つの入力にまとめます。監督Codexは、その情報から担当ごとの実行先とモデルを選びます。

この例では、実装に`workspace_write`、レビューに`code_review`という能力を求めています。名前を指定する候補は両方に対応し、既定モデルの候補はレビューに対応しています。残予算の値は不明ですが、この例の予算は比較材料として扱う`advisory`です。実行を必ず制限する`hard`の残量が不明な場合は、[上限の確認規則](#強制する上限の残量が不明な場合)に従います。

```json
{
  "schema_version": 1,
  "request_id": "selection-01J...",
  "captured_at_ms": 1790000000000,
  "task": {
    "task_id": "task-57",
    "objective": "Issue #57 の入力・出力仕様を定義する",
    "constraints": ["既存の状態遷移を迂回しない"],
    "state": "active",
    "issue": {
      "repository": "owner/repository",
      "number": 57,
      "url": "https://github.com/owner/repository/issues/57",
      "title": "モデル選定の入力・出力仕様を定義する",
      "body": "...",
      "labels": ["design"],
      "comments": []
    },
    "requested_roles": [
      {
        "role": "implementer",
        "instruction": "変更を実装して検証する",
        "required_capabilities": ["workspace_write"]
      },
      {
        "role": "reviewer",
        "instruction": "完了条件と差分をレビューする",
        "required_capabilities": ["code_review"]
      }
    ]
  },
  "providers": [
    {
      "provider": "example_api",
      "availability": {
        "status": "available",
        "observed_at_ms": 1790000000000,
        "source": {"kind": "provider_api", "reference": "models.list"}
      },
      "limits": [],
      "api_usage": {
        "scope": "repository_budget",
        "window": {"starts_at_ms": 1789990000000, "ends_at_ms": 1790076400000},
        "actual_usage": [
          {
            "name": "cost",
            "value": {
              "status": "known",
              "value": {"amount": "1.42", "unit": "USD"},
              "basis": "measured",
              "assessed_at_ms": 1790000000000,
              "source": {"kind": "provider_api", "reference": "usage"}
            }
          }
        ],
        "configured_budget": [
          {
            "name": "cost",
            "enforcement": "advisory",
            "value": {
              "status": "known",
              "value": {"amount": "10.00", "unit": "USD"},
              "basis": "configured",
              "assessed_at_ms": 1790000000000,
              "source": {"kind": "repository_config", "reference": "daily_budget"}
            }
          }
        ],
        "remaining_budget": [
          {
            "name": "cost",
            "enforcement": "advisory",
            "value": {
              "status": "unknown",
              "reason": "provider did not expose a matching billing window",
              "assessed_at_ms": 1790000000000,
              "source": {"kind": "provider_api", "reference": "usage"}
            }
          }
        ]
      },
      "performance": [],
      "models": [
        {
          "model": {"kind": "named", "model": "sample-model-a"},
          "availability": {
            "status": "available",
            "observed_at_ms": 1790000000000,
            "source": {"kind": "provider_api", "reference": "models.list"}
          },
          "limits": [],
          "api_usage": null,
          "capabilities": ["workspace_write", "code_review"],
          "performance": [],
          "estimated_execution": [
            {
              "name": "cost",
              "value": {
                "status": "known",
                "value": {"amount": "0.08", "unit": "USD"},
                "basis": "estimated",
                "assessed_at_ms": 1790000000000,
                "source": {"kind": "execution_ledger", "reference": "recent_attempt_average"}
              }
            }
          ]
        },
        {
          "model": {"kind": "provider_default"},
          "availability": {
            "status": "available",
            "observed_at_ms": 1790000000000,
            "source": {"kind": "provider_cli", "reference": "capability_probe"}
          },
          "limits": [],
          "api_usage": null,
          "capabilities": ["code_review"],
          "performance": [],
          "estimated_execution": []
        }
      ]
    }
  ],
  "current_attempts": []
}
```

### 監督CodexからRustへ返すJSON

監督Codexは、同じ`request_id`に対して、要求された担当ごとの実行先・モデル・選定理由を返します。上の入力にある利用量や利用可否を、選定結果の中で書き換えて返しません。

この例では、実装担当には名前を指定したモデル、レビュー担当には実行先の既定モデルを選んでいます。Rustはこの結果を入力と現在の利用条件に照合してから、実行を許可するか確認します。

```json
{
  "schema_version": 1,
  "request_id": "selection-01J...",
  "assignments": [
    {
      "role": "implementer",
      "target": {
        "provider": "example_api",
        "model": {"kind": "named", "model": "sample-model-a"}
      },
      "reason": "利用可能で必要 capability を満たす候補だから"
    },
    {
      "role": "reviewer",
      "target": {
        "provider": "example_api",
        "model": {"kind": "provider_default"}
      },
      "reason": "レビュー capability を持つ利用可能な候補だから"
    }
  ]
}
```

選定結果にレビュー担当（`reviewer`）が含まれても、レビューが必須になるわけではありません。監督Codexが必要な担当だけを要求します。履歴の関係はServiceが入力と確認済みの記録から保存します。選定理由（`reason`）はモデルを選んだ理由であり、再試行やレビュー指摘への修正を行った理由として使いません。

## Rustが行う検証

選定結果を実行へ渡す前に、Rust側は次の項目を確認します。

1. `schema_version`を解釈でき、`request_id`が入力と一致する。
2. 要求した各担当が出力に一回ずつ現れ、未知・重複・欠落の担当がない。
3. 選定理由（`reason`）が空白だけではない。
4. 実行先とモデルの組が、選定に渡した候補に存在する。
5. 実行先とモデルの利用可否が、両方とも`available`である。
6. モデルが担当に必要な能力をすべて持つ。
7. Rust側が設定した予算・利用枠・再試行上限・安全条件に反しない。
8. 実行直前に利用可否と強制する制約を再取得し、選定後に変化した事実にも反しない。

1〜6では返された選定結果を元の入力と照合し、7〜8では現在の事実に基づいて実行を許可できるか確認します。両方を通過した値（`ValidatedPlannerDecision`相当）だけを実行へ渡す。

失敗した場合は、未知の実行対象・利用不能・古い選定結果・許可条件違反など、種類を識別できるエラーを返す。監督Codexの選定理由で検査を迂回しない。

選定前にも、Rust側は入力の識別子・担当・実行先とモデルの組が一意で空文字列がないこと、`unavailable`や`unknown`に理由があること、各値の取得方法が本書の規則に合うことを確認する。ある担当に実行可能な候補が一件もなければ、モデルに架空の候補を作らせず、候補なしを示す`no eligible target`エラーを返す。

### 強制する上限の残量が不明な場合

`hard`の`ResourceLimitSnapshot.remaining`や`remaining_budget.value`が`unknown`なら、Rust側が実行先の接続処理や使用量の収集処理から値を再取得するまでは実行可能とみなさない。

- 再取得後も不明なら、`SelectionValidationError::ConstraintIndeterminate { scope, name }`で拒否する。
- 残量が不足していると分かった場合は、`SelectionValidationError::ConstraintExceeded { scope, name }`で拒否する。
- `advisory`の値は不明でも実行を妨げない。不明であることと理由を監督Codexへ渡す。

実行直前の再確認にも同じ規則を適用する。監督Codexの割当や理由で制約の適用方法を変えない。

## 未確定の送信形式

| 不足している規則 | 現在の記述だけでは決められないこと |
| --- | --- |
| 値なし項目の表現 | `task.issue`、各`api_usage`、制限の`window`、実績の`role`、履歴の`relation`・`failure`・開始終了時刻などを、省略するか`null`にするか。`role`・`review`が復元不能な旧履歴で省略する規則と、`specified_input.attempt_id: null`は既に記載済み |
| 状態とレビュー結論の文字列表現 | Task・Attemptの全状態とReviewOutcomeをJSONでどう表し、新旧履歴の意味の違いをどう伝えるか |
| 結果不明の検証要約 | `ValidationSummary.passed`は真偽値しか持たないが、結果不明を失敗へ変換してはならない。新しいValidation結果との対応 |

これらは例の見やすさとは別の、送信形式の仕様不足である。Rust型案や例から暗黙に補って実装せず、仕様を確定してから統合実装へ進む。

## 実装担当者向けのRust型案

ここからは、上で説明した情報とRust型・JSON項目の対応を示します。設計の目的や分担を確認する場合は、上の説明を先に読む。型名・値の名前は実装上の名前として保持する。

<details>
<summary>Rust型定義の詳細を表示する</summary>

以下は、v1の必須項目と省略可能な項目を示す型案です。実装では`serde`を使い、分類を識別する値付きの列挙型（tagged enum）にする。JSONの項目名は`snake_case`とする。時刻は既存DBと同じUnix時刻のミリ秒、量は丸めや浮動小数点誤差を避けるため十進数の文字列で表す。

JSONでは、`ModelChoice`を`kind`、`Evidence`を`status`で識別するオブジェクトとして表す。
`Evidence::Known` は `status: "known"` と `basis`、`Evidence::Unknown` は
`status: "unknown"` と `reason` を持つ。`AvailabilityStatus` だけは
`AvailabilityObservation`の直下へ配置し、`status: "available"`、または
`status: "unavailable" | "unknown"` と 空文字列ではない`reason`を同じオブジェクトに置く。
`status`オブジェクトを入れ子にする通信形式は許可しない。

```rust
struct ModelSelectionInput {
    schema_version: u32,
    request_id: SelectionRequestId,
    captured_at_ms: i64,
    task: SelectionTask,
    providers: Vec<ProviderSnapshot>,
    current_attempts: Vec<AttemptSummary>,
}

struct SelectionRequestId(String);

struct SelectionTask {
    task_id: TaskId,
    objective: String,
    constraints: Vec<String>,
    state: TaskState,
    issue: Option<IssueContext>,
    requested_roles: Vec<RoleRequirement>,
}

struct IssueContext {
    repository: String,
    number: u64,
    url: String,
    title: String,
    body: String,
    labels: Vec<String>,
    comments: Vec<IssueCommentContext>,
}

struct IssueCommentContext {
    url: String,
    author: String,
    created_at_ms: i64,
    body: String,
}

struct RoleRequirement {
    role: SelectionRole,
    instruction: String,
    required_capabilities: Vec<CapabilityRef>,
}

struct SelectionRole(String);
struct CapabilityRef(String);
struct ModelRef(String);

enum ModelChoice {
    Named { model: ModelRef },
    ProviderDefault,
}

struct ExecutionTarget {
    provider: ProviderRef,
    model: ModelChoice,
}

struct ProviderSnapshot {
    provider: ProviderRef,
    availability: AvailabilityObservation,
    limits: Vec<ResourceLimitSnapshot>,
    api_usage: Option<ApiUsageSnapshot>,
    performance: Vec<PerformanceSnapshot>,
    models: Vec<ModelSnapshot>,
}

struct ModelSnapshot {
    model: ModelChoice,
    availability: AvailabilityObservation,
    limits: Vec<ResourceLimitSnapshot>,
    api_usage: Option<ApiUsageSnapshot>,
    capabilities: Vec<CapabilityRef>,
    performance: Vec<PerformanceSnapshot>,
    estimated_execution: Vec<NamedMetric>,
}

struct AvailabilityObservation {
    #[serde(flatten)]
    status: AvailabilityStatus,
    observed_at_ms: i64,
    source: EvidenceSource,
}

#[serde(tag = "status", rename_all = "snake_case")]
enum AvailabilityStatus {
    Available,
    Unavailable { reason: String },
    Unknown { reason: String },
}

struct ResourceLimitSnapshot {
    name: String,
    scope: String,
    enforcement: ConstraintEnforcement,
    window: Option<TimeWindow>,
    limit: Evidence<MetricValue>,
    used: Evidence<MetricValue>,
    remaining: Evidence<MetricValue>,
    resets_at_ms: Evidence<i64>,
}

struct ApiUsageSnapshot {
    scope: String,
    window: TimeWindow,
    actual_usage: Vec<NamedMetric>,
    configured_budget: Vec<PolicyMetric>,
    remaining_budget: Vec<PolicyMetric>,
}

struct NamedMetric {
    name: String,
    value: Evidence<MetricValue>,
}

struct PolicyMetric {
    name: String,
    enforcement: ConstraintEnforcement,
    value: Evidence<MetricValue>,
}

enum ConstraintEnforcement {
    Hard,
    Advisory,
}

struct MetricValue {
    amount: String,
    unit: String,
}

enum Evidence<T> {
    Known {
        value: T,
        basis: EvidenceBasis,
        assessed_at_ms: i64,
        source: EvidenceSource,
    },
    Unknown {
        reason: String,
        assessed_at_ms: i64,
        source: EvidenceSource,
    },
}

enum EvidenceBasis {
    Measured,
    Configured,
    Computed,
    Estimated,
}

struct EvidenceSource {
    kind: EvidenceSourceKind,
    reference: String,
}

enum EvidenceSourceKind {
    ProviderApi,
    ProviderCli,
    ExecutionLedger,
    RepositoryConfig,
}

struct TimeWindow {
    starts_at_ms: i64,
    ends_at_ms: i64,
}

struct PerformanceSnapshot {
    role: Option<SelectionRole>,
    window: TimeWindow,
    attempts: Evidence<u64>,
    succeeded: Evidence<u64>,
    failed: Evidence<u64>,
    cancelled: Evidence<u64>,
    validation_passed: Evidence<u64>,
    validation_failed: Evidence<u64>,
    review_approved: Evidence<u64>,
    review_changes_requested: Evidence<u64>,
    review_inconclusive: Evidence<u64>,
    retries: Evidence<u64>,
}

struct AttemptSummary {
    attempt_id: AttemptId,
    sequence: u32,
    role: Option<SelectionRole>,
    relation: Option<AttemptRelationSummary>,
    target: AttemptTarget,
    state: AttemptState,
    failure: Option<AttemptFailureSummary>,
    validation: Vec<ValidationSummary>,
    review: Option<ReviewSummary>,
    usage: Evidence<Vec<NamedMetric>>,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
}

#[serde(tag = "kind", rename_all = "snake_case")]
enum AttemptRelationSummary {
    Initial,
    RetryOf { attempt_id: AttemptId },
    EscalationOf { attempt_id: AttemptId },
    ReviewOf { attempt_id: AttemptId },
    ReworkFrom { review_attempt_id: AttemptId },
    SpecifiedInput { attempt_id: Option<AttemptId> },
    LegacyUnspecified,
}

struct AttemptTarget {
    provider: ProviderRef,
    model: Evidence<ModelChoice>,
}

struct AttemptFailureSummary {
    category: String,
    retryable: bool,
    summary: String,
}

struct ValidationSummary {
    name: String,
    passed: bool,
    summary: String,
}

struct ReviewSummary {
    outcome: ReviewOutcome,
    summary: String,
}

enum ReviewOutcome {
    Approved,
    ChangesRequested,
    Inconclusive,
}

struct ModelSelectionDecision {
    schema_version: u32,
    request_id: SelectionRequestId,
    assignments: Vec<RoleAssignment>,
}

struct RoleAssignment {
    role: SelectionRole,
    target: ExecutionTarget,
    reason: String,
}
```

`SelectionRole`と`CapabilityRef`は、v1では空文字列を許さない、拡張可能な文字列型とする。

担当とレビュー結果はAttemptに保存する。レビューは任意であり、`ReviewSummary`はレビュー担当の実行結果に使う。履歴の関係と移行条件の正本は[履歴設計の規則](implementation-review-model.md#守る規則)と[保存する分類](implementation-review-model.md#実装で使う名前)である。

`AttemptRelationSummary`は、その保存済みの関係を選定入力へ要約する。

- 初回実装・再試行・切替・レビュー・レビュー指摘への修正を、既存の分類で区別する。
- 一般の指定入力から実行した記録は`SpecifiedInput`とする。ArtifactInputに同じTask内の作成元Attemptが記録されていれば、`attempt_id`へその参照を残す。分類理由が不明というだけで参照を消さない。BaseInputと作成元不明の場合は`null`にする。
- 入力自体はAttemptの入力欄に一度だけ保存し、この要約へ複製しない。
- 一般の実行要求から再試行やレビュー指摘への修正の理由を推定しない。明示的な理由を受ける既存APIは、その理由と参照先の検証を維持する。
- 移行時は保存済みの関係と参照先を保持し、新しい分類規則で再分類しない。復元できない旧関係だけを`LegacyUnspecified`とする。
- 旧DBから確定できない担当（`role`）やレビュー結果（`review`）は省略し、推測で補わない。関係を復元できない移行済み履歴は`relation: { "kind": "legacy_unspecified" }`として渡す。

`provider_default` は「Model を指定しない」の暗黙表現ではない。Rust が候補として明示した場合だけ
選べる判別値である。Provider が実際に解決した Model の記録方法は Issue #59 で定義する。
既存 Ledger のように過去のモデル実行に使用モデルを記録していない場合は、`AttemptTarget.model` を
`unknown` とする。選定候補と選定結果の `ExecutionTarget.model` に `unknown` は許可しない。

</details>
