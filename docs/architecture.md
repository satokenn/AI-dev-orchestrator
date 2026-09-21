# 初期アーキテクチャ

主経路は、監督Codexが意味的に判断し、Rust の Operation Service が確定的な操作を実行・記録する構成である。これは後続実装の設計であり、既存の Rust 内部 `CodexPlanner` を主経路として拡張する方針ではない。

```text
User
  ↓
監督Codex ── operation request ──> Rust Operation Service
  ↑                                  ├─ Domain / Policy / Budget / Ledger
  └── result / observation ──────────├─ Workspace / Artifact
                                     ├─ Provider / Model
                                     ├─ Validator
                                     └─ GitHub adapter / CI
```

| 境界 | 担当すること | 担当しないこと |
| --- | --- | --- |
| 監督Codex | 要求解釈、Provider / Model選択、実行・review・再試行の要否、成果物の採否、Task完了判断 | 状態・usage・CI結果の書換え、policyの迂回 |
| Rust Operation Service | request / revision検証、状態遷移、排他、予算・安全制約、実行・永続化、結果返却 | 次の仕事・モデル・成果物の意味的な採否を自発的に決めること |
| Provider / Model | 指定されたモデル呼び出し、入出力と使用量の観測 | Task全体の成功判定 |
| Validator | 指定成果物への機械的checkと結果返却 | 要求解釈・意味的レビュー |
| GitHub adapter | 指定成果物の公開、指定SHAのCI観測 | Task達成・mergeの判断 |

Attempt は Provider / Model 呼び出し1回の記録であり、`Succeeded` は呼び出しの正常終了だけを示す。Validation、AI review verdict、監督Codexの受入、公開/CI、Task完了は、同じ成果物へ束縛する別々の事実である。詳細は[作業・モデル実行・成果物のドメインモデル](domain-model.md)を正本とする。

モデル選定の入力・出力は[モデル選定の入力・出力仕様](model-selection-spec.md)、実装・review・修正の履歴関係は[実装・レビュー・修正を記録する設計](implementation-review-model.md)を参照する。#66 はこの Operation Service、#70 は成果物同一性、#71 は修正Attemptの成果物引継ぎを実装する。

監督Codexから Operation Service を呼ぶ MCP tool の名前、request / response、識別子、冪等性、非同期・取消・失敗、秘匿境界は、[監督Codex向け MCP Operation Contract](mcp-operation-contract.md)を正本とする。MCP transport 実装、SQLite schema、Provider interface、AI review Provider、並列workflow engineは後続 Issue で実装する。
