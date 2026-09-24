from __future__ import annotations

import copy
import json
import unittest
from unittest.mock import patch
from pathlib import Path

from scripts.pr_ruleset import _gh_api, check_active_ruleset, validate_ruleset


ROOT = Path(__file__).resolve().parents[1]
DECLARED = json.loads(
    (ROOT / ".github/rulesets/main-pr-policy.json").read_text(encoding="utf-8")
)


def actual_ruleset():
    actual = copy.deepcopy(DECLARED)
    actual["id"] = 21361293
    return actual


class RulesetDeclarationTests(unittest.TestCase):
    def test_declares_all_observed_required_check_contexts(self):
        self.assertEqual(validate_ruleset(DECLARED, actual_ruleset()), [])

    def test_required_rust_contexts_match_ci_job_names_and_commands(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        jobs_section = workflow.split("jobs:\n", maxsplit=1)[1]
        jobs: dict[str, list[str]] = {}
        current_job: str | None = None
        current_lines: list[str] = []
        for line in jobs_section.splitlines():
            if line.startswith("  ") and not line.startswith("    ") and line.endswith(":"):
                if current_job is not None:
                    jobs[current_job] = current_lines
                current_job = line[2:-1]
                current_lines = []
            elif current_job is not None:
                current_lines.append(line)
        if current_job is not None:
            jobs[current_job] = current_lines

        named_jobs = {
            next(
                (
                    line.removeprefix("    name: ")
                    for line in lines
                    if line.startswith("    name: ")
                ),
                job_id,
            ): lines
            for job_id, lines in jobs.items()
        }
        declared_contexts = {
            entry["context"]
            for rule in DECLARED["rules"]
            if rule["type"] == "required_status_checks"
            for entry in rule["parameters"]["required_status_checks"]
        }

        self.assertTrue({"Format", "Clippy", "Test"}.issubset(named_jobs))
        self.assertTrue({"Format", "Clippy", "Test"}.issubset(declared_contexts))
        expected_commands = {
            "Format": "cargo fmt --all -- --check",
            "Clippy": "cargo clippy --workspace --all-targets --all-features -- -D warnings",
            "Test": "cargo test --workspace --all-features",
        }
        for job_name, command in expected_commands.items():
            with self.subTest(job=job_name):
                self.assertIn(f"run: {command}", "\n".join(named_jobs[job_name]))

    def test_missing_ci_context_is_reported(self):
        declared = copy.deepcopy(DECLARED)
        parameters = next(
            rule["parameters"]
            for rule in declared["rules"]
            if rule["type"] == "required_status_checks"
        )
        parameters["required_status_checks"] = [
            item for item in parameters["required_status_checks"] if item["context"] != "Clippy"
        ]

        errors = validate_ruleset(declared, declared)

        self.assertTrue(any("Clippy" in error and "宣言" in error for error in errors))

    def test_inactive_ruleset_is_rejected(self):
        actual = actual_ruleset()
        actual["enforcement"] = "disabled"

        errors = validate_ruleset(DECLARED, actual)

        self.assertTrue(any("active" in error for error in errors))

    def test_wrong_branch_target_is_rejected(self):
        actual = actual_ruleset()
        actual["conditions"]["ref_name"]["include"] = ["refs/heads/release"]

        errors = validate_ruleset(DECLARED, actual)

        self.assertTrue(any("対象branch" in error for error in errors))

    def test_live_context_difference_reports_missing_and_extra_names(self):
        actual = actual_ruleset()
        parameters = next(
            rule["parameters"]
            for rule in actual["rules"]
            if rule["type"] == "required_status_checks"
        )
        parameters["required_status_checks"] = [
            {"context": "PR Policy"},
            {"context": "Format"},
            {"context": "Test"},
            {"context": "CodeRabbit"},
        ]

        errors = validate_ruleset(DECLARED, actual)

        self.assertTrue(any("Clippy" in error and "CodeRabbit" in error for error in errors))


class ActiveRulesetLookupTests(unittest.TestCase):
    def test_uses_read_only_list_and_detail_endpoints(self):
        expected = actual_ruleset()
        calls = []

        def api(endpoint):
            calls.append(endpoint)
            if endpoint == "repos/example/project/rulesets?includes_parents=false&per_page=100":
                return [{"id": expected["id"], "name": expected["name"]}]
            if endpoint == f"repos/example/project/rulesets/{expected['id']}":
                return expected
            raise AssertionError(f"unexpected API endpoint: {endpoint}")

        actual, errors = check_active_ruleset("example/project", DECLARED, api)

        self.assertEqual(actual, expected)
        self.assertEqual(errors, [])
        self.assertEqual(
            calls,
            [
                "repos/example/project/rulesets?includes_parents=false&per_page=100",
                f"repos/example/project/rulesets/{expected['id']}",
            ],
        )

    def test_ruleset_on_later_page_is_found(self):
        expected = actual_ruleset()
        calls = []
        first_page = [{"id": index, "name": f"other-{index}"} for index in range(100)]
        second_page = [{"id": expected["id"], "name": expected["name"]}]

        def api(endpoint):
            calls.append(endpoint)
            if endpoint.endswith("includes_parents=false&per_page=100"):
                return first_page + second_page
            if endpoint == f"repos/example/project/rulesets/{expected['id']}":
                return expected
            raise AssertionError(f"unexpected API endpoint: {endpoint}")

        actual, errors = check_active_ruleset("example/project", DECLARED, api)

        self.assertEqual(actual, expected)
        self.assertEqual(errors, [])

    @patch("scripts.pr_ruleset.subprocess.run")
    def test_list_api_follows_and_flattens_all_pages(self, run):
        run.return_value.stdout = json.dumps([[{"id": 1}], [{"id": 2}]])

        result = _gh_api(
            "repos/example/project/rulesets?includes_parents=false&per_page=100"
        )

        self.assertEqual(result, [{"id": 1}, {"id": 2}])
        self.assertEqual(
            run.call_args.args[0],
            [
                "gh",
                "api",
                "--paginate",
                "--slurp",
                "repos/example/project/rulesets?includes_parents=false&per_page=100",
            ],
        )

    def test_absent_ruleset_has_actionable_diagnostic(self):
        actual, errors = check_active_ruleset(
            "example/project", DECLARED, lambda _endpoint: []
        )

        self.assertIsNone(actual)
        self.assertTrue(any("一意に見つかりません" in error for error in errors))

    def test_api_failure_preserves_diagnostic(self):
        def unavailable(_endpoint):
            raise RuntimeError("connection refused")

        actual, errors = check_active_ruleset("example/project", DECLARED, unavailable)

        self.assertIsNone(actual)
        self.assertTrue(any("connection refused" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
