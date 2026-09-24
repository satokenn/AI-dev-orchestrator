# Repository-local configuration

Repository-specific mechanical validation is configured in
`.ai-dev-orchestrator/config.toml`. This keeps configuration beside the existing
Ledger and managed-worktree data without renaming or migrating that data. The
library function `init_repository(root)` creates the initial template and refuses
to overwrite an existing config.

## Validation checks

Checks are explicit, structured process invocations. They run in declaration
order, and arguments are passed as an argument array without shell-string
concatenation. A configured cwd is resolved inside the target workspace; parent
traversal and symlinks resolving outside that workspace are rejected before any
check starts. Each timeout is in milliseconds and must be positive. Cancellation
is passed to `ProcessRunner`, which applies the process-group stop contract.

```toml
schema_version = 1

[validation]
checks = [
  { name = "format", command = "cargo", args = ["fmt", "--all", "--", "--check"], cwd = ".", timeout_ms = 60000 },
  { name = "tests", command = "cargo", args = ["test", "--workspace"], cwd = ".", timeout_ms = 300000 },
]
```

An absent config file is an error when loading configuration. An empty or absent
check list is represented by the validator but fails with `NoChecksConfigured`
when invoked; it never produces an unconditional successful validation. Unknown
TOML fields are rejected, so this config format has no credentials or secret
storage field. Keep credentials in the external command's normal credential
mechanism rather than writing them into this file.

The current public Validation result schema does not carry a config/profile
identifier. Consequently this implementation does not claim to bind a validation
result to a config version; that association remains open until the Domain and
Operation result schemas define it.
