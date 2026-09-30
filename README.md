# AI-dev-orchestrator

AI エージェントを活用した開発オーケストレーションのためのプロジェクトです。

## インストールと使い方

Rust toolchain を用意した環境では、リポジトリ直下で次を実行してインストールできます。

```shell
cargo install --path . --locked
ai-dev-orchestrator --help
ai-dev-orchestrator --version
```

リリースページには macOS の Apple Silicon (`aarch64-apple-darwin`) と Intel
(`x86_64-apple-darwin`) 向け tar アーカイブを公開します。アーカイブには
`ai-dev-orchestrator`、この README、`LICENSE` が含まれます。ダウンロード後は、同じ
リリースの `SHA256SUMS` を取得して次のように検証してください。

```shell
shasum -a 256 -c SHA256SUMS
tar -xzf ai-dev-orchestrator-<version>-<target>.tar.gz
install <target>/ai-dev-orchestrator "$HOME/.local/bin/ai-dev-orchestrator"
```

検証に失敗したアーカイブは実行せず、再ダウンロードしてください。

## 外部 Agent CLI の実行

外部 CLI は `ProcessRequest` に command、引数配列、作業ディレクトリ、環境変数、タイムアウトを指定し、`ProcessRunner` で実行できます。引数は shell 文字列へ連結されず、stdout / stderr と終了状態が `ProcessOutput` に集約されます。長時間実行を停止する場合は `CancellationToken` を渡して `cancel()` を呼び出してください。

`OperationService`のread-only `get_context` Rust APIはTaskの要求snapshotと、`providers`、`usage`、`attempts`、`reviews`のv2 ContextPageを返します。Provider観測の取得不能理由、source、時刻を含め、CLI起動成功から認証やModel利用権を推定しません。このAPIはMCP tool / transportには未接続です。詳細は[Providerの現在状態を観測する](docs/current-provider-observations.md)を参照してください。

timeout / cancel時のprocess group停止、停止確認結果、部分ログと出力上限は、[ProcessRunner の停止と出力回収](docs/process-runner.md)を参照してください。

## AgentProvider 契約

Agent 実行先の違いは `AgentProvider` に閉じ込めます。`ProviderRequest` は workspace、prompt、timeout と必須の `ModelChoice`（named Modelまたは明示したProvider既定値）を受け取り、`ProviderResult` は stdout、stderr、終了状態、任意の `AgentResult` と `UsageCost`、観測できたProvider / Modelを返します。stdout / stderr本文は `expose_stdout_for_trusted_processing` / `expose_stderr_for_trusted_processing` で明示して取得します。これらはsecretを含む可能性があるため、信頼された呼び出し側に限り、redactionなしに保存・返却してはいけません。Provider出力から実Modelを確定できない場合、observed Modelはunknownのままです。実行に失敗した場合は `ProviderError`（不正な要求、未対応Model、実行失敗、タイムアウト、利用不能）を返します。error本文とDebug表示にはraw stdout / stderrを含めません。

Provider の識別子は `ProviderRef` で表し、Provider 固有の CLI 引数やセッション情報は共通契約に含めません。

## Providerの現在状態を観測する

`CodexProvider`、`CopilotProvider`、`AntigravityProvider` の `observe_current()` は、3秒のtimeoutを設定した `--version` probeでCLIの起動有無を観測し、情報源と時刻を付けて返します。CLIの起動成功だけでは認証済みやModel利用可能とは判断せず、それらは `unknown` のままです。この部分実装はquota、API利用量・料金、budgetを取得しません。Provider観測は`task.get_context`にも接続されています。詳細は[Providerの現在状態を観測する](docs/current-provider-observations.md)を参照してください。

## Operation Ledger

`Orchestrator` は、設定された `SqliteOperationLedger` に外部 Provider の起動前の受理・開始を記録し、workspace 準備、Provider実行、validation の結果を終了状態として保存します。CLI は指定された実行Ledgerのサイドカーへ自動的にOperation Ledgerを保存します。`SqliteOperationLedger` は同じ request ID の再送を同じ operation として返し、異なる payload、古い Task revision、同一 Task の実行中操作を拒否します。終了事実、event、validation、review、汎用usage metric、budget、publication参照、明示的に保存された上限付きredacted log、再起動時の`recovery_required`診断を保存します。`save_log` / `save_log_with_truncation`へ渡すbyte列は、呼び出し側が既知のsecretと認証tokenをredact済みである必要があります。usage metricは名前・値・単位を個別に保持し、入力/出力の2値へ集約しません。

## WorkspaceManager

`WorkspaceManager` は Git リポジトリの root を解決し、リポジトリ外の管理ディレクトリに Task / Attempt ごとの専用 branch と Git worktree を作成します。Provider を実行する前に `validate_provider_workspace`（または `ensure_provider_workspace`）で実行先を検証してください。main の working tree や、Manager が作成していないパスは拒否されます。

`cleanup`/`remove` は非 force で専用 worktree を削除します。未コミットの変更がある場合は型付き Git error を返し、worktree と内容を保持します。破棄が必要な場合だけ `cleanup_force` を明示的に呼び出してください。branch は agent のコミットを後続処理で確認できるよう保持されます。branch の merge や PR 作成は WorkspaceManager の責務ではありません。

## Codex CLI Provider

`CodexProvider` は `codex exec` を非対話モードで起動し、`ProviderRequest` の workspace を cwd として使用します。named Modelなら `--model <model>` を渡し、`ProviderDefault`ならModel引数を渡さずCodex CLI設定を使います。Codex CLI は `PATH` から解決され、workspaceへの書き込みを許可する `--sandbox workspace-write`、JSONL 出力の `--json`、実行状態を永続化しない `--ephemeral` も付けます。

JSONLの最後の`item.completed` / `agent_message.text`を`AgentResult`へ、各`turn.completed.usage`の明示済みtoken値を名前と`tokens`単位を保った`UsageMetric`へ変換します。usageの欠落、不正値、負数、または集計不能な値はunknownとしてmetricに追加しません。金額はJSONLに含まれないため推定しません。`error`と`turn.failed`イベント、壊れたJSONL、stdoutの切り詰めはraw本文を含めない実行エラーとして返し、空出力は`AgentResult`なしで返します。stderrだけの切り詰めはJSONL解析を妨げず、raw captureの切り詰め情報で確認できます。未知のイベントは無視しraw出力に残します。公式JSONLイベントは実Modelを示さないため、observed Modelはunknownです。

Provider APIでは成功・失敗結果にUTF-8変換前のstdout/stderr byte列、終了状態、capture切り詰め状態を保持します。raw byte列は`expose_captured_output_for_trusted_processing` / `expose_*_bytes_for_trusted_processing`を呼んだ信頼された処理だけが参照します。ProviderErrorは`kind()`で意味上のエラーを確認できますが、timeout / cancel / interrupted errorのフィールド、通常のerror本文、Debug表示にはraw診断を含めません。raw captureはsecretや認証tokenを含む可能性があり、redactionなしに永続化・返却してはいけません。正規化usageは`save_usage_metrics`へ渡せます。Codex JSONL解析はfixture unit testで検証し、実Codex CLIは通常のPR testで起動しません。AgentResultを含むAttemptの保存はOperationServiceの責務で、Providerのraw stdout/stderr本文やAgentResultを保存・返却しません。長時間実行は `execute_with_cancellation` に `CancellationToken` を渡して停止できます。

## Codex Planner

`CodexPlanner` は `codex exec` の `--output-schema` と `--output-last-message` を使って、Task の内容と Rust が観測した `ProviderAvailability` 一覧から、`PlannerDecision`（provider、reason、execution intent）を読み取ります。Planner は Task を不変借用するだけで、状態を変更しません。`PlannerService` が決定を `ValidatedPlannerDecision` に変換する前に、未知または利用不能な Provider を Rust 側で拒否します。Planner は `--sandbox read-only` と `--ephemeral` で実行され、schema と出力の一時ファイルは処理後に削除されます。

## Validator

`RustValidator` は明示された workspace を cwd として、`cargo fmt`、`cargo clippy`、`cargo test` の機械的なチェックを順番に実行します。全チェックを内包した aggregate の `ValidationResult` を1件返し、各コマンドの終了状態と stdout / stderr の診断は `ValidationResult::checks()` から参照できます。1つでも失敗した場合は aggregate を成功として扱いません。`CommandValidator` と `ValidationCheck` を使えば、同じ `Validator` API で決定的なチェック列も構成できます。

Repository-local の構造化checkは `.ai-dev-orchestrator/config.toml` に置き、`init_repository` で上書きしないtemplateを作成できます。通常のIssue実行はPlannerやProviderを起動する前に設定を読み込み、同じ設定snapshotでValidatorを選びます。config未設定やcheck空集合は成功扱いにならず、実行を開始しません。設定IDとファイル内容のSHA-256はValidationResultに保存されます。設定形式と使用例は[repository-local config](docs/repository-config.md)を参照してください。

## Orchestrator Service

### MCP Operation Service（中核実装）

`OperationService` はRust API `create_task` で `task.create` を受け付け、Task要求snapshot、revision 0、caller・tool・request ID単位の冪等記録を一つのSQLite transactionで保存します。同じ要求の再送には元の作成結果を返し、異なる要求は拒否します。既存Task列にない情報を推測せず、新しいTaskのrole欄には中立値 `unspecified` を設定します。MCP wire adapterはまだ接続していません。

`OperationService` は監督側が明示した `attempt.run` を単一のExecution Ledger DBで受け付け、request ID・Task revision・busy状態を検査してOperation、Attempt、初回の`BaseInput`を原子的に保存します。Provider終了後は、tracked変更とignoredでない新規ファイルをGit treeと専用refに保存し、Attemptの出力Artifactとして同じLedger DBへ記録します。失敗または再試行可能な取消になった実装AttemptのArtifactを入力にする場合、要求ProviderとModelが元Attemptと同じなら`retry_of`、どちらかを変更するなら`escalation_of`でそのAttemptを参照します。成功済み実装AttemptのArtifactを修正入力にする場合は、同一Task・Artifact・treeに`changes_requested`を返した成功済みreviewer AttemptとReviewVerdictが必要で、`rework_from`はそのreviewer Attemptを参照します。証拠がない、別Task・Artifact・treeの証拠、失敗したreviewer Attemptの場合はProvider起動やAttempt記録の前に拒否します。AI review自体は任意で、reviewなしのArtifactInputは失敗Attemptのretryに限り利用できます。永続LedgerではService生成時にベースLedgerとOperation sidecarのcanonical identityに結び付いたプロセス排他ロックを取得し、未完了Operationの復旧を完了してからServiceを返します。ロックはService破棄まで保持されるため、同じLedgerへの別プロセス実行は拒否されます。復旧はService構築時だけに行い、Provider実行中に再度呼び出す公開APIはありません。`SqliteOperationLedger::recover` も同じ `LedgerRunLock` を要求し、既存ロックを共有する呼出元は `new_with_run_lock` に同じLedger identityのguardを渡します。in-memory Ledgerはプロセス間で生存する中断状態を持たないため、構築時の自動復旧を行いません。named Modelは、Serviceへ明示注入したread-only `ModelCatalog` がsourceと観測時刻付きでfreshかつsupportedと確認した場合だけ受け付けます。catalog未設定、lookup error、欠落、unknown / unsupported、古い・未来の記録は、Task / Attemptの記録やProvider起動より前に拒否し、実行時にもProvider準備前とAttempt開始直前に再確認します。Provider defaultはcatalog照会を必要としません。実Provider用catalog adapter / data sourceは後続Issue #60 / #91です。成功・timeout・cancel・中断状態と汎用usageを保存し、Providerのstdout/stderr、AgentResult、秘密を含む診断本文は保存・返却しません。


`task.create`では設定済み`SecretScanner`で要求とIssue snapshotのtextをredactし、再redactして変化しない固定点を確認してからredacted payloadをsnapshotと冪等性記録へ保存します。scanner未設定・失敗・固定点不成立ならraw textを保存せず拒否します。read-only `get_context` Rust APIはredacted Task snapshotと、`providers`、`usage`、`attempts`、`reviews`のContextPageを返します。Contextを返す前にsnapshotを再redactし、page cursorを検証してからProvider observationを取得します。Provider observationはsource・時刻付きで、CLI起動から認証やModel利用権を推定しません。`usage`にはLedger上のProvider報告metricだけを載せ、quotaやbudgetは作りません。Provider一覧の取得失敗や、全Provider observationが一ページへ収まらない場合はContext requestを失敗させます。MCP tool / transportは未接続です。詳細は[Providerの現在状態を観測する](docs/current-provider-observations.md)を参照してください。

Provider起動前にworkspaceのHEADが指定base commitと一致し、tracked・untracked・ignored fileが空であることも確認します。ArtifactInputでは新しいworktreeがcleanであることを確認してから成果物treeを展開し、tree一致を再検証します。hook等が内容を作った場合はProviderを起動せず、workspaceと内容を保持します。Artifactはtracked変更とignoredでない新規ファイルを含み、ignoredファイルは意図的に除外します。Providerがignoredファイルを作った場合は、欠落を隠して成功Artifactを作らず、Operationを `recovery_required` としてworkspaceを保持します。Operationはworkspace pathとbranchを参照として記録し、成果物保存前にworkspaceを自動削除しません。Git refの保存中断はArtifactを `pending_ref` として残します。Service構築時に永続Ledgerのロック下でpending Artifactのtree/refを照合し、ref未作成でtreeが残っていればrefを復元します。tree欠落・ref不一致の場合は `recovery_required` として利用を拒否します。同時に残存するrunning Operationも `recovery_required` として閉じてから依頼受付を始めます。中断結果を推測せず、同じOperationを再実行しません。ValidationとCodex採否はArtifact IDとtree OIDへ結び付き、Publicationは同じ成果物の検証・採否記録に基づいて実行します。任意のAI reviewは`submit_artifact_review`から明示依頼でき、read-only reviewer AttemptとArtifact単位のverdictをRust Service APIで記録します。これはTask完了や修正を自動決定しません。MCP transport、CI evidence接続、redacted log保存、CLIへの配線は別作業です。仕様の正本は[ドメインモデル](docs/domain-model.md)と[MCP 操作契約](docs/mcp-operation-contract.md)です。

同一Artifact公開MVPはRust API `publish_artifact`（証拠/secret scanと原子的受付）、`run_artifact_publication`（受付済み操作の実行）、専用 `get_artifact_publication_operation`（最終結果取得）から利用できます。呼び出し側が`SecretScanner`とGitHub publication gatewayを明示注入しない場合、公開は拒否されます。Artifactの全treeとPR title/body等のpayloadを受付前と実行直前の両方でscanし、検出・失敗・利用不能なら拒否します。設定済みGit実行ファイルでcommit objectのraw tree/parent headerをreplacement refs無効のもと検証し、tree、親commit、Draft PRのhead SHA・base/head branch・title/body一致を確認し、tree・commit SHA・PR番号・Draft状態と検証/採否IDを専用Ledgerに記録します。raw title/bodyはLedgerに保存せず、digestで冪等性を照合します。Git/gh subprocessはProcessRunnerの有限timeoutと上限付き出力を使い、timeout後にpush/PRの成否が不明な場合は`recovery_required`へ進み再実行しません。GitHub CLIの対象repositoryはGitの`origin` fetch URLから決め、push前にorigin URLはHTTPSまたはSSH形式（SCP-like SSHを含む）だけを受け付け、全ての有効なpush URLが同じhost/owner/repoを指すことを確認します。`gh pr list --repo`とliteralな`repos/<owner>/<repo>/pulls` endpointを使い、CLIの継承環境から`GH_REPO`と`GH_HOST`を除去します。title/bodyはProcessRunnerのstdinから`gh api --input -`へ直接渡し、argv、Ledger、OS一時ファイルへ書きません。Unix系以外ではcancellableなraw stdin送信に未対応です。GitHub gatewayは実行claimやGit commit/pushより前にこの能力を検査し、非対応環境での公開を副作用なしで拒否します。gateway実装の`supports_sensitive_stdin_payload`はService稼働中に値を変えず、安定して返す必要があります。受付時と実行時の両方で能力を確認するため、受付後に値が変わると操作が実行できず、起動時復旧まで受付済み状態に残る場合があります。SQLite schema v13はPublication専用operation tableを追加し、v14はTask要求snapshotとcaller単位の冪等記録を追加します。v15/v16はRepository設定に基づくValidationResultの設定IDとSHA-256 versionを既存Attempt validationとArtifact validationへ記録する列を追加します。旧Ledgerからv16へのmigrationは既存recordを保持します。再起動時は未claimの`accepted` publicationを`failed`（`interrupted_before_start`）、実行claim後の`running` publicationを`recovery_required`として記録し、どちらも自動再実行しません。MCP `publication.publish` と既存`operation.get`への統合、publication用cancel APIは未接続であり、公開操作の取得には専用Rust APIを使います。

Task作成のRust Service APIは実装済みで、MCP transportからの呼び出しは未実装です。たとえばmanual Task要求は次のように渡します。

```rust,ignore
let request = TaskCreateRequest::new(
    "request-01",
    TaskSource::Manual,
    "Update parser",
    "Support escaped delimiters",
    vec![],
    None,
);
let created = service.create_task("supervisor", &request)?;
```

応答には初期 `task_id`、revision `0`、`pending` 状態、および保存した要求snapshotが含まれます。このAPIをMCP transportから利用する配線、redacted log保存、CLIへの配線は別作業です。ArtifactInputは同じServiceで成果物treeを指定する入力として利用できます。仕様の正本は[ドメインモデル](docs/domain-model.md)と[MCP 操作契約](docs/mcp-operation-contract.md)です。

manual Task要求に対する応答例:

```json
{
  "request_id": "request-01",
  "task_id": "task-create-1",
  "revision": 0,
  "state": "pending",
  "request": {
    "source": "manual",
    "title": "Update parser",
    "description": "Support escaped delimiters",
    "constraints": [],
    "issue": null
  }
}
```

Service利用側は明示したbase commitで受付し、返されたIDで同期実行または後から状態取得を行います。

```rust,ignore
let accepted = service.submit_attempt(&AttemptRunRequest::new(
    request_id,
    task_id,
    expected_revision,
    provider_id,
    ModelChoice::ProviderDefault,
    instruction,
    role,
    BaseInput::new(repository_path, full_base_commit_oid),
))?;
let snapshot = service.run(accepted.operation_id(), CancellationToken::new())?;
```

以下は現在のCLI Orchestrator実装の説明です。現行LedgerではAttemptのSucceeded/FailedがValidation結果と結合した旧意味論です（`legacy_validation_coupled`）。新しいMCP Operation Serviceの契約ではAttempt stateはProvider呼出し結果のみを表します。両者を同一のruntime semanticsとして扱わないでください。

`Orchestrator`（`OrchestratorService` の別名）は、Pending の Task を Active にし、Task が所有する1つの Attempt を Queued から Running、Provider 実行、Validating へ進め、`Validator` の aggregate 結果を一度だけ適用します。検証成功時は Attempt が Succeeded、検証失敗時は Failed になります。Provider の失敗や Validator 自体の実行エラーも、失敗した Attempt と Workspace、診断を含む型付き `OrchestratorError` として返します。

Retry / escalation は `RetryPolicy` と `ProviderResolver` を介して Orchestrator が制御します。`execute_decision` は PlannerDecision の意図だけを受け取り、Attempt ID、状態遷移、Provider 解決、Attempt ごとの Workspace 作成は Rust 側で行います。`max_attempts` は新しい Attempt を追加する前に検査され、終端 Task への追加は拒否されます。timeout / cancellation の retry は policy で明示的に許可した場合だけ可能です。`ProviderRegistry` は複数 Provider の本番配線と fake 差し替えに利用できます。

Planner 経路では `ExecutionPolicy` と `execute_decision_with_policy` を使用してください。この hard gate は `max_attempts`、ProviderRegistry 解決、Provider 別 timeout、Provider availability を、Task / Attempt / workspace の変更より前に検査します。timeout は caller から受け取らず、policy に設定された値だけが ProviderRequest に渡されます。availability check は全 AgentProvider の必須境界で、未実装 Provider は fail-closed になります。

Issue #30 の1 Attemptフローは、検証成功時も Task を Active のまま返します。Task全体の完了判断は後続の実行ポリシーで確定します。作成した worktree は Orchestrator が自動削除しないため、結果や未コミット変更を確認した後に `WorkspaceManager::cleanup`（または明示的な `cleanup_force`）を呼び出します。

手動で実 Agent を呼ぶ Live Provider Test は通常のテストには含めません。Codex CLI の導入と認証を確認したうえで、変更してよい隔離 workspace を指定して次を実行します。

```shell
codex --version
codex login status
CODEX_PROVIDER_LIVE_WORKSPACE=/tmp/codex-provider-live \
  cargo test --test live_codex_provider -- --ignored --nocapture
```

`CODEX_PROVIDER_LIVE_WORKSPACE` は事前に作成した、Codex が変更してよい Git workspace に置き換えてください。このテストは API 利用枠と実行時間を消費するため、Pull Request の通常 CI では実行しません。

### GitHub Copilot CLI Provider

`CopilotProvider` は `copilot -p` を非対話で起動し、`ProviderRequest` の workspace を cwd として使用します。named Modelなら `--model <model>` を渡し、`ProviderDefault`ならModel引数を渡さずCopilot CLI設定または既定値を使います。実行時には `-s --no-ask-user` と、既定でファイル変更・リポジトリ操作を許可する `--allow-tool=write,shell` も付けます。silent出力は実Model表示を抑制するため、observed Modelはunknownです。必要な権限だけに絞る場合は `with_allowed_tools` を使用してください。timeout / cancellation は `execute_with_cancellation` から指定できます。

`ExecutionPolicy::new` と `with_timeout` は `Result` を返し、ゼロ値を構築時に拒否します。Planner 経路では `execute_validated_decision_with_policy` を通常入口として使い、`execute_decision_with_policy` は既存利用者向けの legacy 互換 API です。

手動で実 Agent を呼ぶ Live Provider Test の手順は、[GitHub Copilot CLI Provider](docs/copilot-provider.md) を参照してください。

Antigravity CLI (`agy`) の headless Provider と手動 Live Provider Test の手順は、[Antigravity CLI Provider](docs/antigravity-provider.md) を参照してください。

## アーキテクチャ

主要コンポーネントの構造と、Codex、Rust Orchestrator、Provider、Validator の責務境界は、[初期アーキテクチャ](docs/architecture.md)を参照してください。

Provider / Model 選定へ渡す Task、利用状況、過去実績、Attempt 履歴と、role ごとの選定結果の構造は、[モデル選定の入力・出力仕様](docs/model-selection-spec.md)を参照してください。

同じ開発作業で実装、レビュー、修正を行うときの記録方法と、完了にする判断は、
[実装・レビュー・修正を記録する設計](docs/implementation-review-model.md)を参照してください。

`Attempt` をモデル呼び出し1回の記録として扱い、機械検証、AI review verdict、監督Codexの受入、
公開/CI、Task完了を成果物へ束縛する別の事実として扱うDomain契約は、
[作業・モデル実行・成果物のドメインモデル](docs/domain-model.md)を参照してください。

監督Codexが依頼できる操作と返る事実は、[MCP 操作契約の概要](docs/mcp-operation-contract.md)を参照してください。
tool schema、冪等性、ページング、取消・エラー、secret / logの扱いは、同ページから[実装者向け詳細仕様](docs/mcp-operation-contract-reference.md)を参照できます。

## コード品質・テスト

Rustコードに適用する必須検証、テスト種別、unsafeの扱いは、[Rustコード品質・テスト方針](docs/rust-quality.md)を参照してください。

## Execution Ledger

ローカル実行履歴は `SqliteExecutionLedger` に保存できます。`open(path)` は SQLite
ファイルを開いて schema を初期化し、`open_in_memory()` は隔離された ledger を作成します。
Task を `save_task` で保存した後、各 Attempt を `save_attempt` で保存してください。
retry は別の Attempt ID として追加されます。

## GitHub Workflow

以下も現行GitHubWorkflowの挙動であり、MCP Operation Serviceの公開条件ではありません。MCP契約の`publication.publish`と`task.finish`は、対象Artifact、accepted CodexDecision、policy必須の証拠を別途照合します。

`GitHubWorkflow` publishes only an `OrchestrationReport` whose aggregate
`ValidationResult` passed. Commit, push, and pull-request effects are injected
through fakeable traits and persisted as phases in the `SqliteExecutionLedger`,
so interrupted publication resumes idempotently. See
[`docs/github-workflow.md`](docs/github-workflow.md).

## Rust環境の準備

[rustup](https://rustup.rs/)の案内に従ってRustをインストールしてください。このリポジトリでは`rust-toolchain.toml`によりRust `1.88.0`と`rustfmt`、`clippy`を固定しています。リポジトリ直下でCargoコマンドを実行すると、必要なtoolchainとcomponentが自動的に選択されます。

## ローカル検証

Pull Requestを作成する前に、次のコマンドを実行してください。

```shell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

## 継続的インテグレーション

Pull RequestではGitHub Actionsの`Rust CI` workflowが起動し、ローカル検証と同じ3つのコマンドをFormat、Clippy、Testの個別jobとして実行します。いずれかのjobが失敗すると、workflow全体も失敗します。

## 開発に参加する方へ

Issue をもとに実装する前に、[`AGENTS.md`](AGENTS.md) を確認してください。`AGENTS.md` には、AI エージェントを含む実装担当者が従う変更範囲、設計変更、検証、ドキュメント更新のルールと、プロジェクト共通の完了条件を記載しています。

Issue を作成する際は、内容に対応するテンプレートを使用し、完了条件と対応範囲を明確にしてください。

- [機能追加](.github/ISSUE_TEMPLATE/feature.yml)
- [不具合報告](.github/ISSUE_TEMPLATE/bug.yml)
- [設計 / RFC](.github/ISSUE_TEMPLATE/design.yml)

Pull Request を作成する際は、[Pull Request テンプレート](.github/pull_request_template.md)に沿って、概要、関連 Issue、変更内容と判断理由、完了条件への対応、GitHub Actions 以外の追加検証、設計・セキュリティへの影響、未解決事項を記載してください。

Pull Request のbase、stacked依存、必須記載、変更規模、マージ後の`main`到達確認は、[Pull Request Policy](docs/pr-policy.md)に従って確認します。客観的な条件はPolicy as Codeで検査し、変更目的や粒度の妥当性はCodexまたはレビュー担当者が判断します。
