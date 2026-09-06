# 初期アーキテクチャ

この文書は、AI Dev Orchestrator の本体実装を始める際の共通認識として、主要コンポーネントの高レベルな構造と責務境界を定義します。実装の詳細を固定するものではなく、未決定事項は後続の Design / RFC Issue で決定します。

## 設計方針

- 意味的な判断と、確定的な制御を分離する。
- 外部ツールごとの実行方法の違いを Provider に閉じ込める。
- テストなどの機械的な判定を、意味的なレビューから分離する。
- 未決定の詳細を先回りして共通仕様にしない。

## 高レベル構造

```text
User
  ↓
Codex (Planner / Tech Lead)
  ↓ MCP
Rust Orchestrator
  ├─ State / Policy / Budget
  ├─ Execution Ledger (local SQLite)
  ├─ Workspace / Process
  ├─ Validator
  └─ Providers
       ├─ Antigravity
       ├─ GitHub Copilot
       ├─ Codex Worker
       └─ External LLMs
```

この図は責務の関係を示す概念図です。具体的なモジュール構成、通信形式、呼び出し順序、再試行方法は定義しません。

## コンポーネントの責務

| コンポーネント | 担当すること | 担当しないこと |
| --- | --- | --- |
| Codex | Planner / Tech Lead として、利用者の目的を解釈し、計画や技術的な判断などの意味的な意思決定を行う | 状態、予算・利用枠、安全制約を強制することや、プロセスを直接管理すること |
| Rust Orchestrator | 状態、ポリシー、予算・利用枠、安全制約、ワークスペース、プロセス実行を確定的に制御し、Provider と Validator の実行を統括する | Provider 固有の実行差分を保持することや、Planner として意味的な判断を行うこと |
| Provider | Codex、Antigravity、GitHub Copilot、外部 LLM など、実行先ごとの差分を吸収する | システム全体の状態やポリシーを管理することや、検証結果から意味的な合否を決定すること |
| Validator | テスト、lint、build などを機械的に実行・判定し、結果を Orchestrator に返す | 要求の解釈、設計判断、意味的なコードレビューを行うこと |

### Planner の Provider 選択境界

`PlannerRequest` は Task の immutable snapshot と、Rust が観測した `ProviderAvailability` の
事実一覧を保持する。`Planner` は `PlannerDecision`（`provider`、`reason`、
`execution_intent`）だけを返し、Task を直接変更しない。`PlannerService` は決定を消費する前に
一覧との照合を行い、unknown / unavailable provider を `PlannerError` として拒否するため、
Codex の出力だけで状態や Provider の可否を上書きできない。

実装された `CodexPlanner` adapter は `codex exec --output-schema ... --output-last-message ...`
を read-only sandbox と ephemeral mode で実行する。Codex の spawn、non-zero exit、timeout、
cancellation、構造化出力の parse failure は、それぞれ `PlannerError` の typed failure として
返す。schema と最終出力の temporary path は adapter が所有し、終了時に cleanup する。

## 処理の流れ

1. User が目的や制約を Codex に伝える。
2. Codex が意味的な判断を行い、必要な処理を MCP 経由で Rust Orchestrator に要求する。
3. Rust Orchestrator が現在の状態、ポリシー、予算・利用枠、安全制約を確認し、許可された処理を実行する。
4. 外部の AI ツールや LLM を利用する処理は、対応する Provider を通して実行する。
5. 機械的な検証が必要な場合は Validator を実行し、その結果を記録する。
6. Rust Orchestrator が実行結果を返し、Codex が次の意味的な判断を行う。

この流れは責務境界を説明するためのものであり、具体的な状態遷移、MCP tool contract、同期・非同期の方式は未決定です。

## 境界を保つためのルール

- Codex の要求であっても、Rust Orchestrator が管理するポリシー、予算・利用枠、安全制約を迂回しない。
- Rust Orchestrator は Provider 固有の処理を直接抱えず、実行先の差分は Provider 内に閉じ込める。
- retry / escalation の Attempt 追加、最大試行回数、timeout / cancellation retry 可否、Provider 解決は Orchestrator の明示的な policy / resolver 境界で確定する。PlannerDecision は意図だけを返し、過去 Attempt は更新しない。
- hard な Planner 実行では `ExecutionPolicy` が NonZero の最大試行回数と ProviderRef 別 timeout を唯一所有し、解決・timeout・availability を domain mutation より前に検査する。Provider availability は `AgentProvider` の必須チェックであり、未実装は fail-closed となる。
- Validator の機械的な結果と、Codex による意味的な評価を同一の判定として扱わない。
- 新しい責務を追加する場合は、既存コンポーネントとの境界と、その責務を置く理由を Design / RFC Issue で確認する。

## 未決定事項

次の項目はこの文書では定義しません。

- Provider interface の具体的な形
- Task / TaskState の具体的なデータモデルと状態遷移
- Codex と Rust Orchestrator 間の MCP tool contract
- Router の具体的な入出力形式
- quota・cost 情報を Provider 共通モデルに含める範囲

これらは、実装上の必要性と選択肢が明確になった時点で、個別の Design / RFC Issue として決定します。

Execution Ledger のローカル永続化は Issue #35 で SQLite を採用した。`Task` のメタデータと
`Attempt` の 1:N 履歴、AgentResult、ValidationResult、UsageMetric、開始・終了時刻を
`SqliteExecutionLedger` が管理する。分散 DB、Dashboard、cost 集計はこの境界の対象外である。
