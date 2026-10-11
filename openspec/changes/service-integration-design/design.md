# Design

## Context

この設計の目的は、13操作の受付、状態、結果記録を一つの共通処理とDBへつなぐことです。作業依頼（`Task`）の中でモデルを一回呼び出した記録（`Attempt`）、保存済みの変更（`Artifact`）、作業開始時点のcommit（`BaseInput`）を扱います。監督Codexは次に何をするか判断し、Rustサーバーの共通処理は入力を検査して実行と記録を担います。

ここでいう「機能単位」（capability）は、OpenSpecで検証可能な要件をまとめる単位です。ドメイン層（Domain）は作業や結果を表す型を、台帳層（Ledger）はSQLiteへの保存を、共通サービス（Service）は受付と実行の調整を担います。検証器（Validator）は指定されたコマンドを実行し、秘密情報Scannerは入力内の既知の秘密情報を検査・伏せ字化します。モデル情報一覧（ModelCatalog）は指定モデルを確認するための情報源です。

### 一連の流れ

```text
利用者のIssue
    → 監督Codexが実装担当へ依頼
    → Rustが変更を検査して保存
    → テスト結果を確認
    → 監督Codexが修正またはDraft PR公開を判断
    → CI結果を確認
    → 作業完了を記録
```

たとえばテストに失敗した変更は、監督Codexがその変更を次のモデルへ明示して修正を依頼できます。システムは過去の失敗だけから「再試行」や「レビュー指摘への修正」と推測せず、指定された入力と既知の生成元を履歴へ記録します。この履歴の追加条件は[attempt-input-history仕様](specs/attempt-input-history/spec.md)に記載します。

このPRは設計文書の改訂です。Rust実装、公開API、DB schema、実行時動作は変わりません。13操作を含む統合設計、責務、起動・終了・取消・失敗時の詳細は[統合設計](../../../docs/operation-service-integration-design.md)を正本とし、本書では全体の役割と判断例を説明します。

### 誰が決め、誰が設定するか

| 担当 | 決めること・渡すもの | 不足時の扱い |
| --- | --- | --- |
| 監督Codex | 実行先、担当、モデル指定、使う入力、指示を選び、MCP操作としてRustへ渡す。次の操作も観測結果を見て判断する。 | 入力の所属や操作可能性はRustが検査し、不適切な要求は受理しない。 |
| Rustサーバーの起動側 | 信頼するProvider、検査を許す設定、秘密情報Scanner、モデル確認処理、GitHub/CI接続を用意してServiceへ渡す。 | 必須の安全確認や外部接続を利用できない場合、該当操作を開始せず固定エラーを返す。 |
| 共通処理 | 操作入力と対象の作業依頼・保存済み変更の関係を検査し、受付、実行、結果を共通の記録台帳へ保存する。 | 保存に失敗した場合に、成功した実行や完了を推測して返さない。 |

MCP Hostは監督Codexを動かすアプリケーションです。信頼設定を提供するのはHostではなく、Rustサーバーの起動側です。操作要求から許可設定を差し替える項目は設けません。

### 設定の判断と代表的な失敗

モデルを名前で指定する`named`では、モデル確認処理が未設定、または指定名を確認できない場合に要求を拒否します。Provider既定モデルを使う`provider_default`は、ModelCatalogが未設定という理由だけでは拒否しません。ただし、他の正式な入力条件は引き続き適用します。モデル情報を本番環境から取得する処理は未実装です。

Validator設定や秘密情報Scannerの詳細は[Validator許可設定](../../../docs/operation-service-integration-design.md#validatorの許可設定)と[入力値の秘密情報検査](../../../docs/operation-service-integration-design.md#validation要求に含まれる値の秘密情報検査)を参照してください。Scannerが未設定または失敗した場合、あるいは伏せ字化を二回行った結果が同じにならない場合は、未加工の要求を保存・実行せず固定の拒否結果を返します。伏せ字化で実行するコマンドなどの値そのものが変わる場合も同様です。

本番でのモデル情報取得やScanner接続処理はこのPRで提供しません。設計で責任の境界を明確にすることと、実行時の設定取得が実装済みであることは別です。

## Goals / Non-Goals

**Goals:**

- 13操作の受付、状態、結果記録を一つの共通処理とDBへつなぐ責務境界を、利用者の依頼から完了までの例で説明する。
- 既存分類を優先し、明示された入力で行う調査・レビューなしの修正を`SpecifiedInput`として記録できる仕様を定める。
- 同じTask内のArtifact生成Attemptが分かる場合は参照を保ち、`BaseInput`や生成元不明の場合には参照を推定しない。
- 初回実装と旧履歴移行の境界を、検証可能な条件として記録する。

**Non-Goals:**

- Rustコード、DB migration、MCPのtool schema、履歴readerをこのPRで実装する。
- 既存の再試行、エスカレーション、レビュー、修正の要件を一括してOpenSpecへ再定義する。
- 自動レビュー、入力からの理由推定、異なるTask間でのArtifact生成元推定を追加する。

## Decisions

### 新しい入力履歴capabilityだけを追加する

OpenSpecの既存spec一覧は空でした。既存capabilityの要件変更とはせず、Attempt入力履歴の動作を持つ`attempt-input-history`を新設します。他の統合機能をこのcapabilityへ含めません。

**代替案:** 既存capabilityを変更する案は、対応するOpenSpec specがなく、既存pathを特定できないため採用しません。統合設計の全条件を複数の新規capabilityとして一度に登録する案も、この変更の範囲を越えるため採用しません。

### 既存分類を優先し、根拠がない理由は推定しない

既存の明示された理由分類が成立する場合はそれを維持します。一般の`attempt.run`にはRetryやRework理由を指定する入力項目がないため、直前のValidation失敗、Provider/Modelの違い、過去レビューだけを使って理由を推定しません。既存分類で表せない新しい明示入力実行には`SpecifiedInput`を使います。`ReviewOf`は同じTaskの現在Artifactを成功済み実装Attemptが作ったと確認できる場合に限ります。

**代替案:** 直前の失敗からRetryまたはReworkを自動推定する案は、明示されていない理由を履歴へ加えるため採用しません。新規実行を移行用の`LegacyUnspecified`へ分類する案も、旧履歴の意味を持つ値を新規入力に流用するため採用しません。

### `SpecifiedInput`は既知の同Task生成元だけを参照する

`ArtifactInput`に同じTask内の作成Attemptが記録されていれば、そのAttemptを`related_attempt_id`に保存します。入力値は既存Attempt入力欄に一度だけ保持します。`BaseInput`にはAttempt参照を付けません。生成元不明のArtifactについても参照を推測しません。commit hashが一致するだけの別Taskや過去Artifactから、関係を作りません。

**代表例:** 成功ArtifactのValidationが失敗し、レビューを経ずにそのArtifactを入力とする実装Attemptを依頼した場合、`SpecifiedInput`と同じTaskの生成Attempt参照を保存します。BaseInputを使う`explorer`、BaseInputを使う`reviewer`、生成元不明のArtifactを対象とする一般レビューは、`SpecifiedInput`で参照先を持ちません。BaseInput reviewerはread-onlyであり、Artifact対象のReviewVerdictを作りません。

**代替案:** commit hashや内容の一致から生成元を推定する案、TaskをまたぐAttemptを参照する案は、記録にない関係を追加するため採用しません。入力値をrelation用に二重保存する案も採用しません。

### `Initial`と旧履歴の扱いを分ける

新規`Initial`は、同じTaskの`sequence=1`で`implementer`が`BaseInput`から開始するときだけ使います。調査担当が先行した後のimplementerや、ArtifactInputから始める実行は、他の根拠ある分類がなければ`SpecifiedInput`です。

移行時は、旧行に保存済みのrelationと参照先をそのまま保持し、新規分類規則を適用して書き換えません。relationがない旧行を`Initial`にできるのは、`sequence=1`、`role=implementer`、`BaseInput`を全て既存データから確認できる場合だけです。それ以外で関係を復元できない行は`LegacyUnspecified`にし、参照先を作りません。

**代替案:** 旧行すべてに新分類を再適用する案は、既に保存された意味と参照先を変えるため採用しません。移行番号やDDLを先に決める案も、製品実装と互換fixtureが本変更の範囲外のため採用しません。

## Risks / Trade-offs

- [既存データから根拠を復元できない] → `LegacyUnspecified`として保持し、根拠のない関係を作りません。曖昧さは残りますが、誤った参照を保存しません。
- [指定入力を`SpecifiedInput`にまとめると理由の違いが型に現れない] → BaseInput/ArtifactInputは既存入力欄に一度だけ保存し、既知のArtifact生成Attempt参照も保持します。理由を捏造するための追加enumは設けません。
- [仕様書が実装済みと誤認される] → Proposalとこの設計で、文書のみの変更、runtime不変、製品実装とDB移行は後続であることを明記します。

## Migration Plan

このPRではOpenSpec文書と関連仕様文書だけを更新し、データ移行は行いません。後続の製品実装では、旧履歴のrelationを保持し、根拠を確認できない旧行だけに移行用分類を使います。互換性検査や失敗時に元DBを保持する条件は[履歴の保存と移行](../../../docs/implementation-review-model.md#保存と移行)を正本とします。DB migration番号やDDLは、実装担当が既存schemaとfixtureを確認してから決めます。

## Open Questions

この仕様変更で定める履歴条件に未決定事項はありません。一方、本PRは製品実装を含まず、本番のモデル情報取得、Scannerの具体的な接続、Rust側の統合動作は提供しません。これらは既存契約に従う後続実装と検証の対象です。詳しい担当境界と未接続項目は[統合設計](../../../docs/operation-service-integration-design.md)を参照してください。
