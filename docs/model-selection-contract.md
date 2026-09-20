# モデル選定コンテキスト／結果契約

この文書は、Codex Planner が実装・レビュー等の担当 Provider / Model を選ぶために受け取る
入力と、Rust Orchestrator へ返す選定結果の正本を定義する。Provider 固有の CLI / API 形式、
利用状況の収集方法、Planner の実装、モデル性能の評価方法は定義しない。

## 目的と適用範囲

モデル選定では、Rust が収集・記録した事実と、Codex が行う意味的な判断を分離する。

1. Rust は Task / Issue、実行候補、利用可能性、利用制限、API 利用状況、過去実績、
   現在の Task の Attempt 履歴を immutable snapshot として組み立てる。
2. Codex は snapshot を比較し、要求された role ごとに実行対象と理由だけを返す。
3. Rust は結果を snapshot および最新の実行時事実と照合し、許可された選定だけを実行へ渡す。

この契約は既存の `PlannerRequest` / `PlannerDecision` を後続 Issue で拡張するための設計であり、
この Issue では Rust 型、JSON schema、Provider adapter、Ledger schema を変更しない。

## 契約の基本規則

- schema version は入力と出力の両方に必須とし、v1 は整数 `1` とする。
- `request_id` は Rust が選定要求ごとに生成する。出力は同じ値をそのまま返し、別 snapshot の
  結果を誤適用しない。
- Provider と Model は別の識別子で表し、実行対象は常に両方を含む。
- Model は明示名または `provider_default` の判別可能な値とする。Model field の省略や
  空文字は許可しない。
- 取得できない値を `0`、空文字、推定値で補わず、`unknown` として理由を保持する。
- 金額、token、request 数等を単一 score に換算しない。値と単位を組にして保持する。
- 推定値は判断材料にはできるが、availability、hard limit、予算等の Rust 側検証を上書きしない。
- Planner 出力には Task / Attempt state、availability、usage、limit、履歴を含めない。これらを
  Codex から返させないことで、観測事実の書き換えを契約上も禁止する。

## Rust 型案

以下は v1 の意味と必須性を示す型案である。実装時は `serde` の tagged enum を使い、
JSON field は `snake_case` とする。時刻は既存 Ledger と同じ Unix milliseconds、量は丸めや
浮動小数点誤差を避けるため decimal string とする。

JSON では `ModelChoice` を `kind`、`Evidence` を `status` で判別する tagged object とする。
`Evidence::Known` は `status: "known"` と `basis`、`Evidence::Unknown` は
`status: "unknown"` と `reason` を持つ。`AvailabilityStatus` だけは
`AvailabilityObservation` へ flatten し、`status: "available"`、または
`status: "unavailable" | "unknown"` と non-empty `reason` を同じ object に置く。
`status` object を入れ子にする wire 形式は許可しない。

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
    target: AttemptTarget,
    state: AttemptState,
    failure: Option<AttemptFailureSummary>,
    validation: Vec<ValidationSummary>,
    review: Option<ReviewSummary>,
    usage: Evidence<Vec<NamedMetric>>,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
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

`SelectionRole` と `CapabilityRef` は v1 では拡張可能な non-empty string newtype とする。
既存 `TaskRole` は現在の単一 role を `requested_roles` の1要素へ変換できる。複数 role を Domain 上で
どの単位に保持するか、Attempt に role を持たせるか、`ReviewSummary` をどこへ永続化するかは
Issue #58 の決定事項であり、この契約はその結論を先取りしない。未導入の情報は
`AttemptSummary.role` / `review` を省略し、履歴自体を捏造しない。

`provider_default` は「Model を指定しない」の暗黙表現ではない。Rust が候補として明示した場合だけ
選べる判別値である。Provider が実際に解決した Model の記録方法は Issue #59 で定義する。
既存 Ledger のように過去 Attempt の Model を記録していない場合は、`AttemptTarget.model` を
`unknown` とする。選定候補と Planner 出力の `ExecutionTarget.model` に `unknown` は許可しない。

## 入力の意味

### Task / Issue

`task` は選定対象の目的、利用者の制約、現在 state、選定が必要な role を保持する。GitHub Issue が
起点の場合は、選定時点の title、body、labels、comments を順序を保って含める。コメントは
要求変更や設計判断を含み得るため入力対象とするが、secret、認証情報、raw provider log は
snapshot 作成前に Rust 側で除外する。

`requested_roles` は「今回どの担当を選ぶか」を表す。v1 の出力は各要素に対してちょうど1つの
assignment を返す。実装担当とレビュー担当を同時に要求できるが、両者を異なる target にするかは
Rust が組み立てる capability / policy と、Codex の意味的判断に従う。

### Provider / Model 候補

`ProviderSnapshot` は認証、subscription、API account 等の Provider 全体に関わる事実を持ち、
`ModelSnapshot` は同じ Provider 内の個別 Model または明示された provider default を持つ。
実行可能な target は `provider` と `model` の組で一意に決まる。

Provider と Model の両方が `available` で、role の required capability を満たす組だけが実行候補になる。
いずれかが `unavailable` または `unknown` なら fail-closed とし、Codex が理由を付けても実行しない。
候補を列挙できない Provider を空の `models` で渡してはならない。default 実行を許す場合は
`ModelChoice::ProviderDefault` の `ModelSnapshot` を1件渡す。

### 利用制限と API 利用状況

`limits` は request / token / credit / concurrency 等を、名前、scope、window、上限、使用量、残量、
reset 時刻に分けて保持する。Provider 全体の制限は `ProviderSnapshot.limits`、Model 固有の制限は
`ModelSnapshot.limits` に置く。同じ事実を両方へ複製しない。

`ConstraintEnforcement` は Rust が所有する `ExecutionPolicy` から設定し、Codex は変更できない。
`hard` は実行許可を左右する制約、`advisory` は選定時の比較材料を表す。budget policy が設定されて
いない metric は `configured_budget` / `remaining_budget` に架空の `unknown` entry を作らず、
entry 自体を持たない。同じ scope / name の configured budget と remaining budget は同じ
enforcement を持たなければならず、不一致は Rust が不正な入力 snapshot として拒否する。

API の実使用量は `actual_usage` に `measured` として、設定予算は `configured_budget` に
`configured` として入れる。実使用量と設定予算から算出した残予算は `computed` とし、計算元を
`source.reference` で追跡できるようにする。将来の1実行の token / cost 見込みは
`estimated_execution` に `estimated` として入れる。取得不能な quota、残量、価格は
`Evidence::Unknown` とし、他 Provider の価格や過去平均から実測値を作らない。
`ApiUsageSnapshot.scope` は account / organization / project / repository budget 等、値が適用される
範囲を表す。`api_usage: None` は API 利用状況が対象外の場合だけに使い、対象だが取得不能な場合は
各 metric を `unknown` とする。

`MetricValue.unit` には `input_token`、`output_token`、`request`、ISO 4217 の通貨 code 等、
値の意味を失わない単位を使用する。異なる単位の値は Planner へそのまま渡し、Rust が暗黙に
換算・合算しない。

### 過去実績と現在 Task の Attempt 履歴

`performance` は Provider / Model と、必要なら role ごとに Ledger から集計した期間付きの実績である。
母数を隠した成功率だけを渡さず、Attempt、終了状態、Validation、review、retry の件数を渡す。
集計値は `computed`、source は `execution_ledger` とする。review が Domain / Ledger に未導入なら
該当値を `unknown` とする。保存された `ReviewOutcome` は `Approved`、`ChangesRequested`、
`Inconclusive` をそれぞれ `review_approved`、`review_changes_requested`、`review_inconclusive` へ
排他的に1件加算し、どの outcome も集計から除外しない。

`current_attempts` は現在の Task に属する全 Attempt を `sequence` 順で渡す。これにより、直前の失敗だけで
なく、Provider / Model の切替、Validation、review、累積 retry を再判断へ利用できる。raw stdout / stderr
や機密情報は渡さず、選定に必要な分類と短い summary だけを渡す。過去 Attempt は immutable record で
あり、新しい選定結果で上書きしない。

## JSON 例

次の例は、同じ Provider 内の named model と provider default、実測値、推定値、unknown、複数 role の
assignment を示す。説明のため一部の空配列と履歴 field は省略している。実装する schema では
Rust 型案にある必須 field を省略しない。

```json
{
  "input": {
    "schema_version": 1,
    "request_id": "selection-01J...",
    "captured_at_ms": 1790000000000,
    "task": {
      "task_id": "task-57",
      "objective": "Issue #57 の契約を実装する",
      "constraints": ["既存の状態遷移を迂回しない"],
      "state": "active",
      "issue": {
        "repository": "owner/repository",
        "number": 57,
        "url": "https://github.com/owner/repository/issues/57",
        "title": "モデル選定契約を定義する",
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
            "model": {"kind": "named", "model": "economy-model"},
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
          "model": {"kind": "named", "model": "economy-model"}
        },
        "reason": "利用可能で必要 capability を満たし、推定コストが低い候補だから"
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

## Rust が行う検証

Planner 結果を Task / Attempt へ適用する前に、Rust は少なくとも次を検証する。

1. `schema_version` を実装が解釈でき、`request_id` が入力と一致する。
2. `requested_roles` の各 role が出力にちょうど1回現れ、未知・重複・欠落 role がない。
3. `reason` が空白だけではない。
4. `ExecutionTarget` の Provider / Model の組が入力 snapshot に存在する。
5. Provider と Model の availability がともに `available` である。
6. Model が role の required capability をすべて持つ。
7. Rust が所有する予算、利用枠、retry 上限、安全 policy に反しない。
8. 実行直前に availability と `hard` constraint を再取得し、snapshot 後に変化した事実に反しない。

1〜6は選定結果の構造と snapshot に対する検証、7〜8は現在事実に対する実行許可である。
`ValidatedPlannerDecision` 相当の値は両方を通過して初めて Provider 実行へ渡せる。検証失敗は
未知 target、利用不能、stale decision、policy violation 等の typed error として返し、Codex の
reason を根拠に迂回しない。

Planner を呼ぶ前にも、Rust は入力の ID、role、Provider / Model の組が一意で、空文字がなく、
`unavailable` / `unknown` に理由があり、Evidence の配置と basis が上記規則に合うことを検証する。
各 requested role に実行可能な候補が1件もない場合は、Planner に架空の target を作らせず、
Rust が typed `no eligible target` error を返す。

`hard` な `ResourceLimitSnapshot.remaining` または `remaining_budget.value` が `unknown` の target は、
Rust が Provider adapter / usage collector から値を再取得するまで実行可能とみなさない。再取得後も
unknown なら `SelectionValidationError::ConstraintIndeterminate { scope, name }`、既知の残量が
不足していれば `SelectionValidationError::ConstraintExceeded { scope, name }` として fail-closed に
拒否する。`advisory` な値は unknown のままでも実行を妨げず、unknown である事実と理由を Codex へ
渡す。実行直前の再確認にも同じ規則を適用し、Codex の assignment や reason で enforcement を
変更しない。

## Codex と Rust の責務境界

| 情報・操作 | Rust Orchestrator | Codex Planner |
| --- | --- | --- |
| Task / Issue snapshot の収集・secret 除外 | 所有する | 受け取るだけ |
| Provider / Model の列挙、availability、limit | 観測し source / 時刻付きで渡す | 書き換えず比較する |
| API 実使用量、設定予算、残量 | 収集・計算根拠を保持する | 判断材料として使う |
| 推定 token / cost | `estimated` と明示して渡す | 不確実性を考慮して比較する |
| 過去実績・Attempt 履歴 | Ledger から集計し immutable に渡す | 次の target 選定に使う |
| role ごとの Provider / Model 選定 | 候補と hard constraint を提示する | target と reason を返す |
| Task / Attempt state の変更 | Domain API 経由でのみ適用する | 変更しない |
| 実行時の再検証、予算・policy 強制 | 所有する | 迂回できない |

Codex が返すのは assignment という意図であり、Attempt の作成、retry / escalation、実行順、
review 後の再作業、Task の終了を直接確定しない。これらを現在の workflow へ接続する処理は
Issue #61、実装と review の Domain 表現は Issue #58、Provider への Model 指定と記録は
Issue #59、snapshot の収集・集計は Issue #60 で実装する。

## 後続実装への適用順

1. Issue #58 で role / review を Domain と Ledger のどこへ保持するか決める。
2. Issue #59 で `ModelChoice` と実際に使用した Model を Provider / Attempt / Ledger へ接続する。
3. Issue #60 で source と時刻を持つ observation、usage、performance、Attempt summary を収集する。
4. Issue #61 で既存 `PlannerRequest` / `PlannerDecision` をこの入力／出力へ拡張し、Rust 側検証を実装する。

各 Issue はこの契約の field を Provider 固有形式へ置き換えず、取得不能な field は `unknown` として
保持する。契約の互換性を壊す変更は `schema_version` を上げ、入力と出力を同時に更新する。
