#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 [--dry-run] [owner/repository]" >&2
}

dry_run=false
if [[ ${1:-} == "--dry-run" ]]; then
  dry_run=true
  shift
fi
if [[ $# -gt 1 ]]; then
  usage
  exit 2
fi

repository=${1:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd "$script_dir/.." && pwd)
ruleset_file="$repository_root/.github/rulesets/main-pr-policy.json"
ruleset_name=$(jq -r .name "$ruleset_file")
ruleset_id=$(gh api "repos/$repository/rulesets" --paginate --jq ".[] | select(.name == \"$ruleset_name\") | .id" | head -n 1)

if [[ -n $ruleset_id ]]; then
  method=PUT
  endpoint="repos/$repository/rulesets/$ruleset_id"
  action=update
else
  method=POST
  endpoint="repos/$repository/rulesets"
  action=create
fi

if $dry_run; then
  echo "Would $action ruleset '$ruleset_name' in $repository via $method $endpoint"
  jq . "$ruleset_file"
  exit 0
fi

gh api --method "$method" "$endpoint" --input "$ruleset_file" >/dev/null
echo "Ruleset '$ruleset_name' synchronized in $repository"
