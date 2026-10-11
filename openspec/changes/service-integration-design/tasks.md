# Tasks

1・2はこのPRで実施した文書作成と仕様の照合、3は後続の製品実装です。チェック済みの文書作業は製品の動作確認を意味しません。4では形式確認・製品検証・PRレビューを分けます。

## 1. 統合設計と仕様文書

- [x] 1.1 利用者の依頼からAttempt、Artifact、検証、Draft PR、CI、作業完了までの責務と記録を既存設計へ反映し、13操作それぞれの既存契約との対応を確認する
- [x] 1.2 Rustサーバー起動側がProvider、Validator許可設定、秘密情報Scanner、モデル情報確認、GitHub/CI接続を渡す責任を文書化し、MCP入力から信頼設定を差し替えられないことと不足時の失敗例を確認する
- [x] 1.3 起動・終了、受付と実行の分離、保存結果の取得、取消、停止未確認、再起動後の復旧を既存仕様へ対応付け、統合設計の節リンクが正しいことを確認する
- [x] 1.4 README、AGENTS.md、アーキテクチャ、ドメイン、履歴、モデル選定、MCP契約文書を見直し、実装済み動作と文書上の設計を混同しない説明になっていることを確認する

## 2. Attempt入力履歴の仕様

- [x] 2.1 `attempt-input-history`仕様を既存の履歴分類と照合し、Initial、SpecifiedInput、ReviewOf、RetryOf、ReworkFromの適用優先を例で確認する
- [x] 2.2 既知の同Task Artifact生成元参照、BaseInputや生成元不明時のnull、reviewerのBaseInput、初回Initial条件をDomain/Ledgerの既存記録形式と照合する
- [x] 2.3 旧履歴に保存済みのrelation保持と、根拠がない場合だけLegacyUnspecifiedへ移す条件を既存移行説明と照合する

## 3. 後続の製品実装（この文書変更には含まない）

- [ ] 3.1 Domain、Ledger、Serviceで明示入力分類と既知生成元参照を実装し、仕様シナリオの新規Attemptと再送fixtureを検証する
- [ ] 3.2 旧schemaのrelationを保持する移行を実装し、既存relation、Initial判定、復元不能な旧行をfixtureで確認する
- [ ] 3.3 共通Service、MCP入口、Provider/Validator/Scanner/ModelCatalog/GitHub・CI依存の実製品接続を既存統合設計に沿って実装し、各操作に定めた拒否条件が適用されることをfixtureで確認する
- [ ] 3.4 取消停止確認、再起動復旧、結果取得と全操作の既存spec対応をend-to-end fixtureで検証し、文書の説明と実際のRust動作を一致させる

## 4. 検証とレビュー

- [x] 4.1 OpenSpec変更を`openspec validate service-integration-design --strict`で検証し、文書リンクと仕様シナリオを確認する
- [ ] 4.2 製品実装後に`cargo fmt --all -- --check`、strict Clippy、workspace all-featuresテストを実行し、全結果と未実行項目をPRへ記録する
- [ ] 4.3 製品実装PRで独立した仕様レビューと必須CIを完了し、マージ可能状態を確認する。今回の文書PRのレビュー・CI結果はPR本文に記録する
