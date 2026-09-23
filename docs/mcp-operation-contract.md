# 監督Codex向け MCP 操作契約

## 読者と目的

主な読者は、[Issue #45](https://github.com/satokenn/AI-dev-orchestrator/issues/45) で stdio MCP Gateway を実装する人である。監督Codexから tool を呼び出すクライアント側の実装者も対象とする。

この文書の目的は、監督Codexと Rust Operation Service の間で、どの操作を要求でき、どの事実が返るかを決めることにある。Issue #45 はこの契約を MCP tool として実装する。Gateway は要求を検査して Operation Service に渡し、結果を返す。Gateway 自身は仕事の進め方や Provider / Model を決めない。

ここで定める粒度は、tool の入力・出力 schema、状態や error の意味、外部副作用を許す条件までである。Rust の型、SQLite の表、MCP server の起動方法など実装内部の構造は定めない。下記の field 定義と例が v1 の仕様であり、#45 の schema validation とテストはこれに一致させる。

Task、Attempt、Artifact、Validation、review verdict、CodexDecision の意味は[ドメインモデル](domain-model.md)を正本とする。特に Attempt の Succeeded は Provider 呼び出しの正常終了だけを表す。Validation 成功、review の承認、監督Codexの受入、公開、Task 完了は別の事実である。

## まず全体の流れ

監督Codexは context にある観測結果を読んで、次の操作を判断する。Rust は依頼された操作だけを実行・記録し、その結果を返す。

~~~text
Taskを作る → 現在の状況を読む → Provider実行を依頼する → 結果を読む
                                         ↓
                              検証・review・修正を判断
                                         ↓
                       受入 → 公開 → CI確認 → Task完了を要求
~~~

長くかかる Provider、Validation、公開、CI 待機は非同期 operation とする。Rust は受付を記録して operation ID を返す。監督Codexは operation の結果を取得し、次の操作を明示する。retry、review、公開、Task 完了を Rust が自動で選ぶことはない。

| 起きたこと | Rust が返す事実 | 監督Codexが決めること |
| --- | --- | --- |
| Provider 呼び出しが正常終了 | Attempt の終了状態と出力 Artifact | 検証や review を行うか |
| Validation が失敗 | 対象 Artifact の失敗結果 | 修正、再実行、調査のどれを行うか |
| reviewer が修正を要求 | reviewer Attempt と ReviewVerdict | 修正するか、追加 review するか |
| CI が失敗または観測不能 | 対象 PR / SHA の CI 状態 | 修正や CI 再確認を行うか |
| timeout または中断 | operation の終了理由、または復旧が必要な状態 | 状態を再取得して次の操作を決める |

## 誰が何を決めるか

| 担当 | 責務 |
| --- | --- |
| 監督Codex | 要求を解釈する。Provider / Model、retry、review、成果物の採否、Task 完了を判断する |
| Rust Operation Service | ID・revision・policy・workspace を検査する。Provider / Validator / GitHub 操作、状態遷移、予算、timeout、永続化を担う |
| MCP Gateway | tool request / response を schema 検証し、Operation Service へ中継する |
| reviewer Provider | 指定 Artifact を review し、review の結果を返す |

監督Codexは Rust が記録した usage、Provider 終了、Validation、CI の観測結果を書き換えない。Rust は次の Provider / Model や成果物の採否を自発的に決めない。Rust 内部の CodexPlanner はこの MCP 主経路に含めない。worker として起動する Codex CLI は、監督Codexとは別の Provider 実行である。

## Tool 一覧

この表は tool を探すための索引であり、schema の代わりではない。正確な入力・出力は「各 tool の入出力」を参照する。

| Tool | 目的 | 実行方式 |
| --- | --- | --- |
| task.create | Issue または手入力から Task を作る | 同期 |
| task.get_context | 次の判断に必要な情報を読む | 同期・読取 |
| task.cancel | Task と未終了 operation の停止を要求する | 非同期受付 |
| attempt.run | 指定 Provider / Model を一度実行する | 非同期受付 |
| operation.get | 非同期処理の状態・結果を読む | 同期・読取 |
| operation.list_logs | 指定範囲のログを読む | 同期・読取 |
| operation.cancel | 一つの operation の停止を要求する | 非同期受付 |
| validation.run | 指定 Artifact を機械検証する | 非同期受付 |
| decision.record | 監督Codexによる Artifact の判断を記録する | 同期 |
| publication.publish | 指定 Artifact を Pull Request として公開する | 非同期受付 |
| ci.get / ci.wait | CI 状態を読む、または期限まで待つ | 読取 / 非同期受付 |
| task.finish | 証拠を照合して Task 完了を要求する | 同期 |

## 各 tool の入出力

JSON の object と array は、それぞれ {...} と [...] で表す。Request 欄に「必須」とした field はすべて指定する。型や許可値に合わない request、定義されていない request field は invalid_request で拒否する。Response 欄は必須の返却 field を示し、クライアントは未知の追加 response field を無視する。

### Task を作る、状況を読む

| Tool | Request | Response |
| --- | --- | --- |
| task.create | 必須: schema_version=v1、request_id (string)、source (issue / manual)、title (string)、description (string)、constraints (string array)。任意: issue object (url、number、title、body) | task_id、revision、state=pending、保存した要求と Issue 情報 |
| task.get_context | 必須: schema_version、task_id、sections (string array)。任意: page_size (1–100、既定20)、cursors (section 名から cursor への map) | task (ID、revision、state、要求)、要求された section ごとの items と next_cursor、observed_at |

sections は task、providers、usage、attempts、artifacts、validations、reviews、decisions、publication、ci から選ぶ。履歴 item は ID、種類、状態、作成時刻、対象 ID、短い要約を持つ。詳細ログは含めず、必要なら operation.list_logs で読む。読み取り tool は request ID や revision を要求せず、Task revision を増やさない。

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
