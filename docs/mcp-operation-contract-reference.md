# MCP 操作契約 — wire format と詳細規則

この文書は[概要と操作の読み方](mcp-operation-contract.md)を補う、tool request / response の field と詳細規則のリファレンスである。ここで定めるのは契約であり、実装済みであることやテストが通ったことを示すものではない。

## 用語

| 用語 | この契約での意味 |
| --- | --- |
| Task revision | Taskに保存された事実の版番号。依頼側が見た版を特定する |
| request_id | 呼出側が副作用を伴う依頼に付け、再送を識別するID |
| operation / operation_id | 受付後に非同期実行される処理 / その処理を状態・結果・ログ取得・取消で指定するID |
| Attempt | Provider / Model 呼び出し1回の履歴 |
| Artifact | 特定時点のworkspace内容を識別する参照 |
| section | `task.get_context` が返す情報の分類。許可値と各 section の item 内容は「context response」で定義する。Task 自体は常に別の top-level `task` object で返す |
| items | section に属する、時系列順の要約 entry 配列。entry は共通 envelope と section 固有の `details` を持つ（「context response」参照） |
| cursor | その section の次ページを取得するための不透明 token。クライアントは内容を生成・解釈せず、返された値をそのまま再送する |
| next_cursor | 次ページがあれば cursor string、現在のページが最後なら `null`。次ページ取得時は同じ section の `cursors` にこの値を渡す |
| evidence | 保存済みの検証・review・判断・公開・CI観測記録を特定する `{kind, id}` 参照。呼出側が結果や状態を申告する欄ではない。ID の意味と照合条件は「証拠の参照」で定義する |
| 共通 operation response | 非同期 tool が受付時に返す共通 object。受付を識別する request / operation、受付状態、Task revision を含む。「共通 operation response schema」で全 field を定義する |

Domain用語の意味は[ドメインモデル](domain-model.md)を正本とする。

## Tool request / response

JSON の object と array は、それぞれ {...} と [...] で表す。Request 欄に「必須」とした field はすべて指定する。型や許可値に合わない request、定義されていない request field は invalid_request で拒否する。Response 欄は必須の返却 field を示し、クライアントは未知の追加 response field を無視する。

### Task を作る、状況を読む

| Tool | Request | Response |
| --- | --- | --- |
| task.create | 必須: schema_version=v1、request_id (string)、source (issue / manual)、title (string)、description (string)、constraints (string array)。任意: issue object (url、number、title、body) | task_id、revision、state=pending、保存した要求と Issue 情報 |
| task.get_context | 必須: schema_version、task_id、sections (string array)。任意: page_size (1–100、既定20)、cursors (section 名から cursor への map) | `task`、要求された各 section の `{items, next_cursor}`、`observed_at` |

#### Context response

`sections` は `providers`、`usage`、`attempts`、`artifacts`、`validations`、`reviews`、`decisions`、`publication`、`ci` から重複なく選ぶ。Task 自体は常に返るので `task` は section 値として指定できない。各 section は次の形で返す。

~~~json
{
  "task": {"id": "task-1", "revision": 12, "state": "active", "request": {}},
  "sections": {
    "attempts": {
      "items": [
        {"id": "attempt-1", "kind": "attempt", "state": "succeeded", "occurred_at": "2026-09-23T01:02:03Z", "summary": "実装 Provider の実行が完了", "references": [{"kind": "artifact", "id": "artifact-1"}], "details": {"provider_id": "provider-x", "model_id": "model-y", "role": "implementer"}}
      ],
      "next_cursor": null
    }
  },
  "observed_at": "2026-09-23T01:02:04Z"
}
~~~

全 item の共通 field は `id`（その記録の安定 ID）、`kind`（下表の記録種別）、`state`（その種別で定義された状態。状態を持たない item は `null`）、`occurred_at`（記録時刻、RFC 3339 UTC）、`summary`（短い人間可読要約）、`references`（関連記録の `{kind, id}` 配列）、`details`（下表の項目）である。詳細ログ・全文 diff は含めない。

| section | kind / details に含める内容 |
| --- | --- |
| providers | `provider`。`provider_id`、利用可能な `model_ids`、観測状態と観測時刻、利用不可の場合の診断参照 |
| usage | `usage`。計測名、値（数値または `null`）、単位、観測状態、観測時刻。未観測値は 0 にしない |
| attempts | `attempt`。`provider_id`、`model_id`、`role`、入力 Artifact または base commit、出力 Artifact 参照、usage、診断参照 |
| artifacts | `artifact`。Task 内 Artifact ID、内容 digest、作成元 Attempt または初期 base commit、限定 diff 参照 |
| validations | `validation`。Validation ID、対象 Artifact、profile / checks、各 check の結果 |
| reviews | `review_verdict`。ReviewVerdict ID、reviewer Attempt ID、対象 Artifact、verdict |
| decisions | `codex_decision`。Decision ID、対象 Artifact、監督Codexの判断、理由、evidence 参照 |
| publication | `publication`。Publication ID、Artifact ID、repository、head SHA、PR 番号・URL |
| ci | `ci_observation`。観測 ID、repository、PR 番号または commit、観測対象 head SHA、状態、check 要約 |

各 section の item は `occurred_at` の降順、同時刻なら `id` の辞書順降順で返す。`page_size` は各 section に独立して適用する。cursor は Task、section、page size、読み取り開始時の revision に束縛される。続きの要求では最初と同じ `page_size` を使う。revision が変わるなど cursor を使えない場合は `invalid_cursor` を返し、最新 context を読み直して先頭ページから取得する。異なる section の cursor を流用してはならない。読み取り tool は Task revision を増やさない。

source が issue のときは issue object を必須とし、manual のときは指定しない。Issue の title と body は取り込んだ時点の snapshot として保存する。

### Provider を実行し、operation を扱う

| Tool | Request | Response |
| --- | --- | --- |
| attempt.run | 共通変更 field に加え、provider_id、model_id、instruction (string)、role (implementer / reviewer / explorer)、input が必須。input は Artifact ID または {repository, commit} の一方。任意: timeout_ms | 共通 operation response と attempt_id。完了後の結果は Attempt state、出力 Artifact、usage、診断参照を含む |
| operation.get | schema_version と operation_id が必須 | operation ID、種類、状態、受付・開始・終了時刻、result、error |
| operation.list_logs | schema_version、operation_id、stream (stdout / stderr / diagnostic) が必須。任意: cursor、limit (1–1000、既定100) | chunks (連番、時刻、本文、redacted flag)、next_cursor |
| operation.cancel | 共通変更 field と operation_id が必須 | 共通 operation response と対象 operation の現在状態 |
| task.cancel | 共通変更 field が必須。任意: reason (string) | 共通 operation response と Task / 未終了 operation の取消要求状態 |

attempt.run の input Artifact は同じ Task に属していなければならない。初回実行では repository と commit を指定する。branch や filesystem path だけの指定は認めない。timeout_ms を省略した場合は Rust policy の値を使い、request から予算や権限を増やせない。

review も reviewer role の Attempt として実行する。reviewer の出力が所定形式なら、Rust が reviewer Attempt と対象 Artifact に ReviewVerdict を記録する。監督Codexは ReviewVerdict を直接作成・変更しない。

### Artifact を検証し、採否を記録する

| Tool | Request | Response |
| --- | --- | --- |
| validation.run | 共通変更 field、artifact_id、check_profile_id または checks の一方が必須 | 共通 operation response、Artifact ID。完了後は Validation ID と各 check の名前、状態、診断参照 |
| decision.record | 共通変更 field、artifact_id、decision (accepted / rejected / changes_requested)、reason が必須。任意: evidence array | Decision ID、Artifact ID、判断、理由、evidence、更新後 revision |

明示的な checks の各要素は name、command、args (string array)、timeout_ms から成る。Rust は実行前に command と workspace が policy allowlist に合うことを確認する。check の状態は passed、failed、unknown のいずれかである。

evidence の各要素は kind と id を持つ。kind は validation、review、decision、publication、ci のいずれか。Rust は参照先が同じ Task と Artifact の証拠であることを確認する。reviewer の approved と監督Codexの accepted は異なる判断である。

#### 証拠の参照

`evidence` は次の形の配列である。`id` は記録を作成した Rust service が発行した ID であり、呼出側が新しい結果を捏造するための値ではない。

~~~json
[
  {"kind": "validation", "id": "validation-1"},
  {"kind": "review", "id": "review-verdict-1"},
  {"kind": "ci", "id": "ci-observation-1"}
]
~~~

各 kind の ID は、それぞれ ValidationResult、ReviewVerdict、CodexDecision、Publication、CI observation の記録を指す。Rust は ID が存在することに加え、記録が同じ Task と対象 Artifact に属することを解決して検査する。publication / CI evidence では、CI 観測がその Publication の repository、PR、公開 head SHA と一致することも検査する。CI の `passed` は特定 SHA を観測した記録であり、別 SHA の成功を流用できない。存在しない ID は `invalid_request`、別 Artifact の記録は `evidence_artifact_mismatch` で拒否し、Task や外部状態を変更しない。CI 観測 ID は `ci.get` が返し永続化する ID とする。

### PR を公開し、CI を確認し、Task を完了する

| Tool | Request | Response |
| --- | --- | --- |
| publication.publish | 共通変更 field、artifact_id、base_branch、head_branch、title、body が必須。任意: evidence array | 共通 operation response と Artifact ID。完了後は Publication ID、repository、公開した head SHA、Pull Request 番号と URL |
| ci.get | schema_version と対象指定が必須。対象は publication ID、{repository, number} の PR、{repository, sha} の commit のいずれか一つ | 対象指定、観測時刻、各 check の名前・状態・URL・終了時刻、全体状態 |
| ci.wait | 共通変更 field、ci.get と同じ対象指定、deadline (RFC 3339 timestamp) が必須 | 共通 operation response。完了時は ci.get と同じ結果、期限時は timeout と最後の観測値 |
| task.finish | 共通変更 field、artifact_id、decision_id が必須。任意: evidence array | Task ID、更新後 revision、state=completed、完了判断に結び付けた Artifact と証拠 |

CI 全体の状態は pending、passed、failed、unknown のいずれか。task.finish は、同じ Artifact に対する accepted Decision と、policy が必要とする Validation / publication / CI の証拠がそろった場合だけ成功する。不足時は policy_denied を返し、Task を変更しない。自動 merge は行わない。

## 共通の request / response 規則

既存 Task に副作用を起こす request には、すべて次の field を含める。tool 固有 field は同じ object に追加する。

~~~json
{
  "schema_version": "v1",
  "request_id": "呼出側が生成する一意 ID",
  "task_id": "Rust が返した Task ID",
  "expected_revision": 12
}
~~~

request_id は副作用 request ごとに呼出側が生成する。同じ request を再送するときだけ同じ値を使う。Rust は caller、tool 名、payload の組を保存し、同じ payload の再送には保存済み response を返す。同じ ID で payload が変わっていれば idempotency_conflict とする。

expected_revision は最後に観測した Task revision である。現在値と違う場合、Rust は stale_revision と現在 revision を返し、副作用を始めない。request を作り直す場合は context を再取得して、新しい request ID と最新 revision を使う。

非同期操作の受付 response は以下を共通して返す。

~~~json
{
  "schema_version": "v1",
  "request_id": "同じ request ID",
  "task_id": "同じ Task ID",
  "revision": 13,
  "operation": {
    "operation_id": "Rust が生成する ID",
    "kind": "attempt.run",
    "state": "accepted"
  },
  "attempt_id": "Attempt を作る操作の場合のみ"
}
~~~

#### 共通 operation response schema

上例の全 field の意味は次のとおり。

| Field | 意味 |
| --- | --- |
| `schema_version` | response の形式。現行値は `v1` |
| `request_id` | 受付対象となった request の ID。再送時も同じ ID |
| `task_id` | 操作対象 Task |
| `revision` | 受付を記録した後の Task revision |
| `operation.operation_id` | 受付後の状態・結果・ログ取得や取消に使う Rust 発行 ID |
| `operation.kind` | 実行する tool 名 |
| `operation.state` | 受付直後は `accepted`。これは処理成功ではない |
| `attempt_id` | Attempt を作る operation だけに含む。`attempt.run` は受付時に Attempt ID を発行する |

この受付 response には処理結果を含めない。最終結果は `operation.get` の `result`（成功時の tool 固有結果）または `error`（失敗時の共通 error）で読む。`result` と `error` は同時に設定しない。受付後に接続が切れた場合、同じ request ID で再送すれば Rust は保存済み受付 response を返し、二重実行しない。

`operation.list_logs` の cursor も不透明で、operation ID・stream・limit に束縛される。`next_cursor: null` はその stream の現在取得可能な末尾を示す。新しい log が後から追加される可能性があるため、完了前の末尾到達は operation の完了を意味しない。cursor 不一致・期限切れは `invalid_cursor` とし、cursor なしで読み直す。

受付成功は処理結果の成功を意味しない。operation state は accepted、running、completed、failed、cancelling、cancelled、recovery_required のいずれか。読み取り tool は revision を変えない。Task に属する操作受付や結果記録は revision を進める。

すべての response は schema version を返す。任意 field の追加は後方互換とする。必須 field、enum、既存 field の意味を変える場合は新しい schema version を使い、旧 Ledger の状態を新しい意味へ読み替えない。

## エラーと復旧

業務上の拒否は MCP transport error ではなく、次の形の response とする。該当しない field は null とする。

~~~json
{
  "schema_version": "v1",
  "request_id": "同じ request ID",
  "error": {
    "code": "stale_revision",
    "message": "Task revision が更新されています",
    "retryable": true,
    "current_task_revision": 13,
    "operation_id": null,
    "details_ref": null
  }
}
~~~

error code は invalid_request、unsupported_schema_version、task_not_found、stale_revision、idempotency_conflict、busy、unknown_provider、unknown_model、policy_denied、budget_exhausted、workspace_boundary_violation、artifact_not_found、artifact_task_mismatch、evidence_artifact_mismatch、invalid_state_transition、operation_not_found、not_cancellable、timeout、cancelled、interrupted、recovery_required、invalid_cursor、forbidden、internal_error を使う。

retryable は同じ request を再送してよい意味ではない。timeout や中断で終了を確認できない場合、Rust は成功・失敗・未実行と推測せず recovery_required を記録する。取消も停止を確認するまでは完了として返さない。失敗後に retry や修正を行うかは監督Codexが決める。

## 情報の公開範囲

context には、Task 要求・revision、Provider / Model の可用性、quota / reset、budget と usage、過去実績、直近 operation、Artifact、Validation、review / decision、PR / CI の要約を必要な範囲で返す。長い履歴は section ごとに cursor で取得する。観測不能・未集計の値は unknown または理由を持つ null とし、0 や成功として扱わない。旧履歴には state semantics version を付け、旧 Attempt の Succeeded と Provider 呼び出し成功を混同しない。

context と log に secret、認証 token、環境変数値、Provider の認証情報を返さない。既知 secret は永続化時と返却時に redact する。安全に redact できない log は本文を返さず、権限付き参照だけを返す。全文 diff や raw log は既定 response に含めず、要求範囲を限定して取得する。

## 対象外

この契約は仕様であり、MCP server や Operation Service を実装するものではない。MCP transport、SQLite schema、Provider 固有 CLI、AI review Provider、workspace の再利用・cleanup、並列 workflow、自動 merge は後続の実装 Issue で扱う。
