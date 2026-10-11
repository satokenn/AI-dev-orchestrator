# Spec Delta

## Purpose

作業依頼（`Task`）の中でモデルを一回呼び出した記録（`Attempt`）、保存済みの変更（`Artifact`）、作業開始時点のcommit（`BaseInput`）を扱います。この仕様は、明示された入力と、その入力が履歴上どこから来たかを記録します。既存分類と旧履歴の意味を保ち、調査やレビューなしの修正を誤った理由へ結び付けません。

履歴分類、既存relationの優先、旧履歴移行の詳細な正本は[実装・レビュー・修正を記録する設計](../../../../../docs/implementation-review-model.md#守る規則)と[ドメインモデル](../../../../../docs/domain-model.md#実装検証任意レビューの例)です。この差分は、そこに定めた`SpecifiedInput`の条件を独立して検証可能にします。

## ADDED Requirements

### Requirement: 明示入力の履歴分類

システムは、既存の理由分類に根拠をもって当てはまらない新規Attemptを`SpecifiedInput`として記録しなければならない（SHALL）。一般の`attempt.run`入力から再試行やレビュー指摘への修正を推定してはならない。

#### Scenario: Validation失敗後にレビューなしで成果物を修正する

- **WHEN** 成功した実装Attemptが作成したArtifactを明示入力とし、検証失敗後にレビューを経ない実装Attemptを依頼する
- **THEN** 新しいAttemptを`SpecifiedInput`として記録し、既存の根拠のない`RetryOf`や`ReworkFrom`にはしない

#### Scenario: 現在Artifactの作成元が分かるレビュー

- **WHEN** 成功した実装Attemptが作成した同じTaskの現在Artifactをreviewerが対象にする
- **THEN** 既存の`ReviewOf`分類を維持する

#### Scenario: Artifactの作成元が分からないレビュー

- **WHEN** reviewerが現在Artifactを対象にするが、その作成元Attemptを確認できない
- **THEN** 新しいAttemptを`SpecifiedInput`として記録し、作成元を推定しない

### Requirement: 指定入力の生成元参照

システムは、`SpecifiedInput`の元となるArtifactの作成Attemptが同じTask内で記録されている場合、そのAttemptを`related_attempt_id`として保存しなければならない（SHALL）。入力値は既存Attempt入力に一度だけ保存し、理由分類の追加情報として複製してはならない。

#### Scenario: ArtifactInputに同じTaskの作成元Attemptがある

- **WHEN** `SpecifiedInput`のArtifactInputが、同じTask内の記録済み作成Attemptを持つ
- **THEN** そのAttempt IDを`related_attempt_id`に保存する

#### Scenario: BaseInputまたは生成元が不明なArtifactInput

- **WHEN** 入力が`BaseInput`である、またはArtifactの作成Attemptを確認できない
- **THEN** `related_attempt_id`を`null`にし、commitの一致や別Taskの記録から参照を推定しない

#### Scenario: reviewerがBaseInputから作業する

- **WHEN** reviewerの入力が`BaseInput`である
- **THEN** Attemptを`SpecifiedInput`として記録し、`related_attempt_id`とArtifact対象のReviewVerdictを作らない

### Requirement: Initial分類の適用条件

システムは、同じTask内で`sequence=1`の`implementer`が`BaseInput`から開始する場合に限り、新規Attemptを`Initial`として記録しなければならない（SHALL）。この条件に当てはまらない新規Attemptを、初回実装として分類してはならない。

#### Scenario: Taskの最初の実装担当がBaseInputを使う

- **WHEN** `sequence=1`のAttemptが`implementer`であり、入力が`BaseInput`である
- **THEN** そのAttemptを`Initial`として記録する

#### Scenario: 最初のAttemptが調査担当である

- **WHEN** `sequence=1`が`explorer`で、その後に`implementer`が明示入力から実行する
- **THEN** 後続の`implementer`を`Initial`へ変更せず、既存理由分類に当てはまらなければ`SpecifiedInput`とする

### Requirement: 旧履歴の関係保持と移行

システムは、移行時に既存履歴へ保存済みのrelationと参照先をそのまま保持し、新規Attempt向けの分類規則で再分類してはならない（SHALL）。関係がない旧行は、既存データで確認できる条件だけから分類し、復元できない場合に限り`LegacyUnspecified`としなければならない。

#### Scenario: 旧履歴にrelationと参照先が保存されている

- **WHEN** 移行対象の履歴行にrelationと参照先が存在する
- **THEN** 移行後もその値を保持し、`SpecifiedInput`などへ分類し直さない

#### Scenario: relationのない旧行をInitialと確認できる

- **WHEN** 既存データから`sequence=1`、`role=implementer`、`BaseInput`の全てを確認できる
- **THEN** その行を`Initial`として移行できる

#### Scenario: relationのない旧行の関係を復元できない

- **WHEN** 既存データから関係を復元できない
- **THEN** `LegacyUnspecified`として移行し、`related_attempt_id`を設定しない

#### Scenario: 新しいAttemptを旧履歴向け分類で保存しない

- **WHEN** 新しいAttemptが既存理由分類に当てはまらない
- **THEN** `LegacyUnspecified`ではなく`SpecifiedInput`を使う
