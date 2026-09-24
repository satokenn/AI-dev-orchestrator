#!/usr/bin/env python3
"""Read-only comparison of the declared and active main branch ruleset."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any, Callable


ROOT = Path(__file__).resolve().parents[1]
DECLARATION = ROOT / ".github/rulesets/main-pr-policy.json"


def _required_status_checks(ruleset: dict[str, Any]) -> dict[str, Any] | None:
    rules = ruleset.get("rules")
    if not isinstance(rules, list):
        return None
    matches = [
        rule
        for rule in rules
        if isinstance(rule, dict) and rule.get("type") == "required_status_checks"
    ]
    if len(matches) != 1:
        return None
    parameters = matches[0].get("parameters")
    return parameters if isinstance(parameters, dict) else None


def validate_ruleset(declared: dict[str, Any], actual: dict[str, Any]) -> list[str]:
    """Return actionable declaration or live-ruleset mismatches."""
    errors: list[str] = []
    required_contexts = ["PR Policy", "Format", "Clippy", "Test"]

    if declared.get("target") != "branch":
        errors.append("宣言: target は branch である必要があります")
    if declared.get("enforcement") != "active":
        errors.append("宣言: enforcement は active である必要があります")
    if declared.get("conditions") != {
        "ref_name": {"include": ["refs/heads/main"], "exclude": []}
    }:
        errors.append("宣言: 対象branchは refs/heads/main である必要があります")

    declared_parameters = _required_status_checks(declared)
    if declared_parameters is None:
        errors.append("宣言: required_status_checks rule は1つ必要です")
        declared_parameters = {}
        declared_contexts: list[str] = []
    else:
        declared_entries = declared_parameters.get("required_status_checks")
        declared_contexts = (
            [
                entry.get("context")
                for entry in declared_entries
                if isinstance(entry, dict) and isinstance(entry.get("context"), str)
            ]
            if isinstance(declared_entries, list)
            else []
        )
        if len(declared_contexts) != len(set(declared_contexts)):
            errors.append("宣言: required check context が重複しています")
        missing = sorted(set(required_contexts) - set(declared_contexts))
        if missing:
            errors.append(f"宣言: required checks に不足しています: {', '.join(missing)}")
        extra = sorted(set(declared_contexts) - set(required_contexts))
        if extra:
            errors.append(f"宣言: 想定外のrequired checksがあります: {', '.join(extra)}")
        if declared_parameters.get("strict_required_status_checks_policy") is not True:
            errors.append("宣言: strict_required_status_checks_policy は true である必要があります")
        if declared_parameters.get("do_not_enforce_on_create") is not True:
            errors.append("宣言: do_not_enforce_on_create は true である必要があります")

    if actual.get("name") != declared.get("name"):
        errors.append(
            f"有効設定: Ruleset名が不一致です (宣言={declared.get('name')!r}, 実設定={actual.get('name')!r})"
        )
    if actual.get("target") != "branch":
        errors.append("有効設定: target が branch ではありません")
    if actual.get("enforcement") != "active":
        errors.append(
            f"有効設定: enforcement が active ではありません ({actual.get('enforcement')!r})"
        )
    if actual.get("conditions") != declared.get("conditions"):
        errors.append("有効設定: 対象branch条件が宣言と一致しません")

    actual_parameters = _required_status_checks(actual)
    if actual_parameters is None:
        errors.append("有効設定: required_status_checks rule が1つ必要です")
        return errors

    actual_entries = actual_parameters.get("required_status_checks")
    actual_contexts = (
        [
            entry.get("context")
            for entry in actual_entries
            if isinstance(entry, dict) and isinstance(entry.get("context"), str)
        ]
        if isinstance(actual_entries, list)
        else []
    )
    if len(actual_contexts) != len(set(actual_contexts)):
        errors.append("有効設定: required check context が重複しています")
    if sorted(actual_contexts) != sorted(declared_contexts):
        missing = sorted(set(declared_contexts) - set(actual_contexts))
        extra = sorted(set(actual_contexts) - set(declared_contexts))
        details = []
        if missing:
            details.append(f"不足: {', '.join(missing)}")
        if extra:
            details.append(f"余分: {', '.join(extra)}")
        errors.append(
            "有効設定: required check context が宣言と不一致です ("
            + "; ".join(details)
            + ")"
        )
    if actual_parameters.get("strict_required_status_checks_policy") != declared_parameters.get(
        "strict_required_status_checks_policy"
    ):
        errors.append("有効設定: strict_required_status_checks_policy が宣言と不一致です")
    if actual_parameters.get("do_not_enforce_on_create") != declared_parameters.get(
        "do_not_enforce_on_create"
    ):
        errors.append("有効設定: do_not_enforce_on_create が宣言と不一致です")
    return errors


def check_active_ruleset(
    repository: str,
    declared: dict[str, Any],
    api: Callable[[str], Any],
) -> tuple[dict[str, Any] | None, list[str]]:
    """Find the named active ruleset through a read-only API callback."""
    try:
        rulesets = api(f"repos/{repository}/rulesets")
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        return None, [f"GitHub Ruleset一覧を取得できません: {error}"]
    if not isinstance(rulesets, list):
        return None, ["GitHub Ruleset一覧の応答がJSON arrayではありません"]

    matches = [
        ruleset
        for ruleset in rulesets
        if isinstance(ruleset, dict) and ruleset.get("name") == declared.get("name")
    ]
    if len(matches) != 1:
        return None, [
            f"有効Ruleset '{declared.get('name')}' が一意に見つかりません (件数={len(matches)})"
        ]

    ruleset_id = matches[0].get("id")
    if not isinstance(ruleset_id, int):
        return None, ["有効Rulesetに整数のidがありません"]
    try:
        actual = api(f"repos/{repository}/rulesets/{ruleset_id}")
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        return None, [f"GitHub Ruleset詳細を取得できません: {error}"]
    if not isinstance(actual, dict):
        return None, ["GitHub Ruleset詳細の応答がJSON objectではありません"]
    return actual, validate_ruleset(declared, actual)


def _gh_api(endpoint: str) -> Any:
    try:
        result = subprocess.run(
            ["gh", "api", endpoint], check=True, capture_output=True, text=True
        )
    except subprocess.CalledProcessError as error:
        diagnostic = (error.stderr or error.stdout or "").strip()
        raise RuntimeError(diagnostic or f"gh api exited with status {error.returncode}") from error
    return json.loads(result.stdout)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "repository", nargs="?", help="owner/repository (default: current repository)"
    )
    args = parser.parse_args()
    repository = args.repository
    if not repository:
        try:
            result = subprocess.run(
                ["gh", "repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"],
                check=True,
                capture_output=True,
                text=True,
            )
            repository = result.stdout.strip()
        except (OSError, subprocess.SubprocessError) as error:
            diagnostic = getattr(error, "stderr", None) or str(error)
            print(f"FAIL: current repositoryを特定できません: {diagnostic.strip()}", file=sys.stderr)
            return 2

    try:
        declared = json.loads(DECLARATION.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        print(f"FAIL: Ruleset宣言を読み込めません: {error}", file=sys.stderr)
        return 2

    actual, errors = check_active_ruleset(repository, declared, _gh_api)
    if errors:
        for error in errors:
            print(f"FAIL: {error}", file=sys.stderr)
        if actual is not None:
            print(
                f"確認対象: {repository} / {actual.get('name')} (id={actual.get('id')})",
                file=sys.stderr,
            )
        lookup_failed = any(
            "取得できません" in error or "応答が" in error for error in errors
        )
        return 2 if actual is None and lookup_failed else 1
    print(f"PASS: {repository} の有効Rulesetは宣言と一致します (id={actual.get('id')})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
