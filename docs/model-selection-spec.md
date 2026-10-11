# モデル選定の入力・出力仕様

この文書は、監督Codexが実装・レビューの担当モデルを選ぶための情報と、返す選定結果を定める。実行先固有のCLI・API形式、利用状況の取得方法、選定処理の実装、モデル性能の評価方法は定めない。

利用可能なモデルの一覧、モデルごとの料金、どのモデルが安価かを収集・判定する本番機能は、まだ用意できていない。この文書は特定のモデルが利用可能または安価だと保証しない。例に使う実行先・モデル名・利用量・金額は、形式を説明するための架空の値である。

> [!IMPORTANT]
> この文書は後続実装のための仕様である。この文書を追加するだけでは実行時の動作は変わらない。

## 読者と前提

[開発支援システムの全体像](architecture.md)を前提とする。この文書をレビューする人が確認するのは、**監督Codexが何を根拠に担当モデルを選び、その選択をRust側がどう検査するか**である。型の実装を知らなくても、以下の要約と「入力項目の意味」「Rustが行う検証」で判断できる構成とする。

## 最初に読む要約

- **Rust側が集めるもの:** 作業の要求、実行先とモデルの候補、利用可否、使用量や制約、保存済みの実行履歴。各情報には出どころと確認時刻を付ける。
- **監督Codexが返すもの:** 今回必要な担当ごとに、選んだ実行先・モデルと、その選定理由。観測値や過去の結果を書き換えて返すものではない。
- **Rust側が実行前に確かめるもの:** 候補に存在する組合せか、必要な能力があるか、利用可能か、設定された制約に反しないか。選定理由がもっともらしくても、検査を省略しない。

たとえば「実装担当を選ぶ」という要求に、Rust側が候補A・Bと既知の利用状況を渡し、監督Codexが「Aを選ぶ、理由は実装に必要な能力があるから」と返す。Rust側はAがまだ利用可能かを再確認して実行へ渡す。**候補情報を取れない場合に、Codexがモデル一覧や利用量を想像して補うことはない。**

この文書は取得する情報の形式を定める。モデル一覧・料金の具体的な取得方法や、最も安いモデルを探す機能を提供する文書ではない。

| 読む目的 | 該当箇所 |
| --- | --- |
| 選定に渡す情報と返る情報を知る | 入出力の構造、入力項目の意味 |
| 選んだモデルを実行してよい条件を知る | 仕様の基本規則、Rustが行う検証 |
| 正確な値や型を実装する | 詳細なJSON例、末尾のRust型案 |

## 目的と適用範囲

モデル選定では、Rustが収集・記録した事実を使って、監督Codexが担当モデルを判断する。

1. Rust側が、作業の要求、候補、利用可否・制限・使用量・過去実績・実行履歴を、選定時点の記録として固定する（snapshot）。
2. 監督Codexが、その記録を比較して、必要な担当ごとに実行先とモデル、その選定理由を返す。
3. Rust側が、選定結果を元の記録と最新の事実に照合し、実行してよいものだけを実行へ渡す。

```text
Rustが事実を収集 → JSON入力 → Codexが候補を選定 → JSON出力 → Rustが検証 → 実行
```

この仕様は選定用の入出力を定める。Providerごとの呼出し方、観測値を取得する処理、モデルの性能評価方法は定義しない。Rust型・JSON形式・接続処理・DB形式の実装変更も、この文書だけでは行わない。

ここでいうPlannerはモデルを選ぶ役割を指す。目標構成では監督Codexが判断する。現行CLI内部の`CodexPlanner`をMCP経路で自動的に起動するという意味ではない。

## 入出力の構造

### JSON入力の全体像

| 項目 | 内容 |
| --- | --- |
| `schema_version` | 入力形式のバージョン |
| `request_id` | 入力と出力を対応付ける選定要求ID |
| `captured_at_ms` | Rustが入力を作成した時刻 |
| `task` | 目的、制約、Issue、選定する担当 |
| `providers` | 選択可能なProvider / Modelと、利用状況・実績 |
| `current_attempts` | 同じTaskですでに行った実行と、その結果 |

```text
task
├── issue                 Issueの本文・label・コメント
└── requested_roles[]     選定する担当と必要な能力

providers[]
├── availability          Provider全体の利用可否
├── limits[]              Provider全体の利用制限
├── api_usage             実使用量・設定予算・残予算
├── performance[]         Provider全体の過去実績
└── models[]
    ├── availability      Modelの利用可否
    ├── limits[]          Model固有の利用制限
    ├── capabilities[]    対応できる作業
    ├── performance[]     Modelの過去実績
    └── estimated_execution[]  次の1実行の見積り

current_attempts[]        現在のTaskで行った実行履歴
```

### JSON出力の全体像

監督Codexは、入力で要求された各担当について、次の項目だけを返す。

| 項目 | 内容 |
| --- | --- |
| `role` | 選定する担当。例: `implementer` |
| `target.provider` | 選んだProvider |
| `target.model` | 選んだModel、またはProviderの既定Model |
| `reason` | その候補を選んだ理由 |

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
      "reason": "利用可能で、実装に必要な能力を満たす候補だから"
    }
  ]
}
```

### 値の確かさ

利用量や残予算等の値には、数値だけでなく、その値をどのように得たかを付ける。

| 表現 | 意味 | 例 |
| --- | --- | --- |
| `measured` | 実行先のAPI等から実測した | 現在までのトークン使用量 |
| `configured` | 利用者やリポジトリ側が設定した | 1日の予算上限 |
| `computed` | 他の既知の値から計算した | 上限から使用量を引いた残量 |
| `estimated` | 過去実績等から見積もった | 次の1実行にかかる費用 |
| `unknown` | 取得または計算できない | 実行先が残量を返さない |

`unknown` を `0` や推定値へ置き換えず、取得できない理由と確認時刻を保持する。

## 仕様の基本規則

- 入力と出力の形式の版は、両方とも`schema_version`で示す。この選定形式のv1は整数`1`であり、MCP操作の文字列`"v1"`とは別の形式である。
- Rust側が選定要求ごとに`request_id`を生成する。監督Codexは同じ値を返し、別の要求の選定結果を混ぜない。
- 実行先（Provider）とモデル（Model）は別の識別子にし、実行対象には両方を指定する。
- モデル指定は、名前を指定する`named`か、実行先の既定モデルを使う`provider_default`のどちらかにする。省略や空文字列で代用しない。
- 情報を取得できない場合は`unknown`と理由を残す。ゼロ・空文字列・推定値で補わない。
- 金額・トークン数・要求回数などは値と単位を組にする。異なる単位を一つの得点へ換算しない。
- 推定値は比較材料として使えるが、利用可否や強制する上限・予算の検査を上書きしない。
- 監督Codexが返す選定結果には、作業・実行の状態、利用可否・使用量・制限、履歴を含めない。Rust側が観測した事実を選定結果から書き換えないためである。

## 入力項目の意味

### 作業の要求と必要な担当

`task`には目的、利用者の制約、現在の状態、今回選ぶ担当を入れる。GitHub Issueを起点にする場合は、選定時点のタイトル・本文・ラベル・コメントを順序を保って含める。コメントには要求変更や設計判断があり得るためである。

Rust側は、入力を固定する前に秘密情報、認証情報、伏せ字化していない実行先のログを除外する。

`requested_roles`は「今回どの担当を選ぶか」を示す。出力は各担当に対して一つの割当を返す。実装担当とレビュー担当を同時に要求できるが、必ず異なるモデルを選ぶという規則ではない。必要な能力と実行許可の規則をRust側が提示し、監督Codexがその範囲で判断する。

### 実行先とモデルの候補

`ProviderSnapshot`には、認証・利用契約・APIアカウントなど、実行先全体に関わる事実を入れる。`ModelSnapshot`には、その実行先の個別モデル、または候補として指定された既定モデルを入れる。実行対象は実行先とモデルの組で特定する。

候補になるのは、両方の利用可否が`available`で、担当に必要な能力をすべて満たす組だけである。どちらかが`unavailable`（利用不可）や`unknown`（確認不能）なら実行しない。監督Codexが理由を付けても、この拒否を解除できない。

モデル一覧を取得できない実行先は、空の`models`を渡して「候補なし」と見せかけない。既定モデルの実行を許す場合は、`ModelChoice::ProviderDefault`に対応する候補を一件渡す。

### 利用制限と使用量

`limits`は、要求回数・トークン数・クレジット・同時実行数などの制限を表す。名前、適用範囲、期間、上限、使用量、残量、リセット時刻を別々に保持する。実行先全体の制限は`ProviderSnapshot.limits`、モデル固有の制限は`ModelSnapshot.limits`に置く。同じ事実を両方へ複製しない。

制約の適用方法（`ConstraintEnforcement`）は、Rust側の実行設定（`ExecutionPolicy`）が決める。`hard`は実行を許可する条件、`advisory`は比較の参考情報である。監督Codexが変更するものではない。

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

`performance`は記録用DB（Ledger）から集計した、期間付きの実績である。実行先・モデルと、必要なら担当ごとに集計する。成功率だけで母数を隠さず、モデル実行・終了状態・機械検証・レビュー・再試行の件数を渡す。

集計値は`computed`、取得元は`execution_ledger`とする。レビュー結果がまだ保存されていない場合は、その集計値を`unknown`とする。保存済みの`ReviewOutcome`は、`Approved`を`review_approved`、`ChangesRequested`を`review_changes_requested`、`Inconclusive`を`review_inconclusive`へそれぞれ一件加算する。一件を複数分類へ加算せず、どの結果も集計から除外しない。

`current_attempts`は同じTaskに属するすべてのAttemptを、履歴番号（`sequence`）順に渡す。直前の失敗だけでなく、モデルの切替・検証・レビュー・累積の再試行を次の判断に使えるようにする。

伏せ字化していない標準出力・標準エラー出力や機密情報は渡さない。選定に必要な分類と短い要約だけを渡す。過去の実行記録を新しい選定結果で上書きしない。

## 入力と出力の詳細なJSON例

次の例は、同じ実行先でモデル名を指定する場合と既定モデルを使う場合、実測値・推定値・不明な値、複数担当への割当を示す。`example_api`、`sample-model-a`、利用量、金額はすべて架空であり、実在する
Provider / Model の利用可否や価格を示さない。説明のため一部の空配列と履歴 field は省略している。
実装する schema では Rust 型案にある必須 field を省略しない。

例示上は入力と出力を見比べられるよう、外側に `input` と `output` を並べている。実際の受け渡しでは
この外側のobjectは使わず、`input` の値がCodexへの入力、`output` の値がCodexからの出力になる。

<details>
<summary>詳細なJSON例を表示する</summary>

```json
{
  "input": {
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
  },
  "output": {
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
}
```

選定結果にレビュー担当（`reviewer`）が含まれても、レビューが必須になるわけではない。監督Codexが必要な担当だけを要求する。履歴の関係はServiceが入力と確認済みの記録から保存する。選定理由（`reason`）はモデルを選んだ理由であり、再試行やレビュー指摘への修正を行った理由として使わない。

</details>

## Rustが行う検証

選定結果を実行へ渡す前に、Rust側が次を確認する。

1. `schema_version`を解釈でき、`request_id`が入力と一致する。
2. 要求した各担当が出力に一回ずつ現れ、未知・重複・欠落の担当がない。
3. 選定理由（`reason`）が空白だけではない。
4. 実行先とモデルの組が、選定に渡した候補に存在する。
5. 実行先とモデルの利用可否が、両方とも`available`である。
6. モデルが担当に必要な能力をすべて持つ。
7. Rust側が設定した予算・利用枠・再試行上限・安全条件に反しない。
8. 実行直前に利用可否と強制する制約を再取得し、選定後に変化した事実にも反しない。

1〜6は返された選定結果と元の入力の照合、7〜8は現在の事実による実行許可の確認である。両方を通過した値（`ValidatedPlannerDecision`相当）だけを実行へ渡す。

失敗した場合は、未知の実行対象・利用不能・古い選定結果・許可条件違反など、種類を識別できるエラーを返す。監督Codexの選定理由で検査を迂回しない。

選定前にも、Rust側は入力の識別子・担当・実行先とモデルの組が一意で空文字列がないこと、`unavailable`や`unknown`に理由があること、各値の取得方法が本書の規則に合うことを確認する。ある担当に実行可能な候補が一件もなければ、モデルに架空の候補を作らせず、候補なしを示す`no eligible target`エラーを返す。

### 強制する上限の残量が不明な場合

`hard`の`ResourceLimitSnapshot.remaining`や`remaining_budget.value`が`unknown`なら、Rust側が実行先の接続処理や使用量の収集処理から値を再取得するまでは実行可能とみなさない。

- 再取得後も不明なら、`SelectionValidationError::ConstraintIndeterminate { scope, name }`で拒否する。
- 残量が不足していると分かった場合は、`SelectionValidationError::ConstraintExceeded { scope, name }`で拒否する。
- `advisory`の値は不明でも実行を妨げない。不明であることと理由を監督Codexへ渡す。

実行直前の再確認にも同じ規則を適用する。監督Codexの割当や理由で制約の適用方法を変えない。

## 監督CodexとRust側の分担

| 情報・操作 | Rust側 | 監督Codex |
| --- | --- | --- |
| 要求と候補情報の収集・秘密情報の除外 | 収集し、出どころ・時刻とともに渡す | 書き換えずに受け取る |
| API使用量・設定予算・残量 | 取得・計算の根拠を保持する | 比較材料として使う |
| トークン数・費用の推定 | `estimated`と明示して渡す | 不確実性を考慮する |
| 過去実績・実行履歴 | DBから集計し、当時の記録を保持する | 次のモデル選定に使う |
| 担当ごとの実行先・モデル選定 | 候補と強制する制約を提示する | 選んだ組と理由を返す |
| Task・Attemptの状態変更 | ドメインのAPIを通して適用する | 選定結果から書き換えない |
| 実行前の再検証・制約の強制 | 検査し、違反は拒否する | 迂回できない |

選定結果は「この担当を選ぶ」という判断である。Operation Serviceが実行の作成・状態変更・実行を検証して適用する。選定だけでTaskを完了にしない。実装とレビューの記録の意味は#73、モデル指定と実際に使用したモデルの記録は#59、選定用の情報収集は#60、過去の性能実績の集計は#67が担当する。

## 後続実装への適用順

1. Issue #58 で担当とレビュー結果をドメインと記録用DBのどこへ保持するか決める。
2. Issue #59 で `ModelChoice` と実際に使用した Model を Provider / Attempt / Ledger へ接続する。
3. Issue #60 で出どころと確認時刻を持つ観測値、使用量、実行履歴の要約を収集する。性能実績の集計は Issue #67 が担当する。
4. Issue #61 で既存 `PlannerRequest` / `PlannerDecision` をこの入力／出力へ拡張し、Rust 側検証を実装する。

各Issueは共通の項目を実行先固有の形式へ置き換えず、取得できない項目は`unknown`として
保持する。仕様の互換性を壊す変更は `schema_version` を上げ、入力と出力を同時に更新する。

## 実装担当者向けのRust型案

ここからは、上で説明した情報をRust型とJSONの項目へ対応させる参照資料である。設計の目的や分担を確認する場合は、上の説明を先に読む。型名・値の名前は実装上の名前として保持する。

<details>
<summary>Rust型定義の詳細を表示する</summary>

以下はv1の項目と必須性を示す型案である。実装では`serde`を使い、分類を識別する値付きの列挙型（tagged enum）にする。JSONの項目名は`snake_case`とする。時刻は既存DBと同じUnix時刻のミリ秒、量は丸めや浮動小数点誤差を避けるため十進数の文字列で表す。

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
