# Operation ServiceとMCPの統合設計

更新日: 2026-10-11（日本時間、Asia/Tokyo）。**この変更は設計・仕様書の改訂です。製品コード、DB移行、実行時の動作は変わりません。**

## この文書を読む前に

**PRのレビューは[OpenSpecの変更目的](../openspec/changes/service-integration-design/proposal.md)と[設計の説明](../openspec/changes/service-integration-design/design.md)から始めてください。** 本書は、そこで説明した流れを実装へ接続する際の詳細資料です。処理の順序・保存・取消・記録の照合条件を調べるときに使います。

まず[開発支援システムの全体像](architecture.md)を読んでください。本書は、その構成をRustの共通処理とMCPの受け口につなぐ詳細設計です。監督Codexから操作を依頼する流れは、[MCP操作の概要](mcp-operation-contract.md)に従います。

本書でいう「サーバーの起動側」は、Rustサーバーを組み立てて起動するプログラムです。「注入」は処理や設定をServiceへ引数として渡すこと、「解決」は識別名に対応する登録済みの処理を見つけることを意味します。

## 目的と要約

この設計の目的は、監督Codexが一つのTaskについて、モデル実行、検証、判断、Draft PR公開、CI観測を依頼できるMVPを作ることです。各結果を同じ成果物へ結び付けて保存し、監督Codexが履歴を見て次の操作を選べるようにします。そのため、既存の13操作を一つのServiceとSQLiteデータベースへ接続します。

本書では、実装に入る前に、起動から終了まで必要な依存関係、共通の状態、証拠の所属、失敗時の扱いを決めます。操作の正確な入力・出力は[MCP操作仕様](mcp-operation-contract-reference.md)、状態と記録の所属は[ドメインモデル](domain-model.md)を正本とします。実装状況は対応IssueとPRで管理し、本書の設計とは分けて確認します。

本書は、サーバーの起動、設定の受渡し、終了、状態遷移、取消の競合に関する方針を定めます。履歴分類の`SpecifiedInput`は、関連する3仕様書にも反映します。以下に全操作の設計を記載しますが、全てが実装済みという意味ではありません。

対象外は、自動モデル選択、自動修正、自動review、自動merge（変更の統合）、常時有効な予算制御、ログ本文の新規公開です。既存仕様で任意とされる機能を必須にはしません。

## 用語

本書では、Taskを一つの作業依頼、AttemptをそのTaskでモデルを一回呼び出した記録、Artifactを保存した成果物として扱います。ProviderはAttemptを実行する外部処理、Validationは成果物に対する機械検査、CI Observationは公開先で確認した継続的インテグレーションの結果です。

MCP Hostは監督Codexが動作するアプリケーションです。MCP adapterは、そのアプリケーションとの通信を受けて共通Serviceへ渡すRust側の処理です。Rustサーバーの「起動側」は、これらを組み立ててプロセスを開始する側を指し、MCP Hostとは異なります。

## このPRで確認する設計判断

このPRは、構成が要求に合っているかを確認する利用者と、後続の実装担当者向けです。まず誰が何を担当するか、何が未実装かを次の表で確認できます。状態や保存形式の細則は、必要な節を参照してください。

| 確認すること | 本書の結論 | 状況 |
| --- | --- | --- |
| 誰が担当モデルや次の操作を選ぶか | 監督Codexが選ぶ。Rust側は指定された操作を検査・実行する | 既存仕様の分担を維持 |
| 操作ごとにDBや安全検査を別実装にするか | 受付・保存・取得・安全検査を共通Serviceへ集約する | 接続方針を決定。全操作の統合実装は未完了 |
| 設定や外部接続を誰が用意するか | Rustサーバーを起動するプログラムが用意し、Serviceへ渡す | 渡す境界を決定。秘密情報検査・モデル確認の本番処理は未接続 |
| 再送や同時の取消で結果を重ねないか | DBの一括保存で受付と状態を確定し、結果不明なら再実行しない | 内部の保存・競合処理方針を決定 |
| レビューなしの修正や調査を何として記録するか | 入力に基づく履歴分類`SpecifiedInput`を追加し、既存の分類と旧履歴を保持する | 関連仕様を同じPRで改訂。実装・DB移行は後続 |
| このPRをマージすれば使えるか | 文書のみの変更なので、実行時の動作は変わらない | 統合実装と一連の動作確認は後続 |

**まだ揃っていないもの:** 本番用のモデル情報取得処理、秘密情報検査処理の接続、6種類すべての長時間処理と共通取得機能の接続、全種類の根拠記録の照合、MCPサーバーの組立、一連の動作確認です。設定を渡す境界が決まったことを、これらの実装完了と取り違えないでください。

## 根拠と適用版

本書は、[アーキテクチャ](architecture.md)、[ドメインモデル](domain-model.md)、[履歴設計](implementation-review-model.md)、[モデル選定仕様](model-selection-spec.md)で定めた責務を、MCPと共通Serviceへどうつなぐかを説明します。

MCPの入力・出力は[MCP操作仕様](mcp-operation-contract-reference.md)を正本とします。本PRのmain branchにはまだv1があります。後続の統合実装では、承認済みの[ModelChoice改訂PR #84](https://github.com/satokenn/AI-dev-orchestrator/pull/84)と[CI改訂PR #88](https://github.com/satokenn/AI-dev-orchestrator/pull/88)を適用したv2を対象にします。本PRだけでv2が実装済み、またはMCPから利用可能になったとは扱いません。

[参考MCP実装PR #109](https://github.com/satokenn/AI-dev-orchestrator/pull/109)のworker/stdio（バックグラウンド処理と標準入出力）構造は、後続実装で再利用する候補です。参考実装を正式仕様や統合済みの動作とは扱いません。既存Rust APIへの言及は後続Serviceの接続方法を示すもので、main branchに全APIが存在する保証ではありません。

処理段階ごとのrevision（Task更新番号）や終了順序は、本書で選ぶ実装方針です。ソースコード照合の基準は、参考PR #109のソースコードcommit `578169249c53035ca7b089f4378c59fb9a37ec52`と、未公開の共通Serviceローカルcommit `933eb601132eeb7effbd402968a2c5c4c396c513`です。後者のcommitはDB transactionの確定を指す語ではなく、Git上のソースコード履歴です。また、このソースコードがmain branchや本PRに含まれることを意味しません。

## 利用の流れと責務

1. 監督CodexがTaskを作り、Provider、ModelChoice、担当、入力、指示を指定してAttemptを依頼します。
2. ServiceがTaskとの所属、revision、冪等性、安全条件を確認して受付を保存します。受付をDB transactionで確定した後、worker（バックグラウンド処理）がProviderを呼び出します。
3. Provider呼び出しの成功、Artifactの保存、Validation、review結果、監督判断は、それぞれ独立した事実として保存します。
4. 監督Codexが必要な検証やreviewを選び、その結果を対象Artifactに結び付けて判断を記録します。
5. Draft PRを公開するとき、Serviceは保存済みArtifact、採否、検証結果、秘密情報検査を照合します。CIは公開先と対象SHAが一致することを確認して観測します。
6. 監督Codexが証拠を指定してTask完了を依頼します。Serviceは、証拠が同じTaskとArtifactに属すること、および未終了処理がないことを確認します。

| 要素 | 担当すること |
| --- | --- |
| 監督Codex | 要求を解釈し、モデルを選び、再試行やreviewを行うか決め、成果物を採否し、完了を依頼する |
| MCP adapter（MCP接続処理） | JSON-RPCと正式入力を検査する。入力JSONから依頼者を自己申告させず、接続側で確定した依頼者IDをServiceへ渡す。Serviceの結果をMCP形式へ変換する |
| Operation Service（共通操作処理） | 操作受付、revision確認、所属・安全性検査、状態遷移、外部処理の呼出し、保存、共通取得、復旧を行う |
| SQLite Ledger（記録DB） | Task、実行、Artifact、証拠、受付記録を一つのDB内で原子的に保存する |
| Provider / Validator / GitHub・CI adapter | 指定された外部処理を実行し、観測した事実と停止結果を返す |
| worker管理 | 受付済みoperationを開始し、二重起動を防ぎ、停止を通知して終了を待つ。業務上の状態を独自に決めない |

## 起動・設定・終了

この節では、**受付を始める前に用意するものと、サーバー終了時に待つ処理**を決めます。実行中のモデル処理が使うProviderなどを先に破棄すると、結果を確認・保存できません。また、別プロセスが同じDBを使っている間はDB移行を始めないようにします。


### 起動から全処理の終了まで設定を保持する

Rustサーバーを組み立てる箇所が、Ledger、Workspace、ProviderResolver、Scanner、ModelCatalog、Validator設定、Publication gateway、CI providerを所有します。Serviceは、これらの依存を借りて動作します。workerが使うServiceと依存は、すべてのworkerが終了するまで破棄しません。

参考MCP実装の`&'static Service`（プログラム全体と同じ期間だけ生存する参照）や、テスト用`Box::leak`（メモリを意図的に解放しない方法）は、製品実装の条件にしません。起動側が依存とServiceを所有し、workerの終了を待つ構造にします。

具体案は、handler（要求を受ける処理）の借用期間を一般化し、Rustの`thread::scope`の範囲内でtransport（通信処理）とscoped worker（範囲付きworker）を動かすことです。通常のthread spawnで返る`JoinHandle`を個別に保持する方法は、scopedな管理へ置き換えます。Arc（共有所有の参照型）に変えるだけでは、借用した依存の生存期間問題は解消しません。

ソースコード上の所有関係は確認済みですが、この案のコンパイルや実行時確認はまだ行っていません。

サーバーは次の順で起動します。

1. 既存の設定形式を読み、依存を組み立てる。MCP専用の新しいValidator設定形式は作りません。
2. 正規のLedgerに対する排他を取得します。
3. 排他を保持したまま、既知のDB schema（データ構造）を検査し、必要な移行を行います。未知のschemaは変更せず拒否します。
4. 同じ排他をServiceへ渡し、未完了operationとpending Artifact（保存途中の成果物）を復旧します。
5. 依存をServiceへ渡し、MCP受付とworker管理を開始します。

**現行との違い:** 現在の共通コードでは、Ledgerを開いてDB移行を終えた後にServiceが排他を取得します。上記の順序なら、別プロセスが先にDB移行を行う隙間を閉じられます。

ソース監査では、既存APIを次の順に組み合わせれば実現できることを確認しました。`LedgerRunLock::acquire(path)`で排他を取り、`SqliteExecutionLedger::open(path)`でDBを開き、`OperationService::new_with_run_lock(..., Some(lock))`で同じ排他をServiceへ渡します。DBの親directory（格納場所）は既存CLI helperと同じ条件で先に用意します。

既存の排他処理とService constructor（生成関数）を共有し、排他取得や起動時復旧を二重に行いません。これは既存APIの組合せで実現する案です。正式仕様がDB移行の順序まで既に定めていた、という意味ではありません。

メモリ内Ledgerには、永続DB向けの自動startup recovery（起動時復旧）を適用しません。稼働中workerに起動時復旧を再実行する公開操作も追加しません。

### 設定と不足時の扱い

| 依存 | 起動側が用意するものと、不足時の扱い |
| --- | --- |
| ProviderResolver（実行先の解決処理） | 監督Codexが`attempt.run`で指定した実行先の識別名に対応する、登録済み実行処理を見つけます。MCP adapterは別の実行先を選び直しません。 |
| ModelCatalog（モデル情報） | 起動側がモデル名を確認する処理を渡します。`named`（モデル名指定）では、確認処理が未設定または指定名が不明なら起動前に拒否します。`provider_default`（実行先の既定モデル）は、確認処理が未設定という理由だけでは拒否しません。他の正式な入力条件は引き続き適用します。 |
| SecretScanner（秘密情報検査） | 起動側が秘密情報を検査・伏せ字化する処理をServiceへ渡します。未設定なら公開を拒否し、既存の拒否条件を維持します。 |
| Validator（機械検査） | #50の既存設定とWorkspace境界を再利用し、正式MCPのchecks入力をServiceが解決します。参考実装のValidationPolicyと共通側の呼出しごとのValidatorを一本化します。未接続を架空の必須入力項目で補いません。 |
| Publication gateway / CI provider（公開・CI接続） | 起動側がGitHub公開とCI取得の処理をServiceへ渡します。外部サービスのエラー本文は秘密情報を含む可能性があるため、そのまま返しません。定義済みのエラーと記録参照へ変換します。 |
| Budget Policy（予算規則） | 明示的に有効にしたときだけ適用します。既承認のTask実行回数上限から始めます。保証できない厳密な上限は、Provider起動前に拒否します。 |

### 起動側が渡すものと、まだ提供されていないもの

Rustサーバーを組み立てるプログラムが、以下の処理と設定を用意してServiceへ渡します。操作依頼を送る監督Codexは、許可設定を変更できません。MCPの操作入力を設定の取得元にはしません。また、#50のTOMLへ未定義の秘密情報・モデル情報・検査セット欄を追加しません。

この節は、設定を提供する側と、それを使って検査する側の分担を決めます。秘密情報やモデル情報を取得する本番処理が用意済みという意味ではありません。特にモデル情報については、取得元、確認時刻、取得元を追跡する参照情報を返す処理を受け取る境界までを定めます。具体的な取得処理は#60/#91の後続作業です。

#### Validatorの許可設定

起動側は、検査セットの識別名（profile ID）と、登録済みの検査処理との対応表をServiceへ渡します。また、要求に直接含まれる検査コマンドと作業場所に対する許可一覧も渡します。

例として、識別名「rust-checks」をfmt、clippy、testの検査セットへ対応させます。これは説明用で、予約名ではありません。Serviceは識別名をファイル名として解釈せず、登録済みの対応表から検査セットを解決します。

登録されたprofileは、実際のchecksと設定ID/hashを解決します。受付時にそのsnapshot（固定した設定内容）を保存します。要求で明示されたchecksは、指定された順序、引数、timeoutを保ち、実行前に同じpolicy（許可規則）で認可します。

未登録profile、不許可command / workspace、policy未設定の場合は、外部検査を始める前に拒否します。入力項目の有無でprofile指定とchecks指定を判別します。明示されたchecksの空配列を「未指定」へ変換しません。空配列に架空のminItems（最小件数）制約を加えず、検査なしをpassed扱いにもしません。結果はUnknown（不明）として記録します。

RepositoryConfigから作成したValidatorを明示的に登録できます。単なる配列を暗黙のprofileとして公開しません。

#### SecretScanner（秘密情報検査）

既承認の注入方式を維持します。起動側が既知の秘密情報の取得元と検査処理を管理し、文字列の伏せ字化と、Gitへ保存する内容・操作データの検査を提供します。

空のsecret集合を安全性の証明として自動採用しません。秘密情報の取得元へアクセスできない、または完全な検査を保証できない場合、Scannerはtyped failure（型付き失敗）を返します。MCP入力にsecret欄を追加しません。

#### ModelCatalog（モデル情報の確認）

起動側は、モデル名を確認する根拠となる情報の取得元、確認時刻、取得元を追跡する参照情報を返す処理をServiceへ渡します。候補一覧やCLIを起動できることだけで、モデルの実行権限があるとは判断しません。

確認処理が未設定なら、モデル名を指定する要求を拒否します。Providerの既定モデルを使う指定はそのまま受け付けられるようにします。具体的な情報取得処理は#60/#91の担当です。起動側は架空のモデル情報を代用しません。

#### MCPサーバーの入口

Serviceの組立とstdio runner（標準入出力で通信する起動処理）を分けます。起動側が用意した入出力、処理、設定を渡してサーバーを起動できる構成にします。

既存CLIは旧Orchestrator経路です。MCP接続作業にCLI全体の置き換えを含めません。

これは新しいユーザー設定形式ではありません。正式仕様に既にあるpolicy検査と承認済みの依存注入を接続する方針です。ScannerやModelCatalogが現状test-only（テスト専用）であることを、製品の全機能が使える状態として報告しません。stdio serverの起動と、必要な依存を注入した各操作の動作確認は、実装後の別の完了条件です。

### Validation要求に含まれる値の秘密情報検査

#### コマンド・引数・検査名

実行するcommand / args、check名、設定snapshot内の実行文字列を、受付記録の保存前に既存SecretScannerの`redact_text`で検査します。まず原文を伏せ字化し、結果をもう一度同じ方法で伏せ字化します。二回目の結果が一回目と同じになる状態を「固定点」と呼びます。さらに、一回目の結果が原文とbyte単位で同じである値だけを安全と判定します。その原文を変更せずに保存し、許可一覧と照合して実行します。

redaction（伏せ字化）で変化する文字列は`policy_denied`で拒否します。伏せ字化後のcommandを実行して、意味を変えることはしません。

#### profile識別子と設定snapshot

`check_profile_id`もopaque（内容を解釈しない）な識別子として検査します。原文のbyte列が安全ならそのまま保持し、秘密情報が見つかった場合は拒否します。

Scannerが未設定、検査に失敗、完全な伏せ字化を保証できない、または二回の伏せ字化で結果が安定しない場合は、未加工のchecks（raw checks）や受付記録を保存する前に、固定の`policy_denied`を返します。profileを解決した結果にも同じ検査を適用します。

保存するsnapshotには、解決済みchecksと設定ID/hashだけを含めます。秘密情報を含む可能性のある設定ファイル全文は複製しません。

#### 結果保存と再送比較

結果のsummary、check metadata、diagnosticは、既存の保存・返却時の安全化を通します。外部サービスのエラー本文をそのまま返しません。冪等性の比較では、同じ安全検査を通過したpayloadの意味を保持して照合します。

### 受付からworker開始まで

受付記録は同じDBのtransaction（一括保存）に保存し、その確定後にworkerを開始します。operation IDごとに同時実行できるworkerは一つだけです。workerが実行権を得たことは、Serviceの状態更新で確定します。プロセス内のmapだけを実行権の根拠にしません。

workerの起動に失敗しても、受付記録は残し、正式エラーに含まれる`operation_id`から状態を取得できるようにします。

外部への副作用が始まる前で、状態が`Accepted`（受付済み）と確認できる場合だけ、同じプロセス内での再送によりworkerを開始できます。再起動時の復旧で状態や外部効果を確定できないoperationは再実行しません。永続キューや自動再試行機能は追加しません。

### EOF・正常終了・異常終了

MCP入力がEOF（入力ストリームの終了）になったら、新しいworkerの開始を止めます。内部の終了方針として、参考実装と同様、AttemptとCI waitには停止を通知し、ValidationとPublicationは完了まで待ちます。

EOFはユーザーの`task.cancel`要求ではありません。EOFを理由にTask全体を`Cancelled`へ変更しません。各operationの終端状態は、Serviceが停止確認と結果保存に基づいて決めます。停止や保存を確認できない場合は成功や取消を推定しません。

全workerのjoin（終了待ち）が終わってからServiceと依存を破棄し、最後にDB排他を解放します。プロセスが異常終了した場合は、次回起動時に復旧します。正常終了でも、外部処理が停止したと確認できなければ`Cancelled`として保存しません。

製品の終了待ちに新しい期限や強制kill（強制終了）規則を加える場合は、別の設計判断として扱います。

## 13操作の接続先と共有データ

以下の表は接続すべき設計を示します。全行が実装済みという一覧ではありません。

| 操作 | Serviceが扱うデータ・境界 | 実行形態 |
| --- | --- | --- |
| `task.create`（Task作成） | 要求のsnapshot（保存時点の写し）、Task、呼出元ごとの要求記録 | 同期処理。応答前にDB transaction（一括保存）を確定 |
| `task.get_context`（Task情報取得） | 同じTaskの履歴・成果物・証拠を安全に投影 | 同期読取 |
| `attempt.run`（Attempt実行） | ModelChoice、role（担当）、入力、Attempt、実行受付 | 受付を保存した後にworker（バックグラウンド処理）で実行 |
| `operation.get`（処理状況取得） | 6種類すべてのoperation（処理）の状態と型付き結果・エラー | 同期読取。処理は起動しない |
| `operation.list_logs`（ログ一覧） | 正式なログ参照境界。現状は本文の安全設定がないため公開しない | 同期読取。承認済み拒否方針を使う |
| `operation.cancel`（処理取消） | 対象operation、取消operation、停止確認 | 取消受付を保存した後に停止処理 |
| `task.cancel`（Task取消） | 同じTaskの対象operation、Task、停止確認 | 取消受付を保存した後に停止処理 |
| `ci.get`（CI観測取得） | target（観測対象）、SHA、Required Check（必須チェック）、Observation（観測結果） | 同期観測。応答前に観測を終える |
| `ci.wait`（CI待機） | Task、target、deadline（期限）、Observation、待機operation | 受付を保存した後にworkerで待機 |
| `validation.run`（検証実行） | Artifact / tree、checks（検査項目）、Validation、実行受付 | 受付を保存した後にworkerで実行 |
| `decision.record`（監督判断の記録） | Artifact / tree、監督判断、参照証拠 | 同期処理。応答前にDB transactionを確定 |
| `publication.publish`（公開） | Artifact / tree、公開条件、独立Publication ID | 受付を保存した後にworkerで実行 |
| `task.finish`（Task完了） | 現在Artifact、最新の採否、指定EvidenceRef、未終了処理 | 同期処理。応答前にDB transactionを確定 |

共通の取得処理（getter）には、AttemptRun、TaskCancel、OperationCancel、ValidationRun、PublicationPublish、CiWaitの6種類を接続します。個別の取得処理があることは、共通取得処理が全種類に対応済みであることを意味しません。共通取得への接続は後続実装で行います。

## 再送・同時操作・結果不明時の保存

たとえば、通信が途切れてモデル呼出しの受付応答が届かない場合があります。監督Codexが同じ依頼を再送したときは保存済みの受付応答を返し、モデルを二重に呼び出しません。同じ要求IDで内容の異なる依頼が届いた場合は、別の依頼として再受付せず拒否します。

Taskの更新番号`revision`は、監督Codexが情報を読んでからTaskが変わっていないかを確かめる値です。DB transaction（一括保存）を使い、Taskの変更と受付記録の片方だけが残らないようにします。

GitやGitHubの操作はDB transactionに含められません。外部操作の結果を確認できない場合は、成功・失敗に決めつけず「結果不明」として復旧確認へ回します。


| 境界 | 必須条件 |
| --- | --- |
| 同じ依頼の再送 | 接続側で確定した依頼者ID、操作名、内容を解釈しない`request_id`を組にします。同じ正規化済み入力（normalized payload）なら元の受付応答を返します。内容が異なる入力（payload）は衝突（conflict）として拒否します。MCP caller（呼出元）をJSONから自己申告させません。 |
| Taskの更新番号 | 正式操作ごとに定めた時点で`revision`を照合します。受付の再送では新しい更新番号を要求しません。各操作の増分箇所は実装前に状態遷移表で固定します。 |
| 受付の保存 | Taskの変更と関連Attempt、operation、要求記録を同じDB transaction（一括保存）で保存します。外部処理はそのtransactionの確定後に始めます。 |
| 実行結果の保存 | 必要な結果、usage（利用量）、Artifact、operation終端を同じDB transactionで保存します。CLI実行中はDB transactionを開いたままにしません。 |
| 取消 | `Cancelling`（停止処理中）と停止確認済みの`Cancelled`（取消完了）を区別します。外部処理が動作中なら、取消受付だけで完了扱いにしません。 |
| 結果不明時の復旧 | 外部への効果、停止、結果保存のどれかが不明なら`recovery_required`（復旧確認が必要）です。自動再実行、成功の推定、捏造した結果を禁止します。 |
| SQLiteとGitの境界 | SQLite transactionとGit操作を一つの原子操作にはできません。保存途中のArtifact（pending Artifact）と専用refを確認する既存の復旧経路を共有します。 |

実装前に、各operationについて、他の処理を受け付けない状態、Taskの更新番号を増やす箇所、取消対象、起動時に行う復旧を照合します。CI waitに、正式仕様にない「TaskがActiveの間だけ受け付ける」「期限は未来でなければならない」といった条件や、独自の同時実行制限は加えません。

### 操作別の確定条件と残る内部判断

正式仕様は、変更要求に含まれる`expected_revision`との照合と、受付後・完了後の`revision`返却を求めています。一方、各処理段階（phase）で何回増やすかは定めていません。正式仕様にない回数を仕様上の決定事項として扱わず、既存の増分方法を確認した上で内部方針を統一します。

| 操作 | 仕様で確定している更新番号・競合条件 | 停止と復旧の条件 |
| --- | --- | --- |
| `AttemptRun` | `expected_revision`を照合し、受付後の`revision`を返します。実行中に次のAttemptは開始できません。この排他を全operationへ広げません。 | 停止を確認する前に`Cancelled`にしません。実行が残っている場合は、起動時復旧で結果不明として扱います。 |
| `OperationCancel` | `expected_revision`を照合し、取消受付後の`revision`を返します。復旧確認前の`RecoveryRequired`対象は`not_cancellable`です。 | 対象の停止を確認するまで取消完了にしません。`target_state`は正式な結果型で定められた値だけを返します。 |
| `TaskCancel` | `expected_revision`を照合し、受付後の`revision`を返します。同じTaskの未終了operationが対象です。 | 関連operationの停止を確認するまでTaskを取消完了にしません。新しい受付との競合は同じtransaction境界で解決します。 |
| `ValidationRun` | `expected_revision`を照合し、受付後の`revision`を返します。外部効果の前に正式なpolicyと対象の所属を確認します。 | 取消をValidation失敗に見せかけません。停止未確認ならworktreeを保持し、`RecoveryRequired`にします。 |
| `PublicationPublish` | `expected_revision`を照合し、受付後の`revision`を返します。副作用の前に採否と公開条件を確認します。 | 起動時に`Accepted`なら`interrupted_before_start`を理由に`Failed`、`Running`なら`RecoveryRequired`にします。pushやPR作成の結果が不明なら再実行しません。 |
| `CiWait` | `expected_revision`を照合し、受付後の`revision`を返します。期限が未来であることやTaskがActiveであることは追加条件にしません。 | 期限到達時は`Failed`/`timeout`とし、結果はnull、最後に保存したObservationを参照します。保存を確定できなければ`RecoveryRequired`です。 |
| `task.create` | `expected_revision`はなく、初期`revision`を返します。同じ依頼者による再送を冪等に扱います。 | Task作成と要求記録を同じtransactionで確定します。 |
| `decision.record` | `expected_revision`を照合し、記録後の`revision`を返します。 | 判断と参照証拠の照合・保存を同じtransactionで確定します。 |
| `task.finish` | `expected_revision`を照合し、完了後の`revision`を返します。terminal状態、証拠不一致、同じTaskで処理中のoperationまたはPublicationがあれば拒否します。 | Pendingからの開始・完了、またはActiveからの完了と完了記録を同じtransactionで確定します。 |
| 読取4操作 | `task.get_context`、`operation.get`、`operation.list_logs`、`ci.get`には`expected_revision`も`request_id`もありません。 | contextのcursor（続きの位置を示す値）はTask、section、page size、snapshot revisionに結び付けます。`ci.get`によるObservation追記をTask状態の変更と混同しません。 |

`Accepted`（受付済み）、`Running`（実行中）、`Cancelling`（停止処理中）、`RecoveryRequired`（復旧確認が必要）を全操作共通の受付拒否条件にはしません。Task完了仕様が明記する受付拒否条件とAttempt間の実行排他を維持します。それ以外の競合は、正式操作の条件と既存の状態更新に基づいて決めます。

この表は、MCP操作仕様の「型と共通規則」「OperationAcceptance」「operation.cancel」「task.cancel」「ci.wait」「task.finish」と、ドメインモデルの取消・起動時復旧・Publication復旧規則に基づきます。実装時に決めるのは、処理段階ごとの`revision`増分、TaskCancelと新規受付の競合順、取消worker間の競合です。通信上の形式（wire field）は追加しません。

### 更新番号を増やす処理段階

この表は実装担当者向けの細則です。`+1`はTaskの`revision`を一つ増やすこと、受付は依頼を保存すること、claim（実行権の取得）は処理を実行する担当が確定することを指します。全ての操作で同じ回数だけ増やすわけではありません。


次の表は正式仕様の引用ではなく、この統合設計で採用する内部方針です。増分は成功したDB transaction（一括保存）ごとに行います。rollback（保存取消）、再送、読取専用の取得処理では増やしません。

| 経路 | 受付時 | 実行権取得・処理中 | 結果保存・復旧時 |
| --- | --- | --- | --- |
| `AttemptRun` | 既存の`+1`を維持します。 | Serviceによる実行権取得、開始、locator（保存場所参照）の更新で既存の増分を維持します。adapter側で重ねて増やしません。 | 既存の終端・復旧更新を維持します。 |
| `PublicationPublish` | 既存の受付時増分を維持します。 | 既存の処理段階ごとの更新を維持します。 | 既存の終端・復旧更新を維持します。 |
| `ValidationRun` | 非同期受付時に`+1`します。 | `Accepted`から`Running`へのDB transaction確定時に`+1`します。自分で更新した後に元の`expected_revision`を再照合して古い値扱いしないようにします。 | Validation記録と終端を同じtransactionで保存し、`+1`します。取消時はValidationを作りません。既存の同期APIは記録時の`+1`を維持します。 |
| `CiWait` | `expected_revision`を照合し、Taskの現在値を返します。 | Taskの`revision`は増やしません。 | Observationとwait終端を専用記録へ保存します。Taskの`revision`は据え置きます。 |
| `OperationCancel` | 対象の取消予約と新しい取消operationの受付を同じtransactionで保存し、`+1`します。 | 停止通知だけでは増やしません。対象workerに既存の更新があれば維持します。 | 対象終端が既に確定している場合は、取消operationの終端を新たに保存して`+1`します。両方を同じtransactionで確定する場合は一度だけ増やします。 |
| `TaskCancel` | 対象集合、取消予約、新しいoperationを保存して`+1`します。 | 対象workerの既存更新を維持します。 | 最後の停止確認、Task取消、取消operation完了を同じtransactionで確定して`+1`します。先に確定した対象終端の増分を重ねません。 |
| `Decision` / `Finish` | 判断の記録または完了transactionで既存の`+1`を維持します。 | workerは起動しません。 | Pendingからの開始・完了も一つのtransactionで行い、一度だけ増やします。 |

reviewerの開始時`revision`とverdict保存前の照合は既存どおり維持し、他の操作による変更を覆い隠しません。Validationは実行時と結果保存時に対象Artifact/treeを再確認します。Taskの`revision`が変わっただけで、結果を別の成果物へ付け替えません。

CI ObservationはTaskの`revision`を変えずに追加できます。その場合でもcontext（Task情報取得）のページ境界が変わらないよう、初回読取時に安全化済み情報の投影と並び順を固定し、プロセス内snapshotとして保持します。opaque cursor（内容を解釈しない続き位置）はsnapshot、Task、section、page size、`revision`、次位置に結び付けます。続きは固定済みsnapshotから返します。Taskの`revision`変更後や再起動後にsnapshotを使えなければ`invalid_cursor`を返します。毎回最新一覧をoffset（先頭からの件数）で読み直す方法は使いません。通信形式やSQLite tableは追加しません。

### 取消と新しい依頼が同時に来た場合

**通常の例:** Task取消を受け付けたら、対象処理と取消要求をDBに記録してから停止を通知します。停止を待つ間は新しい変更依頼を拒否します。全対象の停止を確認してからTask取消を確定します。読取と同じ依頼の再送は引き続き受け付けます。

**競合する例:** 個別処理の取消受付直後に対象が正常終了した場合は、その正常終了記録を保持します。停止したことにして履歴を書き換えず、取消要求を`not_cancellable`で失敗させます。

次の手順は、この動作を実現する内部設計です。`IMMEDIATE transaction`はSQLiteで書込み順を先に確保する一括保存です。CAS（条件付き更新）は、読み取った状態が変わっていない場合だけ更新します。取消予約は未終了のTask取消記録で表します。


1. 取消受付では、まず`IMMEDIATE transaction`内で冪等記録を確認し、その後に`revision`を照合します。同じ依頼の再送では、取消予約も`revision`も変更しません。
2. `operation.cancel`は、同じTaskに属する取消可能な対象をCASで予約し、取消operationも同じtransactionに保存します。対象が先に終端した場合は、状態を変更せず`not_cancellable`にします。停止通知はtransaction確定後に送ります。対象がすでに`Cancelling`で、別要求による取消が進行中の場合も`not_cancellable`です。この条件をTask全体の通常の同時実行制限には広げません。
3. `task.cancel`は、対象集合と取消operationを一つのtransactionで保存します。対象は`Accepted`、`Running`、`Cancelling`状態の実行operationです。取消要求operation自体を再帰的に対象へ含めません。停止または外部効果が不明な`RecoveryRequired`対象があれば、Task取消を受け付ける前に`not_cancellable`で拒否し、既存の復旧確認を求めます。
4. 未終了のTaskCancel記録をTaskの取消予約として使います。新しい列やTask状態は追加しません。予約中は新しいTask変更や非同期の副作用を拒否します。読取、`ci.get`、同じ依頼の再送は拒否対象にしません。通常時に全operationを一律に排他する条件ではありません。
5. 停止できない対象があれば、取消受付前に`not_cancellable`で拒否します。対象の一部だけを止めてTask取消成功とはしません。停止後の結果保存が不明なら、可能な範囲で`RecoveryRequired`を記録し、安全な固定エラーとoperation IDを返します。
6. `operation.cancel`受付後に対象が自然完了した場合は、その`Completed`または`Failed`結果を保持します。取消operationは`Failed`/`not_cancellable`で終端させます。正式な結果型にない`target_state=completed`や、架空の`Cancelled`は返しません。TaskCancelでは自然完了した対象を`cancelled_operation_ids`へ含めません。全対象の停止と新規受付の閉鎖を確認できた場合にTask取消を確定します。
7. Task取消予約は、停止確認済みの取消完了、または取消失敗を確定して保存するまで保持します。`RecoveryRequired`を成功とみなして予約を解放しません。異常終了後は起動時に復旧確認し、対象operationを自動再実行しません。

この順序は、競合時にもTask全体の取消契約を守るための内部設計です。新しい操作やTask状態、通常時の並行禁止policyは追加しません。

## 検証・判断を同じ成果物に結び付ける

ここでいう「証拠」は、保存済みの検証、レビュー、採用判断、公開、CIの記録です。呼出元が「成功した」と自己申告する欄ではありません。

たとえば、成果物Aのテスト成功後に修正して成果物Bを作った場合、Aの成功記録をBの検証結果として使えません。PRのCIも、公開したコミットと観測対象が一致するかを確認します。記録の識別子を表す`EvidenceRef`は、こうした所属と一致の確認に使います。


| 記録 | 照合する事実 |
| --- | --- |
| `Artifact` | どのTaskの記録か、保存したGit tree、生成元Attempt、専用refを確認します。Gitのignored設定で除外される新規ファイルを含めません。 |
| `Validation` | 同じTaskのどのArtifactとtreeを対象にしたか、各checkで何を観測したかを確認します。Providerが成功しただけで`passed`と判定しません。 |
| `ReviewVerdict` | reviewer Attemptと対象Artifact/treeを確認します。レビューは任意であり、監督Codexの採否とは別の記録です。 |
| `CodexDecision` | 同じTaskのArtifact/tree、採否、参照証拠を確認します。古い成果物への`accepted`判断を新しい成果物へ流用しません。 |
| `Publication` | 独立したPublication ID、operation、保存Artifact、repository、公開先のhead SHAを確認します。pushやPR作成の結果を確認できなければ再実行しません。 |
| `CI Observation` | target、観測したSHA、Required Checkの取得状況、各checkを確認します。必須CIチェック集合を取得した結果が空（known-empty）の場合や取得不能の場合を`passed`へ変換しません。 |
| Task完了 | 現在のArtifact、最新の`accepted`判断、指定された各`EvidenceRef`の所属と意味を確認します。追加のevidence policyが未設定でも、レビューを常時必須にはしません。 |

後続のTask完了処理では、ValidationとCodexDecisionに加えて、Review、Publication、CIの全`EvidenceRef`を照合します。部分実装が拒否していた証拠種別も、既存の正式仕様を満たすために接続します。

## 履歴分類の改訂

### なぜ分類を追加するのか

分類条件の正本は[履歴設計](implementation-review-model.md#守る規則)です。ここでは、なぜ分類を改訂するのか、統合時にどの分類を使うのかを例で説明します。


[ドメインモデル](domain-model.md)には、実装Artifactの検証失敗後、レビューを経ずに修正Attemptを始める例があります。[履歴設計](implementation-review-model.md)の既存規則では、`RetryOf`は失敗したAttemptを、`ReworkFrom`は修正要求を返したレビューを参照します。そのため、どちらの関係もこの例には当てはまりません。

正式な`attempt.run`では、`explorer`役割や明示的な`BaseInput`も指定できます。既存の履歴関係は、最初の`implementer`や過去Attemptへの参照を中心にしています。調査後に`BaseInput`から実装した新しい履歴を、旧データ移行用の`LegacyUnspecified`として保存することはできません。

**改訂:** 既存の関係を維持したまま`SpecifiedInput`を一つ追加します。`BaseInput`や`ArtifactInput`を別に二重保存せず、既存の履歴分類で表せない明示入力の実行を記録します。Artifactの生成Attemptが分かる場合は必ずそれを参照します。`BaseInput`、またはArtifactの生成Attemptが分からない場合は、参照先なしを明示的な例外として認めます。Task内の`sequence`は一意にします。`sequence=1`で`BaseInput`を使う`implementer`だけを、従来どおり一度だけ`Initial`とします。

### 正式attempt.runと既存レビュー補助APIを分ける

[AIレビューPR #108](https://github.com/satokenn/AI-dev-orchestrator/pull/108)の`submit_artifact_review`は補助APIです。このAPIの存在を根拠に、一般の`attempt.run`へValidation IDやcriteria（評価基準）の必須条件を加えません。

- 一般の`attempt.run`は、正式なrole、input、instructionをそのまま受け取ります。`reviewer`の場合は、読取専用実行、安全化後の16 KiB上限、停止確認を共通化します。instructionからValidation IDやcriteria一覧を推測しません。
- `reviewer`と`ArtifactInput`の組合せでは、同じTaskの現在Artifact/treeにレビューを結び付けます。古いArtifactはProvider起動前に拒否します。Providerが成功した場合は、既存のverdict JSON規則で出力を検査し、安全化したVerdictを保存します。Artifactの生成元が不明なら、履歴参照を作りません。このときの関係には`SpecifiedInput`を使います。
- `reviewer`と`BaseInput`の組合せでは、指定されたcommitを読取専用で処理します。Artifact IDを作らず、Artifactに対するReviewVerdictも作りません。Providerの終了結果とusageは通常のAttemptとして記録します。ドメインモデルの「Artifactに対するVerdictを1件持てる」という定義を、全reviewerが必ずArtifactを持つという意味に変えません。
- 既存の`submit_artifact_review`は、Validation IDとcriteriaを明示する補助APIとして維持します。そこにある厳しい受付条件を取り除かず、一般の通信入力（wire）を無理にそのAPIの引数へ変換しません。実行、安全化、Verdict保存の共通部分はService内部で再利用します。

代表例と分類の決定順:

| 要求・根拠 | 保存案 |
| --- | --- |
| Task最初の`implementer`と`BaseInput` | 従来の`Initial`として記録します。 |
| 現在Artifactを対象とし、成功した`implementer`の生成元を確認できるreviewer | 従来の`ReviewOf`を維持します。 |
| 成功Artifact AのValidation失敗後、レビューを経ずに修正 | `SpecifiedInput`を使い、Artifact Aを生成したAttemptを参照します。 |
| `explorer`による`BaseInput`調査 | `SpecifiedInput`を使い、参照先は持ちません。 |
| 調査後、明示的な`BaseInput`から行うimplementer | `SpecifiedInput`を使い、参照先は持ちません。 |
| 生成元が不明な現在Artifactを対象とするreviewer | `SpecifiedInput`を使い、参照先は持ちません。Verdictは対象Artifact/treeに結び付けます。 |
| `BaseInput`を使うreviewer | `SpecifiedInput`を使い、参照先もArtifact Verdictも作りません。 |
| retry、escalation、reworkを明示する既存API・記録 | 従来の分類と検証を維持します。 |

レビューなしでArtifact Aを修正するv2入力の例（IDは例示）:

```json
{
  "schema_version": "v2",
  "request_id": "repair-a",
  "task_id": "task-a",
  "expected_revision": 6,
  "provider_id": "codex",
  "model_id": {"kind": "provider_default"},
  "instruction": "検証で見つかった問題を修正する",
  "role": "implementer",
  "input": {"artifact_id": "artifact-a"}
}
```

後続実装では、例えば`relation_kind=specified_input`、`related_attempt_id=attempt-a`として保存します。依頼から架空のreview verdictを作らず、同じTaskのArtifact Aに記録された生成元だけを参照します。`BaseInput`の場合は参照先を`null`にします。MCP入力へrelation field（関係を指定する項目）は追加しません。

一般の通信形式（wire）にないretry/rework理由を、Providerの違いや過去レビューの存在だけから自動推定しません。理由を明示する既存経路では従来の分類を使います。それ以外の一般入力を`SpecifiedInput`として記録することは、移行用の`LegacyUnspecified`へ分類を逃がすことではありません。

この改訂では、保存に使うenumの正式な値と、参照先がない場合の例外を定めます。[履歴設計](implementation-review-model.md)、[ドメインモデル](domain-model.md)、[モデル選定仕様](model-selection-spec.md)も同時に更新します。Domain、Ledger、Service、DB移行、履歴読取、受付・再送の実装は後続作業です。既存履歴の関係を再分類しません。移行に失敗した場合は元のDBを保持します。移行番号は実DDLとの互換性を確認した後に割り当てます。

## 実装担当者向けの保存形式と実装順

ここからは、前述の動作をDBへ接続する方法を説明します。大切な点は、同じTaskの記録を共通DBで扱うこと、旧データの意味を推測で書き換えないこと、結果不明を成功や失敗へ置き換えないことです。


全操作は同じTask ownerとDBを使います。operationごとの入力記録は既存の型付き保存を維持し、MCP側に別のLedgerを作りません。

| 保存対象 | 最小の一意性・整合性 |
| --- | --- |
| 要求受付 | 呼出元、tool名、`request_id`、正規化済み入力、元の受付応答を結び付けます。Task作成や同期要求にも共通の規則を適用します。 |
| Validation operation | operation ID、Task、対象Artifact/tree、profile設定snapshotまたは明示checks、状態、時刻、結果または固定エラーを保存します。 |
| 取消operation | operation ID、Task、TaskCancel/OperationCancelの種別、対象ID集合、受付時revision、状態、結果または固定エラーを保存します。 |
| CI wait | operation ID、Task、target、秒とnanosで表すdeadline、状態、最後のObservation参照、結果または固定エラーを保存します。 |
| 履歴分類 | 既存のsequence、role、input、参照先を維持します。`SpecifiedInput`は定めた条件で追加し、既存記録の意味を変更しません。 |

Unknown（不明）のValidationにも独立した`validation_id`を発行し、Validation operationの正式結果記録（canonical result）と対象Task/Artifact/treeを同じtransactionで保存します。旧形式のboolean `passed`だけを持つ記録に、UnknownをFailedとして詰め込みません。共通取得処理、context、`EvidenceRef`解決処理は、新しい正式結果記録と旧Validationを区別して読みます。両形式を通じて`validation_id`が重複しないよう、同じtransaction内で検査します。旧行の状態は元の意味のまま保持します。

既知の開発branchのDB schemaは、実際のDDL、index、FK、trigger、保存行を確認してから共通のDB移行へ接続します。`user_version`の番号だけでschemaを読み替えません。未知または判別できない形式は、書込みを始める前に拒否します。共通DB担当が移行の依存順を確認して番号を割り当て、開発branchごとに同じ番号を別用途へ使わないようにします。実DDLと旧fixtureを検査するまでは、新しい番号の移行を完成扱いにしません。

実装を再開したら、まず共通の保存・受付・取得を整え、次にValidation・取消・履歴をServiceへ接続します。その後、全ての証拠を照合し、MCPサーバーと13操作の通信処理を組み立てて、一連の動作を確認します。各段階で更新番号、安全化、target解決処理を共通して使います。PRは独立した目的または既存の依存関係に基づいて分け、内部作業の小段階ごとには分けません。

## 設計の監査と再開条件

| 項目 | 決定・確認内容 |
| --- | --- |
| 起動と依存の所有 | DB排他を先に取得して既存の`new_with_run_lock`へ渡します。workerは範囲付きで管理し、依存は全workerの終了まで保持します。 |
| 設定の接続 | 起動側が検査セット、許可一覧、秘密情報検査、モデル確認の処理を渡します。新しいMCP/TOML設定形式は追加しません。 |
| 終了とworker | EOFで受付を止め、Attempt/CIへ停止を通知し、Validation/Publicationの終了を待ちます。EOFをTask取消へ読み替えません。 |
| 状態と競合 | 処理段階ごとの更新、CI用の固定snapshot、TaskCancelの永続的な予約を扱います。CASと`IMMEDIATE transaction`で順序を確定します。 |
| 履歴と一般reviewer | 正式な通信形式と補助APIを区別します。`SpecifiedInput`の改訂、読取、DB移行への影響を定めます。 |
| 未実装との区別 | 起動時の組立処理（factory）、全operation、全`EvidenceRef`照合、本番依存の接続は未実装です。設計書を動作確認の代わりにはしません。 |

本書の確認では、文書の内容・参照・規則が一致することと、読者が目的や責務を理解できることを確かめます。製品コードの整形・静的解析・テスト・動作確認は統合実装後に行います。設計段階では製品コードを変更しません。

**実装への引渡し条件:** 本改訂の監査・レビューと必須CIが成功し、採用する仕様の版を固定した後、単一の実装担当へ変更範囲と既存データとの互換条件を渡します。実行時の対応完了は、各Issueの完了条件、関連する検証、CIを満たしてから判定します。
