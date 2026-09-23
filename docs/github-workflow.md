# GitHub Issue publication

`GitHubWorkflow::publish` accepts only a `ValidatedPublication`, which can be
prepared from an `OrchestrationReport` when its aggregate `ValidationResult` is
passed. It calls fakeable `RepositoryEffects` in the order commit, push, and
`PullRequestGateway::create`. Failures stop the remaining effects.

`GhPullRequestGateway` implements `create` as an idempotent create-or-get
operation. Before creating anything it runs `gh pr list --state all` for the
same repository, head, and base (limited to one result). An existing URL is
returned directly; only an empty result invokes `gh pr create`. The executable
can be injected with `GhPullRequestGateway::with_executable` for deterministic
tests. Non-zero commands and malformed list JSON are returned as typed command
errors.

Publication records are stored in the same SQLite database as execution history.
The idempotency key is `<repository>#<issue number>`. A retry resumes from the
stored phase: a committed record pushes only, a pushed record invokes the
gateway, and a published record returns `AlreadyPublished`. If saving the
published record fails after GitHub has accepted the PR, a retry invokes the
gateway again; its list-before-create contract finds and returns the existing
PR, so no duplicate is created.

The `Gh*` adapters use argument arrays and fixed subcommands; tests should use
the traits instead of performing external GitHub or repository operations.
