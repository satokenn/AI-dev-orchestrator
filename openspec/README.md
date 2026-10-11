# OpenSpecの使い方

## 何のために導入したか

利用者が、**何を作るか・どんな場面でどう動くか・何が未実装か**を確認してから、AIへ実装を依頼できるようにするためです。OpenSpecは、変更ごとに次の資料を分けて管理します。

| ファイル | 読むと分かること |
| --- | --- |
| `proposal.md` | 変更の目的、利用者への効果、対応範囲と対象外 |
| `specs/<機能名>/spec.md` | 必要な動作。入力と結果を具体的な利用例で説明する |
| `design.md` | 実現方法、処理の分担、設定の用意方法、失敗時の扱い |
| `tasks.md` | 実装・文書・検証の作業項目と、残っている作業 |

提案中の資料は`openspec/changes/<変更名>/`に置きます。変更を完了して整理すると、仕様は`openspec/specs/`、変更の記録は`openspec/changes/archive/`へ反映します。ファイルが揃っただけで、実装やレビューが完了したことにはなりません。

## 今回できたことと、まだ行っていないこと

OpenSpec CLI **1.14.1**を使います。Codex用の公式スキルは、各作業環境で初期化して利用します。[設定](config.yaml)で、日本語の説明、具体的な利用例、既存仕様との対応を指定しています。

**PR #112の統合設計を、提案中の変更として整理しています。** 最初に[変更の目的](changes/service-integration-design/proposal.md)、次に[設計の説明](changes/service-integration-design/design.md)を読んでください。[履歴の要件と利用例](changes/service-integration-design/specs/attempt-input-history/spec.md)と[残りの作業](changes/service-integration-design/tasks.md)から、必要な詳細を確認できます。

既存仕様全体の移行はまだ行っていません。承認済み仕様を置く`openspec/specs/`へ、提案中の変更を確定済みとして反映していません。既存の要件は`docs/`を参照します。製品コード、DB、実行時の動作はこの文書改訂で変わりません。

既存仕様を移すときは、[全体の構成](../docs/architecture.md)、[操作の概要](../docs/mcp-operation-contract.md)、[操作の詳細](../docs/mcp-operation-contract-reference.md)、[状態と成果物](../docs/domain-model.md)、[実装・レビュー・修正の履歴](../docs/implementation-review-model.md)、[モデル選定](../docs/model-selection-spec.md)と照合します。未マージの提案や手元の変更を、承認済みの要件として取り込みません。

## Codexで使う

`openspec init --tools codex --language Japanese --profile core`をリポジトリ直下で実行すると、`.agents/skills/`へ公式スキルが生成されます。既に初期化した作業環境では再実行は不要です。Codexでこのリポジトリを開き直すと利用できます。たとえば次のように依頼します。

```text
$openspec-propose 既存仕様の要件を変えず、利用者が読める仕様へ整理する。
誰が何を依頼し、何が返るかを利用例で示す。製品コードは変更しない。
```

資料を確認し、実装へ進める内容が決まったら、`$openspec-apply-change`で対象の変更を指定します。これはCodexへの依頼であり、端末で実行するコマンドではありません。既存の[AGENTS.md](../AGENTS.md)にある検証・レビュー・PRの完了条件も引き続き適用します。

## 端末で確認する

```shell
openspec --version
openspec list
openspec list --specs
```

変更を作成した後は、次で作業項目と仕様の形式を確認できます。

```shell
openspec status --change <変更名>
openspec validate <変更名> --strict
```

形式の検査に通っても、文章の読みやすさや実装の正しさが保証されるわけではありません。利用例が理解できるか、既存要件が保たれているか、必要な検証が実施されたかは別に確認します。

別の環境で導入する場合は、Node.js 20.19.0以上を用意して次を実行します。

```shell
npm install -g @fission-ai/openspec@1.14.1
openspec --version
```

このPRに公式スキルの生成物は含めていません。新しい作業環境では、上の初期化コマンドでCodex用スキルを用意してください。

詳細は[OpenSpec公式資料](https://github.com/Fission-AI/OpenSpec)を参照してください。
