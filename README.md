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

## AgentProvider 契約

Agent 実行先の違いは `AgentProvider` に閉じ込めます。実装は `ProviderRequest` の workspace、prompt、timeout を受け取り、`ProviderResult` の stdout、stderr、終了状態、任意の `AgentResult` と `UsageCost` を返します。実行に失敗した場合は `ProviderError`（不正な要求、実行失敗、タイムアウト、利用不能）を返します。

Provider の識別子は `ProviderRef` で表し、Provider 固有の CLI 引数やセッション情報は共通契約に含めません。

## WorkspaceManager

`WorkspaceManager` は Git リポジトリの root を解決し、リポジトリ外の管理ディレクトリに Task / Attempt ごとの専用 branch と Git worktree を作成します。Provider を実行する前に `validate_provider_workspace`（または `ensure_provider_workspace`）で実行先を検証してください。main の working tree や、Manager が作成していないパスは拒否されます。

`cleanup`/`remove` は非 force で専用 worktree を削除します。未コミットの変更がある場合は型付き Git error を返し、worktree と内容を保持します。破棄が必要な場合だけ `cleanup_force` を明示的に呼び出してください。branch は agent のコミットを後続処理で確認できるよう保持されます。branch の merge や PR 作成は WorkspaceManager の責務ではありません。

## Codex CLI Provider

`CodexProvider` は `codex exec` を非対話モードで起動し、`ProviderRequest` の workspace を cwd として使用します。Codex CLI は `PATH` から解決され、実行時には workspace への書き込みを許可する `--sandbox workspace-write`、JSONL 出力の `--json`、実行状態を永続化しない `--ephemeral` を付けます。長時間実行は `execute_with_cancellation` に `CancellationToken` を渡して停止できます。

## Codex Planner

`CodexPlanner` は `codex exec` の `--output-schema` と `--output-last-message` を使って、Task の内容と Rust が観測した `ProviderAvailability` 一覧から、`PlannerDecision`（provider、reason、execution intent）を読み取ります。Planner は Task を不変借用するだけで、状態を変更しません。`PlannerService` が決定を `ValidatedPlannerDecision` に変換する前に、未知または利用不能な Provider を Rust 側で拒否します。Planner は `--sandbox read-only` と `--ephemeral` で実行され、schema と出力の一時ファイルは処理後に削除されます。

## Validator

`RustValidator` は明示された workspace を cwd として、`cargo fmt`、`cargo clippy`、`cargo test` の機械的なチェックを順番に実行します。全チェックを内包した aggregate の `ValidationResult` を1件返し、各コマンドの終了状態と stdout / stderr の診断は `ValidationResult::checks()` から参照できます。1つでも失敗した場合は aggregate を成功として扱いません。`CommandValidator` と `ValidationCheck` を使えば、同じ `Validator` API で決定的なチェック列も構成できます。

## Orchestrator Service

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

`CopilotProvider` は `copilot -p` を非対話で起動し、`ProviderRequest` の workspace を cwd として使用します。実行時には `-s --no-ask-user` と、既定でファイル変更・リポジトリ操作を許可する `--allow-tool=write,shell` を付けます。必要な権限だけに絞る場合は `with_allowed_tools` を使用してください。timeout / cancellation は `execute_with_cancellation` から指定できます。

`ExecutionPolicy::new` と `with_timeout` は `Result` を返し、ゼロ値を構築時に拒否します。Planner 経路では `execute_validated_decision_with_policy` を通常入口として使い、`execute_decision_with_policy` は既存利用者向けの legacy 互換 API です。

手動で実 Agent を呼ぶ Live Provider Test の手順は、[GitHub Copilot CLI Provider](docs/copilot-provider.md) を参照してください。

Antigravity CLI (`agy`) の headless Provider と手動 Live Provider Test の手順は、[Antigravity CLI Provider](docs/antigravity-provider.md) を参照してください。

## アーキテクチャ

主要コンポーネントの構造と、Codex、Rust Orchestrator、Provider、Validator の責務境界は、[初期アーキテクチャ](docs/architecture.md)を参照してください。

## コード品質・テスト

Rustコードに適用する必須検証、テスト種別、unsafeの扱いは、[Rustコード品質・テスト方針](docs/rust-quality.md)を参照してください。

## Execution Ledger

ローカル実行履歴は `SqliteExecutionLedger` に保存できます。`open(path)` は SQLite
ファイルを開いて schema を初期化し、`open_in_memory()` は隔離された ledger を作成します。
Task を `save_task` で保存した後、各 Attempt を `save_attempt` で保存してください。
retry は別の Attempt ID として追加されます。

## GitHub Workflow

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
