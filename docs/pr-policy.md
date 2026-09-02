# Pull Request Policy

この文書は、Pull Requestのマージ先、依存関係、必須記載、変更規模、マージ後のdefault branchへの到達を検査するPolicy as Codeの運用を定義します。検査の正本は[`.github/pr-policy.json`](../.github/pr-policy.json)です。

## 判定の境界

機械検査は、GitHubとPull Request本文から客観的に確認できる条件だけを合否判定します。

| 判定対象 | 扱い |
| --- | --- |
| 実際のbaseと本文の`Base`が一致する | 違反時に失敗 |
| default branch以外をbaseにする | 有効なstacked依存がなければ失敗 |
| stacked依存先がopenで、head branchがbaseと一致する | 違反時に失敗 |
| 必須sectionとPR判定fieldが記載されている | 違反時に失敗 |
| 差分が存在し、未完了の完了条件がない | 違反時に失敗 |
| GitHubが競合ありと判定している | 失敗 |
| ファイル数、変更行数、参照Issue数が閾値を超える | 警告 |
| 変更が1つの目的にまとまっている | Codexまたは人間が意味的に判断 |
| Issueの要求を実装内容が満たす | Validatorの結果を基にCodexまたは人間が判断 |

警告はPRを自動的に拒否しません。PR本文の`Purpose`、`Scope decision`、`Excluded`と実際の差分を比較し、分割が必要か判断してください。ファイル数や変更行数だけを根拠に粒度を決定しません。

## 通常のPR

通常は`main`をbaseとし、PR本文へ次の情報を記録します。

```markdown
## PR判定

- Base: main
- Depends on: none
- Purpose: このPRが達成する1つの目的
- Scope decision: 同じPRへ含める変更と、その理由
- Excluded: 今回は含めない変更
```

## Stacked PR

未マージPRの変更がなければ子PRをレビュー・検証できない場合だけ、stacked PRを使用できます。`Base`には親PRのhead branch、`Depends on`には親PR番号を1つ記載します。

```markdown
- Base: agent/parent-change
- Depends on: #123
```

機械検査は、依存PRがopenであり、そのhead branchが実際のbaseと一致することをGitHub APIで確認します。親PRがmergeまたはcloseされた後は検査に失敗するため、子PRのbaseを`main`へ変更し、本文と差分を更新してください。

## ローカル検査

認証済みのGitHub CLIを使用し、PR番号を指定します。

```shell
scripts/check-pr-policy.sh <pull-request-number>
```

別repositoryを検査する場合は、`owner/repository`も指定できます。

```shell
scripts/check-pr-policy.sh <pull-request-number> <owner/repository>
```

検査は`PASS`、`WARN`、`FAIL`を出力し、`FAIL`が1件以上あれば終了status `1`を返します。入力や設定を解釈できない場合は終了status `2`です。

## GitHub Actions

[`pr-policy.yml`](../.github/workflows/pr-policy.yml)は、PRの作成、本文編集、commit追加、reopen、Ready for reviewへの変更時に同じ検査を実行します。Workflowの権限は`contents: read`と`pull-requests: read`に限定しています。

[`post-merge-policy.yml`](../.github/workflows/post-merge-policy.yml)は、mergeされたPRの`merge_commit_sha`が、checkoutしたdefault branchから到達可能であることを確認します。merge commit、squash、rebaseの各方式でGitHub APIが返すmerge後のSHAを使用します。

`pull_request_target`を利用するpost-merge workflowでは、PRのheadをcheckoutまたは実行しません。default branchの検査コードだけを実行します。

## Required Checkの同期

Rulesetの宣言は[`.github/rulesets/main-pr-policy.json`](../.github/rulesets/main-pr-policy.json)で管理します。Policy workflowが`main`へmergeされ、`PR Policy` checkが一度GitHub上で実行された後、repository管理権限を持つ認証済みGitHub CLIで同期します。

適用内容を先に確認します。

```shell
scripts/sync-pr-ruleset.sh --dry-run
```

確認後に適用します。

```shell
scripts/sync-pr-ruleset.sh
```

このRulesetは`main`に対して`PR Policy`をRequired Checkにし、baseが最新であることも要求します。Ruleset同期はrepository設定を変更するため、PR上の未mergeファイルから自動実行しません。

## 安全上の制約

- Workflow tokenは読み取り権限だけを使用する。
- 外部Actionは完全なcommit SHAへ固定する。
- `pull_request_target`でPR headのコードをcheckoutまたは実行しない。
- Rulesetの同期は管理者が差分をレビューし、Policy workflowのmerge後に実行する。
- 機械検査の成功を、目的・設計・PR粒度の意味的な妥当性と同一視しない。

## 参考資料

- [Available rules for rulesets](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)
- [Events that trigger workflows](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows)
- [Secure use reference](https://docs.github.com/en/actions/reference/security/secure-use)
- [REST API endpoints for pull requests](https://docs.github.com/en/rest/pulls/pulls)
