#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--no-run" ]]; then
  exec cargo test --test live_e2e --no-run
fi

: "${LIVE_E2E:?Set LIVE_E2E=1 to opt in}"
[[ "$LIVE_E2E" == 1 ]] || { echo "LIVE_E2E must be 1" >&2; exit 2; }
: "${LIVE_PROVIDER:?Choose codex, copilot, or antigravity}"
case "$LIVE_PROVIDER" in codex|copilot|antigravity) ;; *) echo "unsupported LIVE_PROVIDER" >&2; exit 2 ;; esac
: "${LIVE_REPOSITORY_ROOT:?Set the disposable fixture repository main worktree path}"
[[ -d "$LIVE_REPOSITORY_ROOT" ]] || { echo "repository root does not exist" >&2; exit 2; }
: "${LIVE_CREDENTIALS:?Set LIVE_CREDENTIALS=1 only after authenticating}"
[[ "$LIVE_CREDENTIALS" == 1 ]] || { echo "LIVE_CREDENTIALS must be 1" >&2; exit 2; }
: "${LIVE_PUBLISH:?Set LIVE_PUBLISH=1 to permit commit/push/PR publication}"
[[ "$LIVE_PUBLISH" == 1 ]] || { echo "LIVE_PUBLISH must be 1" >&2; exit 2; }
: "${LIVE_REPOSITORY:?Set LIVE_REPOSITORY=owner/name}"
: "${LIVE_ISSUE_NUMBER:?Set LIVE_ISSUE_NUMBER to a disposable test issue}"
: "${LIVE_LEDGER:?Set LIVE_LEDGER to an isolated SQLite ledger path}"
[[ "$LIVE_REPOSITORY" == */* && "$LIVE_REPOSITORY" != */ && "$LIVE_REPOSITORY" != /* ]] || { echo "LIVE_REPOSITORY must be owner/name" >&2; exit 2; }
export LIVE_E2E LIVE_PUBLISH LIVE_PROVIDER LIVE_REPOSITORY_ROOT LIVE_REPOSITORY LIVE_ISSUE_NUMBER LIVE_LEDGER LIVE_CREDENTIALS
cargo test --test live_e2e -- --ignored --nocapture
