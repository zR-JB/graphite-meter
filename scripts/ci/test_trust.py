from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import cast
from unittest.mock import patch

from github_api import APICall, JsonValue
from prerelease import PRERELEASE_RE
from release import STABLE_SEMVER_RE, require_compatible_release_tag, validate_request_context
from trust import (
    TrustError,
    exact_files,
    require_check_run,
    require_ci_gate,
    require_control_plane_matches_main,
    require_current_main,
    require_dispatch_run,
    require_exact_current_main,
    require_main_codeql,
)

ROOT = Path(__file__).resolve().parents[2]
REPO = "zR-JB/graphite-meter"
MAIN, HEAD, OLD = "1" * 40, "2" * 40, "3" * 40
APP = {"slug": "github-advanced-security"}


def fake(responses: dict[str, object]) -> APICall:
    def api(path: str, *, paginate: bool = False) -> JsonValue:
        for fragment, response in responses.items():
            if fragment in path:
                return cast(JsonValue, response)
        raise AssertionError(path)

    return api


def analysis(analysis_id: int, minute: int, error: str = "") -> dict[str, object]:
    return {
        "id": analysis_id, "commit_sha": MAIN, "created_at": f"2026-08-15T10:{minute:02}:00Z",
        "category": "go", "analysis_key": "default", "environment": "", "error": error,
        "warning": "", "tool": {"name": "CodeQL"},
    }


def ci_run(run_id: int, conclusion: str, pr: int = 101) -> dict[str, object]:
    return {
        "id": run_id, "run_number": run_id, "run_attempt": 1, "head_sha": HEAD,
        "head_branch": "fix/test", "event": "pull_request", "status": "completed",
        "conclusion": conclusion, "pull_requests": [{"number": pr}],
    }


def dispatch_run(workflow_id: int, run_id: int) -> dict[str, object]:
    return {
        "id": run_id, "workflow_id": workflow_id, "event": "workflow_dispatch",
        "head_branch": "main", "head_sha": MAIN, "status": "completed", "conclusion": "success",
        "run_attempt": 1, "actor": {"login": "zR-JB"}, "triggering_actor": {"login": "zR-JB"},
    }


def artifacts(name: str, size: int = 1024, expired: bool = False) -> list[object]:
    return [{"artifacts": [{"name": name, "expired": expired, "size_in_bytes": size}]}]


class MainBindingTests(unittest.TestCase):
    def test_pr_must_contain_exact_current_main(self) -> None:
        for behind, base, ok in ((0, MAIN, True), (3, OLD, False), (0, OLD, False)):
            api = fake({"/commits/main": {"sha": MAIN},
                        "/compare/": {"behind_by": behind, "merge_base_commit": {"sha": base}}})
            with self.subTest(behind=behind, base=base):
                if ok:
                    self.assertEqual(require_current_main(REPO, 101, HEAD, api=api), MAIN)
                else:
                    with self.assertRaisesRegex(TrustError, "behind current main"):
                        require_current_main(REPO, 101, HEAD, api=api)
        api = fake({"/commits/main": {"sha": MAIN}})
        self.assertEqual(require_exact_current_main(REPO, MAIN, api=api), MAIN)
        with self.assertRaisesRegex(TrustError, "no longer current main"):
            require_exact_current_main(REPO, OLD, api=api)

    def test_prerelease_control_plane_must_match_main(self) -> None:
        def trees(scripts_at_head: str) -> APICall:
            def tree(scripts: str) -> dict[str, object]:
                return {"tree": [{"path": ".github", "sha": "a" * 40},
                                 {"path": "scripts", "sha": scripts}, {"path": "go", "sha": HEAD}]}
            return fake({f"/git/trees/{HEAD}": tree(scripts_at_head),
                         f"/git/trees/{MAIN}": tree("b" * 40)})

        require_control_plane_matches_main(REPO, HEAD, MAIN, api=trees("b" * 40))
        with self.assertRaisesRegex(TrustError, "PR changes scripts"):
            require_control_plane_matches_main(REPO, HEAD, MAIN, api=trees("c" * 40))

    def test_release_tag_preflight_accepts_only_the_expected_commit(self) -> None:
        tag_ref = "/git/matching-refs/tags/v1.2.3"
        require_compatible_release_tag(REPO, "v1.2.3", MAIN, api=fake({tag_ref: []}))
        annotated = fake({
            tag_ref: [{"ref": "refs/tags/v1.2.3", "object": {"type": "tag", "sha": OLD}}],
            f"/git/tags/{OLD}": {"object": {"type": "commit", "sha": MAIN}},
        })
        require_compatible_release_tag(REPO, "v1.2.3", MAIN, api=annotated)
        moved = fake({tag_ref: [{"ref": "refs/tags/v1.2.3",
                                 "object": {"type": "commit", "sha": HEAD}}]})
        with self.assertRaisesRegex(TrustError, "already exists"):
            require_compatible_release_tag(REPO, "v1.2.3", MAIN, api=moved)

    def test_release_versions_are_strict_semver(self) -> None:
        for value, stable, prerelease in (
            ("v0.5.2", True, False), ("v10.12.30-rc.7", False, True),
            ("v0.5.2-alpha.0", False, True), ("0.5.2", False, False), ("v0.5", False, False),
            ("v0.5.2-preview.1", False, False), ("v01.5.2", False, False),
            ("v0.05.2-rc.1", False, False), ("v0.5.2-alpha.00", False, False),
        ):
            with self.subTest(value=value):
                self.assertEqual(STABLE_SEMVER_RE.fullmatch(value) is not None, stable)
                self.assertEqual(PRERELEASE_RE.fullmatch(value) is not None, prerelease)


class GateTests(unittest.TestCase):
    def test_newest_codeql_analysis_per_category_decides(self) -> None:
        for analyses, error in (
            ([analysis(10, 0, "transient"), analysis(11, 5)], None),
            ([analysis(20, 0), analysis(21, 6, "finalize failed")], "has errors"),
            ([], "missing"),
        ):
            api = fake({"/code-scanning/analyses": [analyses]})
            with self.subTest(error=error):
                if error is None:
                    require_main_codeql(REPO, MAIN, api=api)
                else:
                    with self.assertRaisesRegex(TrustError, error):
                        require_main_codeql(REPO, MAIN, api=api)

    def test_ci_gate_uses_newest_pr_run_and_its_gate(self) -> None:
        gate_ok = [{"jobs": [{"name": "Gate", "status": "completed", "conclusion": "success"}]}]
        gate_failed = [{"jobs": [{"name": "Gate", "status": "completed", "conclusion": "failure"}]}]
        for runs, jobs, result in (
            ([ci_run(10, "failure"), ci_run(11, "success")], gate_ok, 11),
            ([ci_run(10, "success"), ci_run(11, "failure")], gate_ok, "latest CI run 11"),
            ([ci_run(11, "success")], gate_failed, "Gate in CI run 11"),
            ([ci_run(11, "success", pr=999)], gate_ok, "missing"),
        ):
            api = fake({"/actions/workflows/ci.yml/runs?": [{"workflow_runs": runs}],
                        "/actions/runs/11/jobs?": jobs})
            with self.subTest(result=result):
                if isinstance(result, int):
                    self.assertEqual(require_ci_gate(REPO, HEAD, event="pull_request",
                                                     branch="fix/test", pr_number=101, api=api),
                                     result)
                else:
                    with self.assertRaisesRegex(TrustError, result):
                        require_ci_gate(REPO, HEAD, event="pull_request", branch="fix/test",
                                        pr_number=101, api=api)

    def test_python_and_workflow_select_the_same_codeql_check(self) -> None:
        workflow = (ROOT / ".github/workflows/_publish-oci.yml").read_text(encoding="utf-8")
        prefix = "latest_codeql=$(jq -c --argjson pr \"$PR_NUMBER\" '"
        selector = workflow.split(prefix, 1)[1].split("' <<<\"$check_pages\")", 1)[0]
        older = {"id": 41, "status": "completed", "conclusion": "success",
                 "started_at": "2026-09-05T10:00:00Z", "pull_requests": [{"number": 101}]}
        for status, conclusion, started, pr, selected, allowed in (
            ("in_progress", None, "2026-09-05T10:04:00Z", 101, 42, False),
            ("queued", None, None, 101, 42, False),
            ("completed", "failure", "2026-09-05T10:04:00Z", 101, 42, False),
            ("completed", "success", "2026-09-05T10:04:00Z", 101, 42, True),
            ("completed", "failure", "2026-09-05T10:04:00Z", 999, 41, True),
        ):
            newer = {"id": 42, "status": status, "conclusion": conclusion, "started_at": started,
                     "pull_requests": [{"number": pr}]}
            checks = [{"name": "CodeQL", "app": APP, **item} for item in (older, newer)]
            pages = [{"check_runs": checks}]
            with self.subTest(status=status, conclusion=conclusion, pr=pr):
                api = fake({"/check-runs?per_page=100&filter=all": pages})
                if allowed:
                    self.assertEqual(require_check_run(
                        REPO, HEAD, name="CodeQL", app_slug=APP["slug"], pr_number=101, api=api,
                    ), selected)
                else:
                    with self.assertRaises(TrustError):
                        require_check_run(REPO, HEAD, name="CodeQL", app_slug=APP["slug"],
                                          pr_number=101, api=api)
                result = subprocess.run(
                    ["jq", "-c", "--argjson", "pr", "101", selector + " | .id"],
                    input=json.dumps(pages), text=True, capture_output=True, check=True,
                )
                self.assertEqual(result.stdout.strip(), str(selected))


class RequestTests(unittest.TestCase):
    def test_request_run_is_bound_to_main_owner_attempt_and_artifact(self) -> None:
        name = "prerelease-candidate-6001"
        for field, value, error in (
            (None, None, None),
            ("head_branch", "feature/stale", "not dispatched from main"),
            ("head_sha", OLD, "main changed"),
            ("run_attempt", 2, "reruns"),
            ("workflow_id", 1, "is not a prerelease-request.yml run"),
            ("triggering_actor", {"login": "other"}, "repository owner"),
            ("artifact", artifacts(name, expired=True), "expected one unexpired"),
            ("artifact", artifacts(name, size=4097), "exceeds"),
        ):
            run = dispatch_run(7001, 6001)
            files = artifacts(name)
            if field == "artifact":
                files = value
            elif field is not None:
                run[field] = value
            api = fake({"/actions/workflows/prerelease-request.yml": {"id": 7001},
                        "/actions/runs/6001/artifacts": files, "/actions/runs/6001": run})
            with self.subTest(field=field, error=error):
                def bind() -> None:
                    require_dispatch_run(REPO, "zR-JB", MAIN, 6001, "prerelease-request.yml",
                                         name, max_size=4096, api=api)
                if error is None:
                    bind()
                else:
                    with self.assertRaisesRegex(TrustError, error):
                        bind()

    def test_handoff_directories_hold_exact_regular_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "candidate.json").write_text("{}")
            exact_files(root, {"candidate.json"})
            (root / "extra").write_text("x")
            with self.assertRaisesRegex(TrustError, "files are"):
                exact_files(root, {"candidate.json"})
            (root / "extra").unlink()
            (root / "candidate.json").unlink()
            (root / "candidate.json").symlink_to(root)
            with self.assertRaisesRegex(TrustError, "not a regular file"):
                exact_files(root, {"candidate.json"})

    def test_stable_request_consumer_binds_request_to_trusted_main(self) -> None:
        run_id = 4242
        responses: dict[str, object] = {
            "/commits/main": {"sha": MAIN},
            "/actions/workflows/release-request.yml": {"id": 31337},
            f"/actions/runs/{run_id}/artifacts": artifacts(f"stable-release-request-{run_id}"),
            f"/actions/runs/{run_id}": dispatch_run(31337, run_id),
            "/git/matching-refs/tags/v1.2.3": [],
            "/actions/workflows/ci.yml/runs": [{"workflow_runs": [{
                **ci_run(5151, "success"), "head_sha": MAIN, "head_branch": "main",
                "event": "push", "pull_requests": [],
            }]}],
            "/actions/runs/5151/jobs": [{"jobs": [
                {"name": "Gate", "status": "completed", "conclusion": "success"}]}],
            "/code-scanning/analyses": [[analysis(99, 1)]],
        }
        request: dict[str, object] = {
            "schemaVersion": 1, "repository": REPO, "sourceSha": MAIN, "version": "v1.2.3",
            "mode": "publish", "requestRunId": run_id, "requestRunAttempt": 1,
        }
        with tempfile.TemporaryDirectory() as directory:
            env = {
                "REPOSITORY": REPO, "REPOSITORY_OWNER": "zR-JB", "PUBLISHER_SHA": MAIN,
                "WORKFLOW_REF": f"{REPO}/.github/workflows/release.yml@refs/heads/main",
                "REQUEST_RUN_ID": str(run_id), "REQUEST_DIR": directory,
            }
            for change, error in (({}, None), ({"sourceSha": OLD}, "sourceSha does not match"),
                                  ({"requestRunAttempt": True}, "requestRunAttempt"),
                                  ({"version": "v1.2.3-rc.1"}, "vMAJOR.MINOR.PATCH")):
                (Path(directory) / "request.json").write_text(json.dumps(request | change))
                with (self.subTest(change=change), patch.dict(os.environ, env),
                      patch("release.require_checkout")):
                    if error is None:
                        context = validate_request_context(api=fake(responses))
                        self.assertEqual((context.sha, context.ci_run_id, context.publish),
                                         (MAIN, 5151, True))
                    else:
                        with self.assertRaisesRegex(TrustError, error):
                            validate_request_context(api=fake(responses))


if __name__ == "__main__":
    unittest.main()
