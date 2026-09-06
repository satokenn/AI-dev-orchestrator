# AI-dev-orchestrator

AI エージェントを活用した開発オーケストレーションのためのプロジェクトです。

## 外部 Agent CLI の実行

外部 CLI は `ProcessRequest` に command、引数配列、作業ディレクトリ、環境変数、タイムアウトを指定し、`ProcessRunner` で実行できます。引数は shell 文字列へ連結されず、stdout / stderr と終了状態が `ProcessOutput` に集約されます。長時間実行を停止する場合は `CancellationToken` を渡して `cancel()` を呼び出してください。

## AgentProvider 契約

Agent 実行先の違いは `AgentProvider` に閉じ込めます。実装は `ProviderRequest` の workspace、prompt、timeout を受け取り、`ProviderResult` の stdout、stderr、終了状態、任意の `AgentResult` と `UsageCost` を返します。実行に失敗した場合は `ProviderError`（不正な要求、実行失敗、タイムアウト、利用不能）を返します。

Provider の識別子は `ProviderRef` で表し、Provider 固有の CLI 引数やセッション情報は共通契約に含めません。

## Codex CLI Provider

`CodexProvider` は `codex exec` を非対話モードで起動し、`ProviderRequest` の workspace を cwd として使用します。Codex CLI は `PATH` から解決され、実行時には workspace への書き込みを許可する `--sandbox workspace-write`、JSONL 出力の `--json`、実行状態を永続化しない `--ephemeral` を付けます。長時間実行は `execute_with_cancellation` に `CancellationToken` を渡して停止できます。

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

手動で実 Agent を呼ぶ Live Provider Test の手順は、[GitHub Copilot CLI Provider](docs/copilot-provider.md) を参照してください。

Antigravity CLI (`agy`) の headless Provider と手動 Live Provider Test の手順は、[Antigravity CLI Provider](docs/antigravity-provider.md) を参照してください。

## アーキテクチャ

主要コンポーネントの構造と、Codex、Rust Orchestrator、Provider、Validator の責務境界は、[初期アーキテクチャ](docs/architecture.md)を参照してください。

## コード品質・テスト

Rustコードに適用する必須検証、テスト種別、unsafeの扱いは、[Rustコード品質・テスト方針](docs/rust-quality.md)を参照してください。

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
