# Proposal

## Why

利用者がIssueの修正を依頼し、監督Codexが担当モデルへ作業を伝えます。Rustの共通処理は変更と検証結果を記録し、監督Codexは結果を見て修正またはDraft PR公開を判断します。その後のCIと作業完了までを一つの処理と記録DBにつなぐ設計がPR #112の対象です。

現在は仕様書ごとに責務や設定の説明が分かれており、誰が操作を決め、誰が実行設定を用意し、失敗時に何を記録するのかを一続きでレビューしにくくなっています。

## What Changes

- 13操作を共通ServiceとSQLite記録へつなぐ設計、監督CodexとRustサーバー起動側の責務、起動・終了・取消・結果不明時の扱いを、既存仕様を参照しながら一つの統合設計にまとめます。
- モデル選択やValidator許可設定、秘密情報Scanner、GitHub/CI接続を誰が用意するかと、不足時の代表的な拒否動作を説明します。本番のモデル情報取得や秘密情報Scanner接続は未実装であることを区別します。
- たとえば、テストに失敗した変更をレビューなしで次のモデルへ渡して修正する場合に、既存の履歴理由を推測せず、明示した入力として記録する要件を定めます。履歴分類の詳細は後段の限定された追加です。
- 関連文書を同じ設計に合わせて改訂します。このPRは文書と仕様の変更に限り、製品コード、DB migration、実行時の動作は変えません。製品実装は後続作業です。

## Capabilities

### New Capabilities

- `attempt-input-history`: 明示されたAttempt入力を既存分類と区別して記録し、既知の生成元を同じTaskに限って参照する履歴要件

### Modified Capabilities

- なし。OpenSpecに移行済みの既存capabilityがないため、新規capabilityとして記述します。

## Impact

変更対象は、このOpenSpec changeのproposal/spec/design/tasks文書とPR #112で改訂する関連仕様書です。後続実装ではAttempt履歴のDomain/Ledger/Serviceと旧データ移行に影響しますが、この提案自体はRustコード、公開API、DB schema、runtime動作を変更しません。既存仕様の詳細は[履歴設計](../../../docs/implementation-review-model.md)、[ドメインモデル](../../../docs/domain-model.md)、[統合設計](../../../docs/operation-service-integration-design.md)を参照します。
