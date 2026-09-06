# Live E2E runbook

通常CIは `fake_e2e` のみを実行し、外部CLI・ネットワーク・課金を発生させない。`live_e2e` は `#[ignore]` であり、実providerは承認済みの隔離環境で明示的に実行する。

この操作は実際のprovider利用枠を消費し、実workspace上で変更を作成して commit・push し、GitHubに実PRを作成する。費用・利用枠・公開範囲を確認し、使い捨てのIssue/repositoryだけで実行すること。実live実行を避ける場合は `cargo test --test live_e2e --no-run` と通常の `cargo test` を使う。

実行前に次を確認する。

- 利用枠・費用上限を確認し、短いテストIssueだけを使う。実行時間はRustの実行ポリシーとCLI timeoutに合わせる。
- `LIVE_E2E=1`、`LIVE_PROVIDER=codex|copilot|antigravity`、`LIVE_REPOSITORY_ROOT=/path/to/disposable-fixture-repository`、`LIVE_CREDENTIALS=1` を設定する。
- `LIVE_PUBLISH=1`、`LIVE_REPOSITORY=owner/name`、`LIVE_ISSUE_NUMBER=N`、`LIVE_LEDGER=/path/to/isolated-ledger.sqlite3` も必須。`LIVE_PUBLISH=1` は commit/push/PR の実行を明示的に許可するゲートである。
- repository rootは変更してよい使い捨てfixtureのmain worktree。テストがその配下にmanaged worktreeを作成し、providerへ渡す。認証は選択したproviderのCLIで事前に済ませる。
- 完了後にPR/ブランチを確認し、worktreeをcleanupする。失敗時も診断を保存してから `git worktree remove --force` で隔離workspaceだけを削除する。

実行コマンド:

```sh
./scripts/run-live-e2e.sh
```

ゲート不足、CLI/credential不足、workspace不在、またはprovider実行・検証・publicationの失敗は成功扱いにせず、理由付きで失敗する。認証tokenやprovider stdoutはログへ出力せず、失敗時の診断は隔離worktreeのパスだけを示す。成功・失敗の全経路でmanaged worktreeを強制cleanupする。
