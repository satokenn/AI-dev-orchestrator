# Operation ServiceとMCPの統合設計

更新日: 2026-10-11（日本時間、Asia/Tokyo）。**本変更は設計・仕様の改訂であり、製品コード・DB migration・runtime挙動は変更しない。**

## 目的と要約

監督Codexが、一つのTaskについて実行・検証・判断・Draft PR公開・CI観測を依頼し、同じ成果物に結び付いた履歴から次の操作を判断できるMVPを作る。そのために、既存の13操作を一つのServiceとSQLite DBへ接続する。

実装を先行させず、起動から終了までの依存関係、共通状態、証拠の所属、失敗時の扱いを先に閉じる。本書は接続設計を説明する。入力・出力の正本は[MCP操作仕様](mcp-operation-contract-reference.md)、状態と所属の正本は[ドメインモデル](domain-model.md)。実装状況は対応IssueとPRで管理し、本書の接続設計と区別する。

起動・設定注入・終了・状態遷移・取消競合の方針を定める。履歴分類SpecifiedInputは関連する3仕様書へ同じ変更で反映する。以下の全操作が実装済みという意味ではない。

対象外は、自動モデル選択、自動修正、自動レビュー、自動merge、常時有効なBudget、rawログ本文の新規公開。既存仕様で任意の機能を必須化しない。

## 根拠と適用版

本書は[アーキテクチャ](architecture.md)、[ドメインモデル](domain-model.md)、[履歴設計](implementation-review-model.md)、[モデル選定仕様](model-selection-spec.md)の責務を、MCPと共通Serviceへ接続する設計である。

MCP入力・出力は[MCP操作仕様](mcp-operation-contract-reference.md)を正本とする。ただし本PRのmain基準にはまだv1があり、統合実装は承認済みの[ModelChoice改訂PR #84](https://github.com/satokenn/AI-dev-orchestrator/pull/84)と[CI改訂PR #88](https://github.com/satokenn/AI-dev-orchestrator/pull/88)を適用したv2を対象とする。本PRだけでv2実装・MCP公開済みとはしない。

[参考MCP実装PR #109](https://github.com/satokenn/AI-dev-orchestrator/pull/109)のworker/stdio構造は再利用対象だが、正式仕様や統合済み挙動とは区別する。本文で既存Rust APIに言及する箇所は後続Serviceの接続方法を示し、mainにそのAPIがすべて存在するという保証ではない。内部phaseのrevision回数や終了順序は、本書で選んだ実装方針として示す。source照合の固定基準は参考PR #109の`578169249c53035ca7b089f4378c59fb9a37ec52`と、未公開の共通Serviceローカルcommit `933eb601132eeb7effbd402968a2c5c4c396c513`。後者はmainや本PRへ実装が含まれることを意味しない。

## 利用の流れと責務

1. 監督CodexがTaskを作り、Provider・ModelChoice・役割・入力・指示を明示してAttemptを依頼する。
2. Serviceが所属、revision、冪等性、安全条件を確認し、受付を保存する。受付後にworkerがProviderを呼ぶ。
3. Provider成功と、Artifact保存、Validation、review verdict、監督判断はそれぞれ別の事実として保存する。
4. 監督Codexが必要な検証やレビューを選び、対象Artifactに対する判断を記録する。
5. Draft PR公開ではServiceが保存済みArtifactと採否・検証・secret scanを照合する。CIは公開先とSHAを確認して観測する。
6. 監督Codexが証拠を指定してTask完了を依頼する。Serviceは同じTask・成果物の証拠と未終了処理を確認する。

| 要素 | 一つに集約する責務 |
| --- | --- |
| 監督Codex | 要求解釈、モデル選択、再試行・レビューの実施判断、採否、完了要求 |
| MCP adapter | JSON-RPC、正式入力の検査、trusted callerの受渡し、Service結果の形式変換 |
| Operation Service | 受付、revision、所属、安全性、状態遷移、実行、永続化、共通取得、復旧 |
| SQLite Ledger | Taskと全実行・成果物・証拠・要求記録を原子的に保存する |
| Provider / Validator / GitHub・CI adapter | 指定された外部処理を実施し、観測事実と停止結果を返す |
| worker管理 | 受付済みoperationの実行開始、重複起動防止、停止通知、終了待ち。業務状態を独自決定しない |

## 起動・依存関係・終了の接続

### 所有と起動

プロセスの組立箇所がLedger、Workspace、ProviderResolver、Scanner、ModelCatalog、Validator設定、Publication gateway、CI providerを所有する。Serviceはその生存期間内で依存を借用する。workerが参照するServiceと依存は、すべてのworkerが終了するまで破棄しない。

参考MCP実装の`&'static Service`とテスト用`Box::leak`は、製品の必須条件にしない。接続案は、プロセス所有の依存とServiceをworkerの終了待ちまで生存させる構造とする。具体的にはhandlerの借用lifetimeを一般化し、`thread::scope`内でtransportとscoped workerを動かす。参考実装の`JoinHandle`と通常spawnはscopedな管理へ変更する。Arc化だけで借用依存のlifetime問題が解消すると考えない。この案はsource上の所有関係を確認済みだが、コンパイル・runtime確認は未実施。

起動順:

1. 既存の設定形式を読み、依存を構成する。新しいMCP専用Validator設定形式は作らない。
2. canonicalなLedgerの排他を取得する。
3. 排他を保持してDBの既知schemaを検査・移行する。未知のschemaは変更せず拒否する。
4. 同じ排他をServiceへ引き継ぎ、未完了operationとpending Artifactを復旧する。
5. 必要な依存を注入し、MCP受付とworker管理を開始する。

**既存との差分:** 現在の共通コードはLedger open・migration後にServiceが排他を取得する。上の順序は、稼働中の別プロセスが先にmigrationする窓を閉じる案である。source監査では、既存`LedgerRunLock::acquire(path)`→`SqliteExecutionLedger::open(path)`→`OperationService::new_with_run_lock(..., Some(lock))`で実現可能と確認した。canonical parentは既存CLI helperと同じ条件で先に用意する。既存のlock実装とServiceコンストラクタを共有し、二重lockや二重startup recoveryを作らない。これは既存APIの組合せによる案であり、正式仕様がmigration順を既に要求していたという説明はしない。

in-memory Ledgerには永続DBの自動startup recoveryを適用しない。稼働中workerに対してstartup recoveryを再実行する公開操作を追加しない。

### 設定と不足時の扱い

| 依存 | 構成方法・不足時の扱い |
| --- | --- |
| ProviderResolver | 明示指定されたProviderを解決する。MCPが独自にProviderを選ばない |
| ModelCatalog | trustedな事前検証源を注入する。named Modelは未設定・未知なら起動前に拒否。provider_defaultはその不足だけで拒否しない |
| SecretScanner | 既存Serviceの安全境界へ注入する。設定がない場合の拒否条件を迂回せず、公開は拒否する |
| Validator | #50の既存設定とWorkspace境界を再利用し、MCPの正式checks入力をServiceで解決する。参考実装のValidationPolicyと共通側のper-call Validatorを一本化する。未接続を架空の必須wire fieldで補わない |
| Publication gateway / CI provider | 明示注入。外部エラー本文をそのままMCPへ返さず、安全な固定エラー・参照に変換する |
| Budget Policy | 明示的に有効にした場合だけ適用。既承認のTask実行回数上限から始め、保証不能なhard上限はProvider起動前に拒否する |

### 注入契約として確定する範囲

MVPの組立入口は、trusted hostが所有する依存を受ける。設定sourceをMCP requestから受けたり、#50のTOMLに未定義のsecret/catalog/profile欄を追加したりしない。依存の本番実装が未完成であることは、接続設計の空白と区別して対応Issueへ残す。

- **Validator policy:** trusted hostがprofile IDと設定済みValidatorの対応、および明示checksのcommand/workspace allowlistを提供する。profile IDはopaqueであり、Serviceがファイル名から推定しない。登録profileは実際のchecksと設定ID/hashを解決し、受付時にそのsnapshotを固定する。明示checksは要求どおりの順序・引数・timeoutを保持し、同じpolicyで実行前に認可する。未登録profile、不許可command/workspace、policy未設定は外部実行前に拒否する。fieldの有無でprofileとchecksを判別し、明示checksの空配列を「未指定」へ変換しない。空配列には架空のminItemsを追加せず、検査なしをpassedとしないUnknown結果を記録する。RepositoryConfigから作ったValidatorを明示登録できるが、平坦な配列を暗黙のprofileとして公開しない。
- **SecretScanner:** 既承認の注入方式を維持する。hostがknown secret sourceとScannerを所有し、textのredactionとGit tree/payload scanを提供する。空のsecret集合を安全性の証明として自動採用しない。secret source取得や完全検査が保証できなければScannerはtyped failureを返す。MCPにはsecret入力欄を追加しない。
- **ModelCatalog:** hostがauthoritative source・観測時刻・source reference付き実装を注入する。候補一覧やCLI起動可否を実行権限に変換しない。未設定時はnamed Model拒否、provider_defaultは維持する。Source実装の担当は#60/#91であり、存在しないsourceをfactoryが捏造しない。
- **入口:** Service組立とstdio runnerを分離し、入力・出力とtrusted依存を渡して起動できる構成にする。既存CLIは旧Orchestrator経路なので、MCP接続作業にCLI全置換を混ぜない。

これは新しいユーザー設定形式ではなく、正式仕様に既にあるpolicy検査と既承認の依存注入を接続する方針である。具体的なScanner/Catalog実装がtest-onlyの現状を、製品の全機能有効として報告しない。stdio server起動と、必要な依存を注入した各操作の動作確認は実装後の別の完了条件とする。

### Validation要求のsecret境界

実行するcommand/args、check名、設定snapshotの実行文字列は、受付保存より前に既存SecretScannerの`redact_text`で検査する。結果が原文とbyte単位で同じであり、再適用も同じと確認できるcleanな値だけを正確に保存・allowlist検査・実行する。redactionで変わる文字列はpolicy_deniedで拒否する。redact後のcommandを実行して意味を変えることはしない。

check_profile_idもopaque識別子として原文identity検査を行い、secret検知時は拒否し、cleanなbyte列を保持する。Scanner未設定・失敗・完全なredaction保証不能・固定点不成立の場合も、raw checksや要求記録を保存する前に固定policy_deniedで拒否する。profile解決結果にも同じ検査を行う。保存するsnapshotは解決済みchecksと設定ID/hashであり、secretを含み得る設定ファイル全文を複製しない。結果のsummary/check metadata/diagnosticは既存の保存・返却安全化を通し、外部エラー本文を直接返さない。要求の冪等比較はこの検査を通った意味を保持したpayloadで行う。

### 受付からworker開始まで

受付は同じDBのtransactionで保存し、その確定後にworkerを開始する。operation IDごとに同時実行workerを一つにし、実行権の獲得はServiceの状態更新で確定する。process-local mapだけを実行権の根拠にしない。

worker起動失敗時の案は、保存済み受付を失わず、正式エラーの`operation_id`から取得可能にする。副作用開始前でAcceptedと確認できる場合だけ、そのプロセス内の再送で開始できる。再起動時の復旧で確定不能とされたoperationは再実行しない。永続キューや自動再試行機能は追加しない。

### EOF・正常終了・異常終了

MCPのEOFで新規worker開始を閉じる。内部lifecycle方針として、参考実装同様、AttemptとCI waitへ停止を通知し、Validation・Publicationは完了を待つ方式を採用する。すべてのworkerをjoinした後にServiceと依存を破棄し、最後に排他を解放する。

EOFはユーザーの`task.cancel`要求ではないので、Task全体を勝手にCancelledへ変更しない。各operationの終端は、Serviceが停止確認と結果保存に基づいて決める。停止や保存を確認できなければ成功・取消を推定しない。

プロセスの異常終了は次回起動時復旧に委ねる。正常終了時にも、外部処理の停止未確認をCancelledとして保存しない。製品の終了待ちに新たな期限や強制kill規則を追加する場合は、別の設計判断として扱う。

## 13操作の接続先と共有データ

以下は接続すべき設計であり、全行が実装済みという表ではない。

| 操作 | Serviceが扱うデータ・境界 | 実行形態 |
| --- | --- | --- |
| `task.create` | 要求snapshot、Task、caller単位の要求記録 | 同期transaction |
| `task.get_context` | 同Taskの履歴・成果物・証拠を安全に投影 | 同期読取 |
| `attempt.run` | ModelChoice・役割・入力・Attempt・実行受付 | 受付後worker |
| `operation.get` | 全6種operationの状態・typed result/error | 同期読取、実行しない |
| `operation.list_logs` | 正式ログ参照境界。本文の安全設定がない現段階は公開しない | 同期、既承認の拒否方針 |
| `operation.cancel` | 対象operationと取消operation、停止確認 | 受付後停止処理 |
| `task.cancel` | 同Taskの対象operationとTask、停止確認 | 受付後停止処理 |
| `ci.get` | target・SHA・Required Check・Observation | 同期観測 |
| `ci.wait` | Task・target・deadline・Observation・wait operation | 受付後worker |
| `validation.run` | Artifact/tree・checks・Validation・実行受付 | 受付後worker |
| `decision.record` | Artifact/tree・監督判断・参照証拠 | 同期transaction |
| `publication.publish` | Artifact/tree・公開条件・独立Publication ID | 受付後worker |
| `task.finish` | 現在Artifact・最新採否・指定EvidenceRef・未終了処理 | 同期transaction |

共通取得のoperation種別はAttemptRun、TaskCancel、OperationCancel、ValidationRun、PublicationPublish、CiWait。専用getterの存在を、共通getterの全種対応と取り違えない。後続実装では全6種を同じgetterへ接続する。

## 状態・transaction・復旧

| 境界 | 必須条件 |
| --- | --- |
| 冪等受付 | trusted caller + tool + opaque request_id。同じnormalized payloadには元の受付応答、異なるpayloadはconflict。MCPがcallerをJSONから自己申告させない |
| revision | 各正式操作の規定点で照合する。受付の再送は新しいrevision要求として扱わない。操作ごとの増分箇所は実装前の状態遷移表に固定する |
| 受付保存 | Task変更と関連Attempt/operation/要求記録を同一transactionで保存。外部処理はcommit後 |
| 実行確定 | 必要な実行結果・usage・Artifact・operation終端を同一transactionで保存。CLI実行中にDB transactionを保持しない |
| 取消 | Cancellingと停止確認済みCancelledを分ける。外部処理が動作中なら取消要求の受付だけで完了扱いしない |
| 復旧 | 外部効果・停止・結果保存が不明ならrecovery_required。自動再実行、成功推定、捏造した結果を禁止 |
| SQLiteとGit | 一つの原子transactionにはできない。既存pending Artifact・専用refの確認と復旧経路を共有する |

busyの集合、revision増分、取消対象、起動時復旧を各operationの個別実装から転記して突き合わせる。CI waitについて、正式仕様にないActive Task限定、未来deadline限定、追加busy gateを導入しない。

### 操作別の確定条件と残る内部判断

正式仕様は変更要求のexpected_revision照合と受付・完了後revisionの返却を求めるが、全phaseの増分回数を定義していない。そこを仕様確定事項として捏造しない。既存の増分を確認し、整合する内部接続として統一する。

| 操作 | 正式仕様で固定されるrevision・競合条件 | 停止・復旧の確定条件 |
| --- | --- | --- |
| AttemptRun | expected_revisionと受付後revision。実行中に次のAttemptを始めない。全種operation一律排他の根拠には拡張しない | 停止確認前にCancelledとしない。残存実行中処理は起動時復旧で結果不明を示す |
| OperationCancel | expected_revisionと取消受付後revision。復旧確認前のRecoveryRequired対象はnot_cancellable | 対象停止まで取消完了ではない。target_stateは正式resultの範囲で返す |
| TaskCancel | expected_revisionと受付後revision。同Taskの未終了operationを対象にする | 関連operationの停止確認前にTask取消完了としない。新規受付との競合を同じtransaction境界で解決する |
| ValidationRun | expected_revisionと受付後revision。副作用前に正式policyと所属を確認 | 取消はValidation失敗結果に偽装しない。停止未確認ならworktreeを保持してRecoveryRequired |
| PublicationPublish | expected_revisionと受付後revision。採否・公開条件を副作用前に確認 | 起動時Acceptedはinterrupted_before_startでFailed、RunningはRecoveryRequired。push/PR結果不明は再実行しない |
| CiWait | expected_revisionと受付後revision。未来deadline・Active限定は追加しない | 期限到達はFailed/timeout、result null、最後の保存Observation参照。保存確定不能はRecoveryRequired |
| task.create | expected_revisionなし、初期revisionを返す。caller冪等 | 作成・要求記録は同一transaction |
| decision.record | expected_revision、記録後revision | 判断と参照証拠の照合・保存は同一transaction |
| task.finish | expected_revision、完了後revision。terminal・証拠不一致・同Taskの処理中operation/publicationを拒否 | Pendingのstart→completeまたはActiveのcompleteと完了記録を同一transactionで確定 |
| 読取4操作 | task.get_context、operation.get、operation.list_logs、ci.getはexpected_revisionとrequest_idを持たない | context cursorはTask/section/page size/snapshot revisionに束縛。ci.getのObservation追記をTask状態変更と混同しない |

OperationのAccepted/Running/Cancelling/RecoveryRequiredを全操作一律にbusyとする新規制約は採用しない。Task完了の明記されたbusy条件と、Attempt間の実行排他をまず保持する。それ以外の競合は正式操作の条件と既存の状態更新を確認して決める。

表の根拠はMCP操作仕様の「型と共通規則」「OperationAcceptance」「operation.cancel」「task.cancel」「ci.wait」「task.finish」と、ドメインモデルの取消・起動時復旧・Publication復旧規則である。内部判断として残るのはphase別revision増分、TaskCancelと新規受付の競合順、取消worker間の競合であり、wire fieldの追加ではない。

### phase別revisionの内部方針

下表は正式仕様の引用ではなく、統合で採用する内部方針。増分は成功したDB transactionごとであり、rollback・再送・read-only getterでは増やさない。

| 経路 | 受付 | claim・実行 | 結果・復旧 |
| --- | --- | --- | --- |
| AttemptRun | 既存+1を保持 | Serviceのclaim・開始・locator更新の増分を保持。adapterで重ねない | 既存終端・復旧更新を保持 |
| PublicationPublish | 既存受付増分を保持 | 既存phase更新を保持 | 既存終端・復旧更新を保持 |
| ValidationRun | 非同期受付で+1 | Accepted→Runningのcommitで+1。元expected_revisionを再照合して自己更新をstaleにしない | Validation記録と終端を同一transactionで+1。取消時はValidationを作らない。既存同期APIも記録時+1を保持 |
| CiWait | expected_revisionを照合し、据置値を返す | Task revisionを増やさない | Observation・wait終端は専用記録へ保存。Task revisionは据置 |
| OperationCancel | 対象予約と新取消operation受付を同一transactionで+1 | 通知自体では増やさない。対象workerの既存更新は維持 | 対象終端を既に確定済みなら取消operationの新しい終端commitのみ+1。同一commitで両方確定できる場合は一度だけ+1 |
| TaskCancel | 対象集合・取消予約・新operation保存で+1 | 対象workerの既存更新を維持 | 最後の停止確認・Task取消・取消operation完了の同一commitで+1。先行する対象終端commitの増分は重ねない |
| Decision / Finish | 記録・完了transactionで既存+1 | workerなし | Pending start→completeも一つのcommit、一度の増分 |

Reviewerの開始revisionとverdict保存前revision照合は既存どおり維持する。他操作による変更を覆い隠さない。Validationは対象Artifact/treeを実行・結果保存時に再確認し、現在revisionが変わっただけで別成果物へ結果を付け替えない。

CI ObservationがTask revisionを変えず追記されてもcontextのpage境界を変えないため、初回読取時にsectionの安全化済み投影と順序を固定してprocess内snapshotとして保持する。opaque cursorはsnapshot、Task、section、page size、revision、次位置に束縛する。続きは固定snapshotから返し、Task revision変更や再起動でsnapshotが利用不能ならinvalid_cursor。取得のたびにlive一覧をoffsetで読み直さない。新しいwire fieldやSQLite tableを加えない。

### 取消と新規受付の競合順

1. 受付はIMMEDIATE transactionで冪等記録を先に確認し、その後revisionを照合する。同一再送は予約やrevisionを再変更しない。
2. operation.cancelは同Taskのcancellable対象を状態CASで予約し、取消operationを同じtransactionに保存する。targetが先に終端した場合はnot_cancellableで変更なし。通知はcommit後。対象が既にCancellingである別requestの重複取消もnot_cancellable。Task全体の通常busy条件には拡張しない。
3. task.cancelは同Taskの対象集合と取消operationを一つのtransactionで保存する。対象はAccepted/Running/Cancellingの実行operationで、取消要求operationを再帰的に取消対象へ入れない。RecoveryRequiredで停止・効果が不明な対象を含む場合は、TaskCancelを受付する前にnot_cancellableで拒否し、既存の復旧確認を要する。
4. 未終了TaskCancel記録をTaskの取消予約として用いる。新しい列やTask stateを追加しない。予約中に入る新規Task変更・非同期副作用はbusyで拒否する。読取・ci.getと、同一要求の再送は拒否対象にしない。通常時の全operation一律busy gateではない。
5. 停止能力のない対象は取消受付前にnot_cancellableで拒否する。対象集合の一部だけを止めてTask全体の取消成功としない。停止後の保存が不明ならbest-effortでRecoveryRequiredを記録し、安全な固定エラーとoperation IDを返す。
6. operation.cancel受付後に対象が自然完了した場合は、対象のCompleted/Failed結果を保持する。取消operationはFailed/not_cancellableで終端し、正式resultにないtarget_state=completedや架空のCancelledを返さない。TaskCancelの場合は自然完了した対象をcancelled_operation_idsへ入れず、全対象の停止と新規受付の閉鎖を確認できればTask取消を確定する。
7. Task取消予約は、停止確認済み取消完了または確定した取消失敗の保存まで保持する。RecoveryRequiredを成功とみなして予約を解放しない。異常終了後は起動時復旧で確認し、対象operationを自動再実行しない。

この順序はTask全体の取消契約を競合下で守る内部設計であり、新しい操作、Task状態、通常時の並行禁止policyを追加するものではない。

## 証拠の結び付け

| 記録 | 照合する事実 |
| --- | --- |
| Artifact | Task、保存済みGit tree、元Attempt、専用ref。ignored新規ファイルを含めない |
| Validation | 同Taskの対象Artifactとtree、各checkの観測結果。Provider成功からpassedを推定しない |
| ReviewVerdict | reviewer Attempt、対象Artifact/tree。レビューは任意で、監督採否とは別記録 |
| CodexDecision | 同TaskのArtifact/tree、採否、参照証拠。古い成果物のacceptedを新しい成果物へ流用しない |
| Publication | 独立Publication ID、operation、保存Artifact、repository、公開先head SHA。確認不能なpush/PR作成は再実行しない |
| CI Observation | target、観測SHA、Required Checkの取得状況、checks。known-empty・取得不能をpassedへ変換しない |
| Task完了 | 現在Artifact、最新accepted判断、指定された各EvidenceRefの所属・意味。追加evidence policy未設定をレビュー常時必須へ変更しない |

後続Task完了実装はValidation/CodexDecisionに加えReview/Publication/CIの全EvidenceRefを照合する。部分実装が拒否していた証拠を接続することは、既存の正式仕様を満たす作業である。

## 履歴分類の改訂

### 履歴分類

[ドメインモデル](domain-model.md)の例は、成功した実装Artifactに対するValidation失敗の後、レビューなしに修正Attemptを開始する。一方、[履歴設計](implementation-review-model.md)の関係表では、RetryOfは失敗したAttempt、ReworkFromは修正要求を返したレビューを参照する。この例を正しく保存できる関係がない。

さらに正式`attempt.run`にはexplorerと明示BaseInputがあるが、履歴関係の規則は最初のimplementerと過去Attempt参照を基本とする。調査後にBaseInputから実装する場合を、新規履歴でLegacyUnspecifiedとして保存するのは移行用の意味に反する。

**改訂:** 既存関係を保持して`SpecifiedInput`を一つ追加する。既存のBaseInput/ArtifactInputを二重保存せず、通常の履歴分類では表せない明示入力実行に使う。Artifact生成Attemptが確定している場合はそのAttemptを参照し、BaseInputでは参照先なしを明示的な例外として認める。Task内sequenceは一意、最初のAttemptがimplementerの場合のInitialは従来どおり一回だけとする。

### 正式attempt.runと既存レビュー補助APIを分ける

[AIレビューPR #108](https://github.com/satokenn/AI-dev-orchestrator/pull/108)のsubmit_artifact_reviewは補助APIであり、一般のattempt.runへValidation ID・criteria必須条件を追加する根拠ではない。

- 一般attempt.runは正式role/input/instructionをそのまま受ける。reviewerではread-only実行、安全化後16 KiB上限、停止確認を共通化する。instructionからValidation IDやcriteriaのリストを推測しない。
- reviewer + ArtifactInputは同Taskの現在Artifact/treeにレビューを束縛する。古いArtifactはProvider起動前に拒否する。成功Provider出力は既存のverdict JSON規則で検査し、安全化したVerdictを保存する。生成元が不明なら履歴参照を捏造しない。この場合の関係はSpecifiedInputを使う。
- reviewer + BaseInputは指定されたcommitをread-onlyで実行する。Artifact IDを捏造せず、Artifactに対するReviewVerdictは作らない。Provider終了とusageは通常Attemptとして記録する。Domainの「対象ArtifactへのVerdictを1件持てる」を、全reviewerが必ずArtifactを持つという規則へ変えない。
- 既存submit_artifact_reviewは明示Validation IDs・criteria付きの補助APIとして維持する。そのAPIの厳しいgateを消さず、一般wireをその引数へ無理に変換しない。実行・安全化・Verdict保存の共通部分をService内部で再利用する。

代表例と分類の決定順:

| 要求・根拠 | 保存案 |
| --- | --- |
| Task最初のimplementer、BaseInput | 従来のInitial |
| reviewer、現在Artifact、成功implementer生成元を確認できる | 従来のReviewOf |
| 成功Artifact A、Validation失敗、レビューなしの修正 | implementer / SpecifiedInput、A生成Attempt参照 |
| explorerのBaseInput調査 | explorer / SpecifiedInput、参照先なし |
| 調査後のimplementer、明示BaseInput | implementer / SpecifiedInput、参照先なし |
| 生成元不明の現在Artifactへのreviewer実行 | reviewer / SpecifiedInput、参照先なし。VerdictはArtifact/treeへ束縛 |
| BaseInputのreviewer実行 | reviewer / SpecifiedInput、参照先なし、Artifact Verdictなし |
| 既存の明示retry/escalation/rework API・記録 | 従来の分類と検証を維持 |

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

後続実装で保存する関係は、例えば`relation_kind=specified_input`、`related_attempt_id=attempt-a`となる。要求から架空のreview verdictを作らず、同TaskのArtifact Aに記録された生成元だけを参照する。BaseInputまたは生成元不明なら参照先はnull。MCP入力へ新しいrelation fieldを追加しない。

一般wireにないretry/rework理由を、Provider差や過去レビューの存在だけから自動推定しない。明示理由を持つ既存経路は従来分類を使う。それ以外の一般wire実行をSpecifiedInputへ記録することは、履歴を移行用LegacyUnspecifiedへ逃がすものではない。

本改訂では正規enumと参照先なしの例外を定める。[履歴設計](implementation-review-model.md)、[ドメインモデル](domain-model.md)、[モデル選定仕様](model-selection-spec.md)を同時に更新する。Domain/Ledger/Service・migration・履歴reader・受付と再送の実装は後続で行う。旧履歴を再分類せず、移行失敗時は元DBを保持する。migration番号は実DDLの互換確認後に割り当てる。

## 保存形式と実装順

全操作が同じTask ownerとDBを使う。operationごとのpayloadは既存のtyped保存を維持し、MCP側に別Ledgerを作らない。

| 保存対象 | 最小の一意性・整合性 |
| --- | --- |
| 要求受付 | caller/tool/request_id、normalized payload、元の受付応答。Task createや同期要求も共通規則 |
| Validation operation | operation ID、Task、対象Artifact/tree、profile設定snapshotまたは明示checks、状態、時刻、結果/固定エラー |
| 取消operation | operation ID、Task、TaskCancel/OperationCancel、対象ID集合、受付revision、状態、結果/固定エラー |
| CI wait | operation ID、Task、target、秒+nanosのdeadline、状態、最後のObservation参照、結果/固定エラー |
| 履歴分類 | 既存sequence/role/inputと参照。SpecifiedInputは本仕様に従って追加。旧記録の意味を変更しない |

Unknown Validationも独立したvalidation_idを発行し、Validation operationのcanonical resultと対象Task/Artifact/treeを同一transactionで保存する。旧boolean passedだけの記録へUnknownをFailedとして詰め込まない。共通getter/context/EvidenceRef resolverは新しいcanonical resultと旧Validationを識別して読む。validation_idは両経路を通じて衝突しないよう同じtransactionで検査する。旧行の状態は元の意味のまま保持する。

既知のbranch schemaは実DDL・index・FK・trigger・保存行で識別してから共通migrationへ接続する。user_versionだけの読み替えを禁止する。未知・曖昧な形式は書込み前に拒否する。migration番号は共通DB担当が依存順に割り当て、branchごとの同じ番号の別用途を上書きしない。実DDLと旧fixtureの検査を終える前に新しい番号の移行を完成扱いにしない。

実装再開後の順番は、共通保存・受付/取得 → Validation/取消/履歴のService接続 → 証拠の全種照合 → MCP組立・13操作のtransport → 一連の動作確認。各段階で同じrevision・安全化・target resolverを共有する。PRは独立目的または既存依存に従って整理し、内部の小段階ごとに増やさない。

## 設計の監査と再開条件

| 項目 | 決定・確認内容 |
| --- | --- |
| 起動・依存所有 | lock先行取得→既存new_with_run_lockへ引継ぎ、scoped worker、依存は全worker終了まで生存 |
| 設定の接続 | trusted host注入。Validator profile/allowlist、Scanner、catalogに新しいMCP/TOML設定形式を増やさない |
| 終了とworker | EOFで受付停止、Attempt/CI停止通知、Validation/Publication終了待ち。Task取消へ読み替えない |
| 状態と競合 | phase別増分、CI据置snapshot、TaskCancelのdurable予約、CASとIMMEDIATE transactionで順序を固定 |
| 履歴・一般reviewer | 正式wireと補助APIを区別。SpecifiedInput改訂案と読取・migration影響を具体化 |
| 実装済みとの区別 | factory、全operation、全EvidenceRef、本番依存の接続は未実装。設計書で動作確認を代替しない |

本書の接続方針は設計として監査し、実装のfmt/Clippy/tests/runtime/CodeRabbit確認は実装後に行う。設計段階で製品コードを変更・commit・pushしない。

**実装への引渡し条件:** 本改訂の監査・レビューと必須CIが通り、採用する仕様の版が固定された後、単一の実装担当へ変更範囲と既存データの互換条件を渡す。runtime完了は各Issueの全完了条件・関連検証・CIを満たしてから判定する。
