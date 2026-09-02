# AI-dev-orchestrator

AI エージェントを活用した開発オーケストレーションのためのプロジェクトです。

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
