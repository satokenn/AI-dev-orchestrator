#!/usr/bin/env python3
"""Deterministic pull request policy checks for local use and GitHub Actions."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Callable


@dataclass(frozen=True)
class Finding:
    level: str
    code: str
    message: str


@dataclass(frozen=True)
class Report:
    findings: list[Finding]

    @property
    def failed(self) -> bool:
        return any(finding.level == "failure" for finding in self.findings)


def _finding(level: str, code: str, message: str) -> Finding:
    return Finding(level=level, code=code, message=message)


def _load_json(path: str | Path) -> dict[str, Any]:
    with Path(path).open(encoding="utf-8") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def _pull_request(payload: dict[str, Any]) -> tuple[dict[str, Any], str]:
    pull_request = payload.get("pull_request", payload)
    if not isinstance(pull_request, dict):
        raise ValueError("event does not contain a pull request object")

    repository = payload.get("repository", {})
    repository_name = repository.get("full_name") if isinstance(repository, dict) else None
    if not repository_name:
        base = pull_request.get("base", {})
        base_repository = base.get("repo", {}) if isinstance(base, dict) else {}
        repository_name = base_repository.get("full_name")
    if not repository_name:
        raise ValueError("event does not identify the repository")
    return pull_request, str(repository_name)


def _markdown_sections(body: str) -> dict[str, str]:
    matches = list(re.finditer(r"(?m)^##\s+(.+?)\s*$", body))
    sections: dict[str, str] = {}
    for index, match in enumerate(matches):
        start = match.end()
        end = matches[index + 1].start() if index + 1 < len(matches) else len(body)
        content = re.sub(r"<!--.*?-->", "", body[start:end], flags=re.DOTALL).strip()
        sections[match.group(1).strip()] = content
    return sections


def _decision_fields(section: str) -> dict[str, str]:
    fields: dict[str, str] = {}
    for match in re.finditer(r"(?m)^-\s+([^:]+):\s*(.*?)\s*$", section):
        fields[match.group(1).strip()] = match.group(2).strip().strip("`")
    return fields


def _dependency_number(value: str) -> int | None:
    match = re.fullmatch(r"#(\d+)", value.strip())
    return int(match.group(1)) if match else None


def _is_none(value: str) -> bool:
    return value.strip().lower() in {"none", "なし", "n/a"}


def _github_pull_request(
    repository: str,
    number: int,
    api_url: str,
    token: str | None,
) -> dict[str, Any]:
    if not token:
        raise RuntimeError("GITHUB_TOKEN is required to validate a stacked pull request")
    request = urllib.request.Request(
        f"{api_url.rstrip('/')}/repos/{repository}/pulls/{number}",
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            value = json.load(response)
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"GitHub returned HTTP {error.code} for dependency #{number}") from error
    except urllib.error.URLError as error:
        raise RuntimeError(f"could not query dependency #{number}: {error.reason}") from error
    if not isinstance(value, dict):
        raise RuntimeError(f"GitHub returned an invalid response for dependency #{number}")
    return value


def validate_preflight(
    payload: dict[str, Any],
    config: dict[str, Any],
    fetch_pull_request: Callable[[str, int], dict[str, Any]],
) -> Report:
    findings: list[Finding] = []
    pull_request, repository = _pull_request(payload)
    body = str(pull_request.get("body") or "")
    sections = _markdown_sections(body)
    required_sections = config.get("required_sections", [])

    for section_name in required_sections:
        if not sections.get(str(section_name), "").strip():
            findings.append(
                _finding("failure", "missing-section", f"section '{section_name}' is missing or empty")
            )

    decision_section = sections.get("PR判定", "")
    fields = _decision_fields(decision_section)
    for field_name in config.get("decision_fields", []):
        if not fields.get(str(field_name), "").strip():
            findings.append(
                _finding("failure", "missing-decision", f"PR判定 field '{field_name}' is missing or empty")
            )

    base = str(pull_request.get("base", {}).get("ref") or "")
    declared_base = fields.get("Base", "")
    if declared_base and declared_base != base:
        findings.append(
            _finding(
                "failure",
                "base-mismatch",
                f"declared base '{declared_base}' does not match actual base '{base}'",
            )
        )

    default_base = str(config.get("default_base") or "main")
    depends_on = fields.get("Depends on", "")
    dependency_number = _dependency_number(depends_on)
    stacked_allowed = bool(config.get("stacked_pull_requests", {}).get("allowed", False))

    if base == default_base:
        if depends_on and not _is_none(depends_on):
            findings.append(
                _finding(
                    "failure",
                    "unexpected-dependency",
                    f"a '{default_base}'-based pull request must declare 'Depends on: none'",
                )
            )
        else:
            findings.append(_finding("pass", "base", f"base is the default branch '{default_base}'"))
    elif not stacked_allowed:
        findings.append(_finding("failure", "base", f"base '{base}' is not allowed"))
    elif dependency_number is None:
        findings.append(
            _finding(
                "failure",
                "stacked-dependency",
                "a non-default base requires exactly one dependency in the form '#123'",
            )
        )
    else:
        try:
            dependency = fetch_pull_request(repository, dependency_number)
        except RuntimeError as error:
            findings.append(_finding("failure", "dependency-query", str(error)))
        else:
            dependency_head = str(dependency.get("head", {}).get("ref") or "")
            dependency_open = dependency.get("state") == "open" and not dependency.get("merged_at")
            if not dependency_open:
                findings.append(
                    _finding(
                        "failure",
                        "closed-dependency",
                        f"dependency #{dependency_number} is closed or merged",
                    )
                )
            elif dependency_head != base:
                findings.append(
                    _finding(
                        "failure",
                        "dependency-base-mismatch",
                        f"dependency #{dependency_number} head '{dependency_head}' does not match base '{base}'",
                    )
                )
            else:
                findings.append(
                    _finding(
                        "pass",
                        "stacked-dependency",
                        f"base '{base}' is the open dependency #{dependency_number}",
                    )
                )

    changed_files = int(pull_request.get("changed_files") or 0)
    changed_lines = int(pull_request.get("additions") or 0) + int(pull_request.get("deletions") or 0)
    if changed_files == 0:
        findings.append(_finding("failure", "empty-diff", "pull request has no changed files"))
    else:
        findings.append(_finding("pass", "diff", f"pull request changes {changed_files} file(s)"))

    if pull_request.get("mergeable") is False:
        findings.append(_finding("failure", "conflict", "GitHub reports that the pull request conflicts"))
    elif pull_request.get("mergeable") is None:
        findings.append(_finding("warning", "mergeability-pending", "GitHub has not computed mergeability yet"))
    else:
        findings.append(_finding("pass", "mergeability", "GitHub reports that the pull request is mergeable"))

    acceptance = sections.get("Issue の完了条件", "")
    if re.search(r"(?m)^\s*-\s*\[\s\]", acceptance):
        findings.append(
            _finding("failure", "incomplete-criteria", "Issue acceptance criteria contain unchecked items")
        )
    elif acceptance and not re.search(r"(?m)^\s*-\s*\[[xX]\]", acceptance):
        findings.append(
            _finding("failure", "missing-criteria", "Issue acceptance criteria contain no checked items")
        )

    thresholds = config.get("warning_thresholds", {})
    file_threshold = int(thresholds.get("changed_files") or 0)
    line_threshold = int(thresholds.get("changed_lines") or 0)
    issue_threshold = int(thresholds.get("referenced_issues") or 0)
    if file_threshold and changed_files > file_threshold:
        findings.append(
            _finding(
                "warning",
                "file-count",
                f"{changed_files} changed files exceed the review threshold of {file_threshold}",
            )
        )
    if line_threshold and changed_lines > line_threshold:
        findings.append(
            _finding(
                "warning",
                "line-count",
                f"{changed_lines} changed lines exceed the review threshold of {line_threshold}",
            )
        )

    issue_references = set(re.findall(r"(?<![\w/])#(\d+)\b", body))
    if issue_threshold and len(issue_references) > issue_threshold:
        findings.append(
            _finding(
                "warning",
                "issue-count",
                f"PR body references {len(issue_references)} issues; confirm that it has one purpose",
            )
        )

    semantic_fields = ("Purpose", "Scope decision", "Excluded")
    if all(fields.get(name, "").strip() for name in semantic_fields):
        findings.append(
            _finding(
                "pass",
                "semantic-attestation",
                "purpose, scope decision, and exclusions are recorded for semantic review",
            )
        )

    return Report(findings=findings)


def validate_post_merge(
    payload: dict[str, Any],
    config: dict[str, Any],
    repository_path: str | Path,
) -> Report:
    findings: list[Finding] = []
    pull_request, _ = _pull_request(payload)
    if pull_request.get("merged") is not True:
        return Report([_finding("failure", "not-merged", "pull request is not marked as merged")])

    base = str(pull_request.get("base", {}).get("ref") or "")
    default_base = str(config.get("default_base") or "main")
    if base != default_base:
        findings.append(
            _finding(
                "failure",
                "not-default-base",
                f"pull request was merged into '{base}', not default branch '{default_base}'",
            )
        )
        return Report(findings)

    merge_sha = str(pull_request.get("merge_commit_sha") or "")
    if not re.fullmatch(r"[0-9a-fA-F]{40}", merge_sha):
        return Report([_finding("failure", "invalid-merge-sha", "merge_commit_sha is missing or invalid")])

    repository_path = str(repository_path)
    exists = subprocess.run(
        ["git", "-C", repository_path, "cat-file", "-e", f"{merge_sha}^{{commit}}"],
        check=False,
        capture_output=True,
        text=True,
    )
    if exists.returncode != 0:
        return Report([_finding("failure", "missing-merge-sha", f"commit {merge_sha} is not available")])

    reachable = subprocess.run(
        ["git", "-C", repository_path, "merge-base", "--is-ancestor", merge_sha, "HEAD"],
        check=False,
        capture_output=True,
        text=True,
    )
    if reachable.returncode == 0:
        findings.append(
            _finding(
                "pass",
                "default-reachability",
                f"merge commit {merge_sha} is reachable from checked-out '{default_base}'",
            )
        )
    else:
        findings.append(
            _finding(
                "failure",
                "default-reachability",
                f"merge commit {merge_sha} is not reachable from checked-out '{default_base}'",
            )
        )
    return Report(findings)


def _render(report: Report) -> str:
    labels = {"pass": "PASS", "warning": "WARN", "failure": "FAIL"}
    return "\n".join(
        f"{labels[finding.level]} [{finding.code}]: {finding.message}" for finding in report.findings
    )


def _write_outputs(report: Report, output_path: str | None) -> None:
    rendered = _render(report)
    print(rendered)
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_path:
        with Path(summary_path).open("a", encoding="utf-8") as handle:
            handle.write("## Pull Request Policy\n\n```text\n")
            handle.write(rendered)
            handle.write("\n```\n")
    if output_path:
        with Path(output_path).open("w", encoding="utf-8") as handle:
            json.dump(
                {"failed": report.failed, "findings": [asdict(finding) for finding in report.findings]},
                handle,
                ensure_ascii=False,
                indent=2,
            )
            handle.write("\n")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    preflight = subparsers.add_parser("preflight", help="validate a pull request event or API response")
    preflight.add_argument("--event", required=True, help="path to GitHub event or pull request JSON")
    preflight.add_argument("--config", required=True, help="path to policy JSON")
    preflight.add_argument("--output", help="optional JSON report path")

    post_merge = subparsers.add_parser("post-merge", help="validate default-branch reachability")
    post_merge.add_argument("--event", required=True, help="path to a merged pull request event JSON")
    post_merge.add_argument("--config", required=True, help="path to policy JSON")
    post_merge.add_argument("--repository-path", default=".", help="checked-out default branch")
    post_merge.add_argument("--output", help="optional JSON report path")
    return parser


def main() -> int:
    args = _parser().parse_args()
    try:
        payload = _load_json(args.event)
        config = _load_json(args.config)
        if config.get("schema_version") != 1:
            raise ValueError("unsupported policy schema_version")

        if args.command == "preflight":
            api_url = os.environ.get("GITHUB_API_URL", "https://api.github.com")
            token = os.environ.get("GITHUB_TOKEN")
            report = validate_preflight(
                payload,
                config,
                lambda repository, number: _github_pull_request(
                    repository, number, api_url=api_url, token=token
                ),
            )
        else:
            report = validate_post_merge(payload, config, args.repository_path)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"FAIL [policy-input]: {error}", file=sys.stderr)
        return 2

    _write_outputs(report, args.output)
    return 1 if report.failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
