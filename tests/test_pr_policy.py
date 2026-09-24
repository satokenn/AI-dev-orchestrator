from __future__ import annotations

import copy
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

from scripts.pr_policy import validate_post_merge, validate_preflight


ROOT = Path(__file__).resolve().parents[1]
CONFIG = json.loads((ROOT / ".github/pr-policy.json").read_text(encoding="utf-8"))
VALID_EVENT = json.loads((ROOT / "tests/fixtures/pr-valid.json").read_text(encoding="utf-8"))
POST_MERGE_EVENT = json.loads(
    (ROOT / "tests/fixtures/post-merge.json").read_text(encoding="utf-8")
)


def codes(report, level: str) -> set[str]:
    return {finding.code for finding in report.findings if finding.level == level}


class PreflightTests(unittest.TestCase):
    def validate(self, event, dependency=None):
        def fetch(_repository: str, _number: int):
            if dependency is None:
                raise AssertionError("unexpected dependency lookup")
            return dependency

        return validate_preflight(event, CONFIG, fetch)

    def test_valid_main_pull_request_passes(self):
        report = self.validate(copy.deepcopy(VALID_EVENT))
        self.assertFalse(report.failed)
        self.assertIn("base", codes(report, "pass"))

    def test_missing_decision_field_fails(self):
        event = copy.deepcopy(VALID_EVENT)
        event["pull_request"]["body"] = event["pull_request"]["body"].replace(
            "- Purpose: PR policyを機械的に検証する\n", ""
        )
        report = self.validate(event)
        self.assertTrue(report.failed)
        self.assertIn("missing-decision", codes(report, "failure"))

    def test_conflict_fails(self):
        event = copy.deepcopy(VALID_EVENT)
        event["pull_request"]["mergeable"] = False
        report = self.validate(event)
        self.assertIn("conflict", codes(report, "failure"))

    def test_large_pull_request_warns_without_failing(self):
        event = copy.deepcopy(VALID_EVENT)
        event["pull_request"].update(changed_files=21, additions=1001, deletions=0)
        report = self.validate(event)
        self.assertFalse(report.failed)
        self.assertEqual({"file-count", "line-count"}, codes(report, "warning"))

    def test_valid_stacked_pull_request_passes(self):
        event = copy.deepcopy(VALID_EVENT)
        event["pull_request"]["base"]["ref"] = "agent/parent"
        event["pull_request"]["body"] = (
            event["pull_request"]["body"]
            .replace("- Base: main", "- Base: agent/parent")
            .replace("- Depends on: none", "- Depends on: #99")
        )
        dependency = {"state": "open", "merged_at": None, "head": {"ref": "agent/parent"}}
        report = self.validate(event, dependency)
        self.assertFalse(report.failed)
        self.assertIn("stacked-dependency", codes(report, "pass"))

    def test_non_default_base_without_dependency_fails(self):
        event = copy.deepcopy(VALID_EVENT)
        event["pull_request"]["base"]["ref"] = "agent/parent"
        event["pull_request"]["body"] = event["pull_request"]["body"].replace(
            "- Base: main", "- Base: agent/parent"
        )
        report = self.validate(event)
        self.assertIn("stacked-dependency", codes(report, "failure"))

    def test_closed_stacked_dependency_fails(self):
        event = copy.deepcopy(VALID_EVENT)
        event["pull_request"]["base"]["ref"] = "agent/parent"
        event["pull_request"]["body"] = (
            event["pull_request"]["body"]
            .replace("- Base: main", "- Base: agent/parent")
            .replace("- Depends on: none", "- Depends on: #99")
        )
        dependency = {
            "state": "closed",
            "merged_at": "2026-08-25T00:00:00Z",
            "head": {"ref": "agent/parent"},
        }
        report = self.validate(event, dependency)
        self.assertIn("closed-dependency", codes(report, "failure"))


class PostMergeTests(unittest.TestCase):
    def test_merge_commit_reachable_from_head_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            subprocess.run(["git", "init", "-q", directory], check=True)
            subprocess.run(["git", "-C", directory, "config", "user.name", "Test"], check=True)
            subprocess.run(
                ["git", "-C", directory, "config", "user.email", "test@example.com"], check=True
            )
            Path(directory, "tracked.txt").write_text("test\n", encoding="utf-8")
            subprocess.run(["git", "-C", directory, "add", "tracked.txt"], check=True)
            subprocess.run(["git", "-C", directory, "commit", "-qm", "test"], check=True)
            sha = subprocess.check_output(
                ["git", "-C", directory, "rev-parse", "HEAD"], text=True
            ).strip()
            event = copy.deepcopy(POST_MERGE_EVENT)
            event["pull_request"]["merge_commit_sha"] = sha
            report = validate_post_merge(event, CONFIG, directory)
            self.assertFalse(report.failed)
            self.assertIn("default-reachability", codes(report, "pass"))

    def test_unreachable_commit_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            subprocess.run(["git", "init", "-q", directory], check=True)
            subprocess.run(["git", "-C", directory, "config", "user.name", "Test"], check=True)
            subprocess.run(
                ["git", "-C", directory, "config", "user.email", "test@example.com"], check=True
            )
            Path(directory, "tracked.txt").write_text("first\n", encoding="utf-8")
            subprocess.run(["git", "-C", directory, "add", "tracked.txt"], check=True)
            subprocess.run(["git", "-C", directory, "commit", "-qm", "first"], check=True)
            default_branch = subprocess.check_output(
                ["git", "-C", directory, "branch", "--show-current"], text=True
            ).strip()
            subprocess.run(["git", "-C", directory, "checkout", "-q", "--orphan", "other"], check=True)
            subprocess.run(["git", "-C", directory, "rm", "-qf", "tracked.txt"], check=True)
            Path(directory, "other.txt").write_text("other\n", encoding="utf-8")
            subprocess.run(["git", "-C", directory, "add", "other.txt"], check=True)
            subprocess.run(["git", "-C", directory, "commit", "-qm", "other"], check=True)
            unreachable_sha = subprocess.check_output(
                ["git", "-C", directory, "rev-parse", "HEAD"], text=True
            ).strip()
            subprocess.run(["git", "-C", directory, "checkout", "-q", default_branch], check=True)

            event = copy.deepcopy(POST_MERGE_EVENT)
            event["pull_request"]["merge_commit_sha"] = unreachable_sha
            report = validate_post_merge(event, CONFIG, directory)
            self.assertIn("default-reachability", codes(report, "failure"))

    def test_non_default_merge_fails(self):
        event = copy.deepcopy(POST_MERGE_EVENT)
        event["pull_request"]["base"]["ref"] = "agent/parent"
        report = validate_post_merge(event, CONFIG, ROOT)
        self.assertIn("not-default-base", codes(report, "failure"))


class WorkflowTrustBoundaryTests(unittest.TestCase):
    def test_pr_policy_uses_default_branch_sources(self):
        workflow = (ROOT / ".github/workflows/pr-policy.yml").read_text(encoding="utf-8")
        self.assertNotRegex(workflow, r"(?m)^  pull_request:")
        self.assertRegex(workflow, r"(?m)^  pull_request_target:")
        self.assertRegex(
            workflow,
            r"(?ms)uses: actions/checkout@[^\n]+\n\s+with:\n\s+ref: \$\{\{ github\.event\.repository\.default_branch \}\}",
        )
        self.assertRegex(workflow, r"(?ms)permissions:\n  contents: read\n  pull-requests: read")
        self.assertIn("python3 -m unittest discover -s tests -v", workflow)
        self.assertIn("python3 scripts/pr_policy.py preflight", workflow)
        self.assertIn("--config .github/pr-policy.json", workflow)
        self.assertNotRegex(
            workflow,
            r"(?m)^\s+ref: \$\{\{ github\.event\.pull_request\.(?:head|base)",
        )

    def test_pr_policy_remains_a_required_main_check(self):
        ruleset = json.loads(
            (ROOT / ".github/rulesets/main-pr-policy.json").read_text(encoding="utf-8")
        )
        required_checks = ruleset["rules"][0]["parameters"]["required_status_checks"]

        self.assertIn({"context": "PR Policy"}, required_checks)


if __name__ == "__main__":
    unittest.main()
