#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 <pull-request-number> [owner/repository]" >&2
}

if [[ $# -lt 1 || $# -gt 2 ]]; then
  usage
  exit 2
fi

pull_number=$1
repository=${2:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd "$script_dir/.." && pwd)
event_file=$(mktemp)
trap 'rm -f "$event_file"' EXIT

gh api "repos/$repository/pulls/$pull_number" >"$event_file"
GITHUB_TOKEN=${GITHUB_TOKEN:-$(gh auth token)} \
  python3 "$script_dir/pr_policy.py" preflight \
    --event "$event_file" \
    --config "$repository_root/.github/pr-policy.json"
