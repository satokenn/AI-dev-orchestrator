# モデル選定の入力・出力仕様

この文書では、監督Codexが実装・レビューの担当モデルを選ぶために使う情報と、返す選定結果を定めます。実行先ごとのCLI・API形式、利用状況の取得方法、選定処理の実装、モデル性能の評価方法は定めません。

利用可能なモデルの一覧やモデルごとの料金を収集し、どのモデルが安価かを判定する本番機能は、まだ用意されていません。この文書は、特定のモデルが利用可能または安価であることを保証しません。例に使う実行先・モデル名・利用量・金額は、形式を説明するための架空の値です。

> [!IMPORTANT]
> この文書は後続実装のための仕様である。この文書を追加するだけでは実行時の動作は変わらない。

## 読者と前提

[開発支援システムの全体像](architecture.md)を前提とする。この文書では、**監督Codexが何を根拠に担当モデルを選び、Rust側がその選択をどう検査するか**を確認できます。型の実装を知らなくても、以下の要約と「入力項目の意味」「Rustが行う検証」で判断できる構成とする。

## 最初に読む要約

- **Rust側が集めるもの:** 作業の要求、実行先とモデルの候補、利用可否、使用量や制約、保存済みの実行履歴。各情報には出どころと確認時刻を付ける。
- **監督Codexが返すもの:** 今回必要な担当ごとに、選んだ実行先・モデルと、その選定理由。観測値や過去の結果を書き換えて返すものではない。
- **Rust側が実行前に確かめるもの:** 候補に存在する組合せか、必要な能力があるか、利用可能か、設定された制約に反しないか。選定理由がもっともらしくても、検査を省略しない。

たとえば「実装担当を選ぶ」という要求に、Rust側が候補A・Bと既知の利用状況を渡し、監督Codexが「Aを選ぶ、理由は実装に必要な能力があるから」と返す。Rust側はAがまだ利用可能かを再確認して実行へ渡す。**候補情報を取れない場合に、Codexがモデル一覧や利用量を想像して補うことはない。**

Rustから監督Codexへ渡す選定用JSONの構造と各項目の意味は、この文書の対象です。実行先のCLIやAPIからその情報をどう取得するか、どのMCP操作や通信処理で選定用JSONを受け渡すかは、この文書では定めません。最も安いモデルを探す機能の実装も対象外です。

| 読む目的 | 該当箇所 |
| --- | --- |
| 選定に渡す情報と返る情報を知る | 入出力の構造、入力項目の意味 |
| 選んだモデルを実行してよい条件を知る | 仕様の基本規則、Rustが行う検証 |
| 正確な値や型を実装する | 詳細なJSON例、末尾のRust型案 |

## 目的と適用範囲

モデル選定では、Rustが収集・記録した事実に基づき、監督Codexが担当モデルを判断します。

1. Rust側が、作業の要求、候補、利用可否・制限・使用量・過去実績・実行履歴を、選定時点の記録（snapshot）として固定します。
2. 監督Codexがその記録を比較し、必要な担当ごとに実行先とモデル、選定理由を返します。
3. Rust側が選定結果を元の記録と最新の事実に照合し、実行してよいものだけを実行へ渡します。

```text
Rustが事実を収集 → JSON入力 → Codexが候補を選定 → JSON出力 → Rustが検証 → 実行
```

この仕様は選定用の入出力を定める。Providerごとの呼出し方、観測値を取得する処理、モデルの性能評価方法は定義しない。Rust型・JSON形式・接続処理・DB形式の実装変更も、この文書だけでは行わない。

ここでいうPlannerはモデルを選ぶ役割を指す。目標構成では監督Codexが判断する。現行CLI内部の`CodexPlanner`をMCP経路で自動的に起動するという意味ではない。

## 入出力の構造

### JSON入力の全体像

これはRustが作成して監督Codexへ渡すデータです。監督Codexが自分で候補一覧や利用量を書いてRustへ送る入力ではありません。具体的な形は[入力のJSON例](#rustから監督codexへ渡すjson)で確認できます。

| 項目 | 内容 |
| --- | --- |
| `schema_version` | 入力形式のバージョン |
| `request_id` | 入力と出力を対応付ける選定要求ID |
| `captured_at_ms` | Rustが入力を作成した時刻 |
| `task` | 目的、制約、Issue、選定する担当 |
| `providers` | 選択可能なProvider / Modelと、利用状況・実績 |
| `current_attempts` | 同じTaskですでに行った実行と、その結果 |

この6項目が、Rustから監督Codexへ渡すJSONオブジェクトの最上位の項目である。すべて必要であり、外側に`input`という項目は付けない。型と入れ子の項目は[送信するJSONの構造](#rustから渡すjsonの項目と入れ子構造)で定義する。以下の図は、そのうち候補と履歴の関係を示す概略図である。

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

これは監督Codexが選定後にRustへ返すデータです。入力に対応する[出力のJSON例](#監督codexからrustへ返すjson)も分けて掲載します。

出力の最上位には、整数`1`の`schema_version`、入力と同じ文字列の`request_id`、担当ごとの選定結果を並べた`assignments`配列を必ず含める。各選定結果には`role`、`target`、`reason`を含める。次表の`target.provider`と`target.model`は、`target`オブジェクト内の項目を表す。

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

`unknown`を`0`や推定値へ置き換えず、取得できない理由と確認時刻を保持します。

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

`task`には、目的、利用者の制約、現在の状態、今回選ぶ担当を含めます。GitHub Issueを起点にする場合は、選定時点のタイトル・本文・ラベル・コメントを順序を保って含める。コメントに要求変更や設計判断が含まれる場合があるためです。

Issueを使わず依頼内容を直接登録した作業でも、目的と制約を渡します。MCPの作業登録では、この出所を`source: "manual"`と呼びます。人間が実装するという意味ではありません。[作業登録時の出所と保存内容](mcp-operation-contract-reference.md#taskrequest--tasksnapshot)を参照してください。

Rust側は、入力を固定する前に秘密情報と認証情報を取り除き、伏せ字化していない実行先のログを除外します。

`requested_roles`は「今回どの担当を選ぶか」を示します。出力では、各担当に一つずつ割り当てます。実装担当とレビュー担当を同時に要求できるが、必ず異なるモデルを選ぶという規則ではない。必要な能力と実行許可の規則をRust側が提示し、監督Codexがその範囲で判断する。

### 実行先とモデルの候補

`ProviderSnapshot`には、認証、利用契約、APIアカウントなど、実行先全体に関わる事実を記録します。`ModelSnapshot`には、その実行先の個別モデル、または候補として指定された既定モデルを記録します。実行対象は、実行先とモデルの組で特定します。

候補として扱うのは、実行先とモデルの両方が`available`で、担当に必要な能力をすべて満たす組だけです。どちらかが`unavailable`（利用不可）や`unknown`（確認不能）なら実行しない。監督Codexが理由を付けても、この拒否を解除できない。

モデル一覧を取得できない実行先は、空の`models`を渡して「候補なし」と見せかけない。既定モデルの実行を許す場合は、`ModelChoice::ProviderDefault`に対応する候補を一件渡す。

### 利用制限と使用量

`limits`は、要求回数・トークン数・クレジット・同時実行数などの制限を表す。名前、適用範囲、期間、上限、使用量、残量、リセット時刻を別々に保持する。実行先全体の制限は`ProviderSnapshot.limits`、モデル固有の制限は`ModelSnapshot.limits`に置く。同じ事実を両方へ複製しない。

制約の適用方法（`ConstraintEnforcement`）は、Rust側の実行設定（`ExecutionPolicy`）で決めます。`hard`は実行を許可する条件、`advisory`は比較の参考情報である。監督Codexが変更するものではない。

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

`performance`は、記録用DB（Ledger）から集計した期間付きの実績です。実行先・モデルと、必要なら担当ごとに集計する。成功率だけで母数を隠さず、モデル実行・終了状態・機械検証・レビュー・再試行の件数を渡す。

集計値は`computed`、取得元は`execution_ledger`とする。レビュー結果がまだ保存されていない場合は、その集計値を`unknown`とする。保存済みの`ReviewOutcome`は、`Approved`を`review_approved`、`ChangesRequested`を`review_changes_requested`、`Inconclusive`を`review_inconclusive`へそれぞれ一件加算する。一件を複数分類へ加算せず、どの結果も集計から除外しない。

`current_attempts`は同じTaskに属するすべてのAttemptを、履歴番号（`sequence`）順に渡す。直前の失敗だけでなく、モデルの切替・検証・レビュー・累積の再試行を次の判断に使えるようにする。

伏せ字化していない標準出力・標準エラー出力や機密情報は渡さない。選定に必要な分類と短い要約だけを渡す。過去の実行記録を新しい選定結果で上書きしない。

## Rustから渡すJSONの項目と入れ子構造

ここでは、JSON例やRustの型定義を読まずに、Rustが何をどこに入れて渡すか確認できるよう、各オブジェクトの項目を列挙する。項目名と構造は既存の本文・型案に対応する。以下で「必須」と記した項目は値を含める必要がある。配列の各要素は、対応する表の構造を持つ。

一方、元の型案で`Option`となっている項目は、値がないことを表せても、それをJSONで省略するか`null`にするかが一律には決まっていない。そのような箇所は表で未確定と示す。[未確定の送信形式](#送信形式でまだ確定していないこと)が残るため、この文書を実装可能な完成仕様とは扱わない。

### 最上位の6項目

| 項目 | JSONでの形 | 含める条件と意味 |
| --- | --- | --- |
| `schema_version` | 整数`1` | 必須。選定用の入出力形式のバージョン |
| `request_id` | 文字列 | 必須。Rustが今回の選定依頼に付けるID。監督Codexは出力にも同じ値を入れる |
| `captured_at_ms` | 整数 | 必須。Rustがこの入力を作成したUnix時刻。単位はミリ秒 |
| `task` | 作業オブジェクト | 必須。下記「作業と選定する担当」の構造 |
| `providers` | 実行先オブジェクトの配列 | 必須。下記「実行先とモデル候補」の構造。各要素にその実行先の`models`を含める |
| `current_attempts` | 実行履歴オブジェクトの配列 | 必須。同じTaskの実行を`sequence`順に並べる。各要素は下記「現在の実行履歴」の構造 |

### 作業と選定する担当

`task`に入れる項目は次のとおり。

| 項目 | JSONでの形 | 含める条件と意味 |
| --- | --- | --- |
| `task_id` | 文字列 | 必須。どの作業のモデルを選ぶか |
| `objective` | 文字列 | 必須。作業の目的 |
| `constraints` | 文字列の配列 | 必須。利用者が指定した制約 |
| `state` | 状態を表す値 | 必須。Taskの状態。例は`"active"`。全状態のJSON表記は未確定 |
| `issue` | Issue情報オブジェクト | Issue起点の場合の情報。Issueを使わない場合の省略・`null`の区別は未確定 |
| `requested_roles` | 担当指定オブジェクトの配列 | 必須。今回、どの担当のモデルを選ぶか |

`task.requested_roles`の各要素に入れる項目:

| 項目 | JSONでの形 | 意味 |
| --- | --- | --- |
| `role` | 空でない文字列 | 選定する担当。例は`implementer`、`reviewer` |
| `instruction` | 文字列 | その担当へ依頼する仕事 |
| `required_capabilities` | 文字列の配列（各要素は空文字列不可） | その担当に必要な能力の識別名。例は`workspace_write`、`code_review` |

この3項目は必須。`task.issue`がある場合の項目は、`repository`（リポジトリ名）、`number`（非負整数のIssue番号）、`url`、`title`、`body`、`labels`（文字列の配列）、`comments`（コメントの配列）であり、すべて含める。`number`以外の単独の値は文字列である。各コメントには`url`、`author`、`created_at_ms`、`body`を含める。`created_at_ms`はUnix時刻のミリ秒を表す整数で、他の3項目は文字列である。

### 実行先とモデル候補

`providers`の各要素に入れる項目:

| 項目 | JSONでの形 | 含める条件と意味 |
| --- | --- | --- |
| `provider` | 文字列 | 必須。実行先の識別名 |
| `availability` | 利用可否オブジェクト | 必須。この実行先を使えるか。構造は下記「利用可否と値の取得根拠」 |
| `limits` | 利用制限オブジェクトの配列 | 必須。この実行先全体に適用される制限 |
| `api_usage` | API使用量オブジェクト | API使用量が対象外の場合だけ値なしを許す。例では`null`だが、省略も許すかは未確定 |
| `performance` | 過去実績オブジェクトの配列 | 必須。この実行先全体の過去実績 |
| `models` | モデル候補オブジェクトの配列 | 必須。この実行先で選べる候補 |

各`providers[].models[]`に入れる項目:

| 項目 | JSONでの形 | 含める条件と意味 |
| --- | --- | --- |
| `model` | モデル指定オブジェクト | 必須。下記の2形式のどちらか |
| `availability` | 利用可否オブジェクト | 必須。このモデルを使えるか |
| `limits` | 利用制限オブジェクトの配列 | 必須。このモデルだけに適用される制限 |
| `api_usage` | API使用量オブジェクト | 実行先と同じく、対象外の場合だけ値なしを許す。JSONでの省略可否は未確定 |
| `capabilities` | 文字列の配列（各要素は空文字列不可） | 必須。この候補が対応する能力の識別名 |
| `performance` | 過去実績オブジェクトの配列 | 必須。このモデルの過去実績 |
| `estimated_execution` | 計測項目オブジェクトの配列 | 必須。次の一回の実行についての推定値 |

モデル指定オブジェクトは、名前指定なら`{"kind":"named","model":"モデル名"}`、実行先の既定モデルなら`{"kind":"provider_default"}`とする。候補を選ぶ際の指定に`unknown`は使わない。過去に実際に使ったモデルが不明な場合は、下記の実行履歴で区別する。

### 利用可否と値の取得根拠

`availability`には、`status`、`observed_at_ms`、`source`を必ず含める。`status`は`available`・`unavailable`・`unknown`の文字列であり、後者二つでは空でない文字列の`reason`も同じオブジェクトに含める。`observed_at_ms`は確認時刻を表す整数で、単位はUnix時刻のミリ秒である。`status`を別のオブジェクトへ入れ子にしない。

使用量・金額・残量・件数などには、値の取得根拠を含む次のオブジェクトを使う。末尾のRust型案では、これを`Evidence<T>`と呼んでいる。`T`はその場所で入る値の型を表す。

| 場合 | 必ず含める項目 | 意味 |
| --- | --- | --- |
| 値が分かる | `status: "known"`、`value`、`basis`、`assessed_at_ms`、`source` | 値そのものに、実測・設定・計算・推定のどれかを添える |
| 値が分からない | `status: "unknown"`、`reason`、`assessed_at_ms`、`source` | 数値を作らず、取得できない理由を文字列で残す |

`basis`は`measured`・`configured`・`computed`・`estimated`の文字列である。`assessed_at_ms`は取得または評価したUnix時刻のミリ秒を表す整数である。`source`は取得元を表し、次の2項目を必ず持つ。

| 項目 | JSONでの形 | 意味 |
| --- | --- | --- |
| `kind` | `provider_api`・`provider_cli`・`execution_ledger`・`repository_config`の文字列 | API、CLI、記録DB、リポジトリ設定のどれから得たか |
| `reference` | 文字列 | その情報の取得元や計算元を追うための情報 |

金額・トークン数などの`value`は、`amount`（十進数を文字列にした量）と`unit`（単位の文字列）を持つオブジェクトである。件数の`value`は非負整数、時刻の`value`はUnix時刻のミリ秒を表す整数とする。これらは同じ形の数値ではなく、項目に応じた型で渡す。

### 制限・API使用量・過去実績

利用制限の各要素には、次の項目を含める。

| 項目 | JSONでの形 | 含める条件と意味 |
| --- | --- | --- |
| `name` | 文字列 | 必須。要求回数など、制限する項目の名前 |
| `scope` | 文字列 | 必須。アカウントやプロジェクトなど、制限が適用される範囲 |
| `enforcement` | `hard`または`advisory`の文字列 | 必須。実行許可の条件か、比較の参考情報か |
| `window` | 期間オブジェクト | 期間なしも表せるが、省略・`null`の区別は未確定 |
| `limit`、`used`、`remaining` | 量と取得根拠を含むオブジェクト | 必須。それぞれ上限・使用量・残量。分かる場合の`value`は`amount`と`unit`を持つ |
| `resets_at_ms` | 時刻と取得根拠を含むオブジェクト | 必須。リセット時刻。分かる場合の`value`は整数 |

期間オブジェクトには、`starts_at_ms`と`ends_at_ms`を含める。どちらもUnix時刻のミリ秒を表す整数である。

API使用量オブジェクトには、`scope`（適用範囲の文字列）、`window`（期間）、`actual_usage`（実使用量の配列）、`configured_budget`（設定予算の配列）、`remaining_budget`（残予算の配列）をすべて含める。

- 実使用量と次回実行の推定値の各要素は、`name`と`value`を持つ。`value`は上記の取得根拠を含むオブジェクトで、分かる値には`amount`と`unit`を使う。
- 設定予算と残予算の各要素には、さらに`enforcement`を含める。未設定の予算を架空の要素として追加しない。
- 過去実績の各要素には、`window`、`attempts`、`succeeded`、`failed`、`cancelled`、`validation_passed`、`validation_failed`、`review_approved`、`review_changes_requested`、`review_inconclusive`、`retries`をすべて含める。`window`以外は取得根拠を含む件数であり、分かる場合の`value`は非負整数である。担当別の実績なら`role`も持つ。全担当をまとめた場合の`role`の省略・`null`の区別は未確定。

過去実績の各件数が何を数えるかは[過去実績と現在の実行履歴](#過去実績と現在の実行履歴)に従う。

### 現在の実行履歴

`current_attempts`の各要素に入れる項目:

| 項目 | JSONでの形 | 含める条件と意味 |
| --- | --- | --- |
| `attempt_id` | 文字列 | 必須。過去のモデル実行のID |
| `sequence` | 非負整数 | 必須。Task内での実行順。履歴番号の条件は履歴設計に従う |
| `role` | 空でない文字列 | 担当を復元できない旧履歴では省略する |
| `relation` | 履歴関係オブジェクト | 関係の種類と、必要な参照先。値なしの場合のJSON表現は未確定 |
| `target` | 実行先とモデルを持つオブジェクト | 必須。`provider`は文字列、`model`はモデル指定と取得根拠を含むオブジェクト。実際に使ったモデルが不明なら`status: "unknown"`にする |
| `state` | 状態を表す値 | 必須。Attemptの状態。新旧の状態の意味とJSON表記の対応は未確定 |
| `failure` | 失敗情報オブジェクト | `category`（分類の文字列）、`retryable`（真偽値）、`summary`（短い説明の文字列）。失敗情報がない場合の省略・`null`の区別は未確定 |
| `validation` | 検証要約オブジェクトの配列 | 必須。各要素は`name`（文字列）、`passed`（真偽値）、`summary`（文字列）。結果不明をこの真偽値で表す方法は未確定 |
| `review` | レビュー要約オブジェクト | `outcome`（レビューの結論）と`summary`（文字列）を持つ。旧履歴で復元できない場合は省略する |
| `usage` | 利用量の一覧と取得根拠を含むオブジェクト | 必須。分かる場合の`value`は、`name`と`value`を持つ計測項目の配列 |
| `started_at_ms`、`finished_at_ms` | 整数 | 開始・終了のUnix時刻をミリ秒で表す。時刻がない場合の省略・`null`の区別は未確定 |

`review.outcome`は承認・修正要求・判断不能を区別する。Rust型案の`Approved`・`ChangesRequested`・`Inconclusive`をJSONの文字列へどう対応させるかは、まだ明記されていない。

履歴関係オブジェクトは`kind`で種類を表す。`initial`と`legacy_unspecified`は種類だけ、`retry_of`・`escalation_of`・`review_of`は参照先の`attempt_id`、`rework_from`は`review_attempt_id`、`specified_input`は作成元の`attempt_id`を持つ。`specified_input`で参照先がない場合は`attempt_id: null`とする。この分類と参照条件は[履歴設計](implementation-review-model.md#守る規則)に従い、選定入力を作る際に再分類しない。

### 送信形式でまだ確定していないこと

| 不足している規則 | 現在の記述だけでは決められないこと |
| --- | --- |
| 値なし項目の表現 | `task.issue`、各`api_usage`、制限の`window`、実績の`role`、履歴の`relation`・`failure`・開始終了時刻などを、省略するか`null`にするか。`role`・`review`が復元不能な旧履歴で省略する規則と、`specified_input.attempt_id: null`は既に記載済み |
| 状態とレビュー結論の文字列表現 | Task・Attemptの全状態とReviewOutcomeをJSONでどう表し、新旧履歴の意味の違いをどう伝えるか |
| 結果不明の検証要約 | `ValidationSummary.passed`は真偽値しか持たないが、結果不明を失敗へ変換してはならない。新しいValidation結果との対応 |

これらは例の見やすさとは別の、送信形式の仕様不足である。Rust型案や例から暗黙に補って実装せず、仕様を確定してから統合実装へ進む。

## 入力と出力の詳細なJSON例

以下は、Rustから監督Codexへ渡す入力と、監督CodexからRustへ返す出力を、実際に受け渡すJSONオブジェクトごとに分けた例です。外側に`input`や`output`という項目を付けて一つのJSONにまとめる形式ではありません。

例には、同じ実行先でモデル名を指定する候補と既定モデルを使う候補、実測値・推定値・不明な値、実装担当とレビュー担当の割当を含めます。実行先・モデル名・利用量・金額はすべて架空です。実在するモデルの利用可否や価格は示しません。

### Rustから監督Codexへ渡すJSON

Rustは、作業の目的と制約、今回選ぶ担当、候補ごとの利用可否・使用量・制約・実績、現在の実行履歴を一つの入力にまとめます。監督Codexは、その情報から担当ごとの実行先とモデルを選びます。

この例では、実装に`workspace_write`、レビューに`code_review`という能力を求めています。名前を指定する候補は両方に対応し、既定モデルの候補はレビューに対応しています。残予算の値は不明ですが、この例の予算は比較材料として扱う`advisory`です。実行を必ず制限する`hard`の残量が不明な場合は、[上限の確認規則](#強制する上限の残量が不明な場合)に従います。

```json
{
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
}
```

### 監督CodexからRustへ返すJSON

監督Codexは、同じ`request_id`に対して、要求された担当ごとの実行先・モデル・選定理由を返します。上の入力にある利用量や利用可否を、選定結果の中で書き換えて返しません。

この例では、実装担当には名前を指定したモデル、レビュー担当には実行先の既定モデルを選んでいます。Rustはこの結果を入力と現在の利用条件に照合してから、実行を許可するか確認します。

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
```

選定結果にレビュー担当（`reviewer`）が含まれても、レビューが必須になるわけではありません。監督Codexが必要な担当だけを要求します。履歴の関係はServiceが入力と確認済みの記録から保存します。選定理由（`reason`）はモデルを選んだ理由であり、再試行やレビュー指摘への修正を行った理由として使いません。

## Rustが行う検証

選定結果を実行へ渡す前に、Rust側は次の項目を確認します。

1. `schema_version`を解釈でき、`request_id`が入力と一致する。
2. 要求した各担当が出力に一回ずつ現れ、未知・重複・欠落の担当がない。
3. 選定理由（`reason`）が空白だけではない。
4. 実行先とモデルの組が、選定に渡した候補に存在する。
5. 実行先とモデルの利用可否が、両方とも`available`である。
6. モデルが担当に必要な能力をすべて持つ。
7. Rust側が設定した予算・利用枠・再試行上限・安全条件に反しない。
8. 実行直前に利用可否と強制する制約を再取得し、選定後に変化した事実にも反しない。

1〜6では返された選定結果を元の入力と照合し、7〜8では現在の事実に基づいて実行を許可できるか確認します。両方を通過した値（`ValidatedPlannerDecision`相当）だけを実行へ渡す。

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

選定結果は「この担当を選ぶ」という判断です。Operation Serviceは、実行の作成・状態変更・実行を検証して適用します。選定だけでTaskを完了にしない。実装とレビューの記録の意味は#73、モデル指定と実際に使用したモデルの記録は#59、選定用の情報収集は#60、過去の性能実績の集計は#67が担当する。

## 後続実装への適用順

1. Issue #58 で担当とレビュー結果をドメインと記録用DBのどこへ保持するか決める。
2. Issue #59 で `ModelChoice` と実際に使用した Model を Provider / Attempt / Ledger へ接続する。
3. Issue #60 で出どころと確認時刻を持つ観測値、使用量、実行履歴の要約を収集する。性能実績の集計は Issue #67 が担当する。
4. Issue #61 で既存 `PlannerRequest` / `PlannerDecision` をこの入力／出力へ拡張し、Rust 側検証を実装する。

各Issueは共通の項目を実行先固有の形式へ置き換えず、取得できない項目は`unknown`として
保持する。仕様の互換性を壊す変更は `schema_version` を上げ、入力と出力を同時に更新する。

## 実装担当者向けのRust型案

ここからは、上で説明した情報とRust型・JSON項目の対応を示します。設計の目的や分担を確認する場合は、上の説明を先に読む。型名・値の名前は実装上の名前として保持する。

<details>
<summary>Rust型定義の詳細を表示する</summary>

以下は、v1の必須項目と省略可能な項目を示す型案です。実装では`serde`を使い、分類を識別する値付きの列挙型（tagged enum）にする。JSONの項目名は`snake_case`とする。時刻は既存DBと同じUnix時刻のミリ秒、量は丸めや浮動小数点誤差を避けるため十進数の文字列で表す。

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
