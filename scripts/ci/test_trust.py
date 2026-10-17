from __future__ import annotations

import hashlib
import json
import os
import re
import tempfile
import unittest
from collections.abc import Callable
from pathlib import Path
from typing import cast
from unittest.mock import patch

from github_api import (
    ControlPlaneError, confined_path, file_sha256, local_path, runner_path,
)
from assets import OCI, release_assets, staged_files
from fixtures import git_head, github, outcome, pages, write_client, write_staged
from release import (
    BUILD_JOB,
    Release,
    assets_sha256,
    command_prepare,
    command_recheck,
    command_verify,
    parse_release,
    request_title,
    require_compatible_release_tag,
    require_publishable,
    verify_request,
    write_checksums,
)
from trust import (
    exact_files,
    require_check_run,
    require_ci_gate,
    require_control_plane_matches_main,
    require_current_main,
    require_dispatch_run,
    require_exact_current_main,
    require_main_codeql,
    require_pr,
    require_protected_environment,
)

REPO = "zR-JB/graphite-meter"
MAIN, HEAD, OLD = "1" * 40, "2" * 40, "3" * 40
APP = {"slug": "github-advanced-security"}
P = f"repos/{REPO}/"
PULL, MAIN_COMMIT = P + "pulls/101", P + "commits/main"
REQUEST_WORKFLOW, REQUEST_RUN = P + "actions/workflows/release-request.yml", P + "actions/runs/4242"
ARTIFACTS = REQUEST_RUN + "/artifacts?per_page=100"
REQUEST_JOBS = REQUEST_RUN + "/jobs?filter=latest&per_page=100"
JOBS = P + "actions/runs/5151/jobs?filter=latest&per_page=100"
CHECKS = P + f"commits/{HEAD}/check-runs?per_page=100&filter=all"
CODEQL = P + "code-scanning/analyses?ref=refs%2Fheads%2Fmain&tool_name=CodeQL&per_page=100"
TAG_REFS = P + "git/matching-refs/tags/v1.2.3"
RELEASES, RELEASE_ASSETS = P + "releases?per_page=100", P + "releases/9/assets?per_page=100"
ENVIRONMENT = P + "environments/ghcr-release"
POLICIES = ENVIRONMENT + "/deployment-branch-policies"
GATE = {"name": "Gate", "status": "completed", "conclusion": "success"}
PR = {"state": "open", "base": {"ref": "main"},
      "head": {"sha": HEAD, "ref": "fix/test", "repo": {"full_name": REPO}}}
REVIEWED = {"protection_rules": [{"type": "required_reviewers", "reviewers": [{"type": "User"}]}],
            "deployment_branch_policy": {"custom_branch_policies": True}}
MAIN_ONLY = {"branch_policies": [{"name": "main", "type": "branch"}]}


def compare(main: str) -> str:
    return P + f"compare/{main}...{HEAD}"


def tree(sha: str) -> str:
    return P + f"git/trees/{sha}"


def ci_runs(event: str, sha: str) -> str:
    return P + f"actions/workflows/ci.yml/runs?event={event}&head_sha={sha}&per_page=100"


def analysis(analysis_id: int, minute: int, error: str = "",
             commit: str = MAIN) -> dict[str, object]:
    return {
        "id": analysis_id, "commit_sha": commit, "created_at": f"2026-08-15T10:{minute:02}:00Z",
        "category": "rust", "analysis_key": "default", "environment": "", "error": error,
        "warning": "", "tool": {"name": "CodeQL"},
    }


def ci_run(run_id: int, conclusion: str, pr: int = 101) -> dict[str, object]:
    return {
        "id": run_id, "run_number": run_id, "run_attempt": 1, "head_sha": HEAD,
        "head_branch": "fix/test", "event": "pull_request", "status": "completed",
        "conclusion": conclusion, "pull_requests": [{"number": pr}],
    }


def dispatch_run(workflow_id: int, run_id: int, title: str = "title") -> dict[str, object]:
    return {
        "id": run_id, "workflow_id": workflow_id, "event": "workflow_dispatch",
        "head_branch": "main", "head_sha": MAIN, "status": "completed", "conclusion": "success",
        "run_attempt": 1, "actor": {"login": "zR-JB"}, "triggering_actor": {"login": "zR-JB"},
        "display_title": title,
    }


def written(*names: str, size: int = 1024, expired: bool = False, created: str = "10:05",
            updated: str = "10:06") -> list[dict[str, object]]:
    return [{"name": name, "expired": expired, "size_in_bytes": size,
             "created_at": f"2026-09-01T{created}:00Z", "updated_at": f"2026-09-01T{updated}:00Z"} for name in names]


def artifacts(*names: str, size: int = 1024, expired: bool = False, created: str = "10:05",
              updated: str = "10:06") -> object:
    return pages({"artifacts": written(*names, size=size, expired=expired, created=created, updated=updated)})


def job(name: str = BUILD_JOB, conclusion: str = "success", started: str = "10:00",
        completed: str = "10:10") -> dict[str, object]:
    """A request job that ran from 10:00 to 10:10."""
    return {"name": name, "status": "completed", "conclusion": conclusion,
            "started_at": f"2026-09-01T{started}:00Z", "completed_at": f"2026-09-01T{completed}:00Z"}


def release_of(stable: bool) -> Release:
    return Release("v1.2.3", MAIN, 0) if stable else Release("v1.2.3-rc.1", HEAD, 101)


def trusted(stable: bool, mode: str = "publish") -> dict[str, object]:
    """Every GitHub answer that authorizes a stable release or a PR #101 prerelease."""
    responses: dict[str, object] = {
        MAIN_COMMIT: {"sha": MAIN}, REQUEST_WORKFLOW: {"id": 31337},
        REQUEST_RUN: dispatch_run(31337, 4242, request_title(mode, release_of(stable), MAIN)),
        ARTIFACTS: artifacts("release-request-4242"), JOBS: pages({"jobs": [GATE]}),
        REQUEST_JOBS: pages({"jobs": [job()]}),
    }
    if mode == "publish":
        responses |= {ENVIRONMENT: REVIEWED, POLICIES: MAIN_ONLY}
    if stable:
        run = ci_run(5151, "success") | {"head_sha": MAIN, "head_branch": "main",
                                          "event": "push", "pull_requests": []}
        return responses | {
            TAG_REFS: [], ci_runs("push", MAIN): pages({"workflow_runs": [run]}),
            CODEQL: pages([analysis(99, 1)]), RELEASES: pages([]),
        }
    check = {"id": 77, "name": "CodeQL", "app": APP, "status": "completed",
             "conclusion": "success", "pull_requests": [{"number": 101}]}
    control = {"tree": [{"path": "scripts", "sha": "b" * 40}]}
    return responses | {
        PULL: PR, compare(MAIN): {"behind_by": 0, "merge_base_commit": {"sha": MAIN}},
        tree(HEAD): control, tree(MAIN): control,
        ci_runs("pull_request", HEAD): pages({"workflow_runs": [ci_run(5151, "success")]}),
        CHECKS: pages({"check_runs": [check]}),
    }


def published_with(*names: str, draft: bool, directory: Path | None = None) -> dict[str, object]:
    """GitHub's answers for a v1.2.3 Release with assets `names`, digested from `directory` or made up."""
    def digest(name: str) -> str:
        return "sha256:" + (file_sha256(directory / name) if directory else "0" * 64)

    return {RELEASES: pages([{"id": 9, "tag_name": "v1.2.3", "draft": draft, "prerelease": False}]),
            RELEASE_ASSETS: pages([{"name": name, "digest": digest(name)} for name in names])}


# Main has advanced to OLD, which the PR head still contains.
MOVED = {compare(OLD): {"behind_by": 0, "merge_base_commit": {"sha": OLD}},
         tree(OLD): {"tree": [{"path": "scripts", "sha": "b" * 40}]}}


class MainBindingTests(unittest.TestCase):
    def test_pr_must_contain_exact_current_main(self) -> None:
        for behind, base, error in ((0, MAIN, None), (3, OLD, "behind current main"),
                                    (0, OLD, "behind current main")):
            with self.subTest(behind=behind, base=base), github({
                    MAIN_COMMIT: {"sha": MAIN},
                    compare(MAIN): {"behind_by": behind, "merge_base_commit": {"sha": base}}}):
                self.assertEqual(outcome(self, error, lambda: require_current_main(REPO, 101, HEAD)),
                                 None if error else MAIN)

    def test_current_main_is_an_exact_commit(self) -> None:
        for main, sha, error in ((MAIN, MAIN, None), (MAIN, OLD, "no longer current main"),
                                 ("main", "main", "could not resolve"),
                                 ("A" * 40, "A" * 40, "could not resolve")):
            with self.subTest(main=main, sha=sha), github({MAIN_COMMIT: {"sha": main}}):
                self.assertEqual(outcome(self, error, lambda: require_exact_current_main(REPO, sha)),
                                 None if error else main)

    def test_prerelease_pr_is_open_same_repository_against_main_at_head(self) -> None:
        head = cast(dict[str, object], PR["head"])
        for change, error in (
            ({}, None), ({"state": "closed"}, "not open against main"),
            ({"base": {"ref": "release"}}, "not open against main"),
            ({"head": head | {"repo": {"full_name": "fork/graphite-meter"}}}, "fork PRs"),
            ({"head": head | {"sha": OLD}}, "head SHA changed"),
        ):
            with self.subTest(change=change), github({PULL: PR | change}):
                self.assertEqual(outcome(self, error, lambda: require_pr(REPO, 101, HEAD)),
                                 None if error else "fix/test")

    def test_prerelease_control_plane_must_match_main(self) -> None:
        control = (".github", ".githooks", "scripts", "mise.toml", "mise.lock")
        paths = (*control, "rust", "client")
        main = [{"path": path, "sha": "a" * 40} for path in paths]
        for path in paths:
            for changed in ({"path": path, "sha": "b" * 40}, None):
                entries = [item for item in main if item["path"] != path] + (
                    [changed] if changed else [])
                error = f"PR changes {re.escape(path)};" if path in control else None
                with (self.subTest(path=path, removed=changed is None),
                      github({tree(HEAD): {"tree": entries}, tree(MAIN): {"tree": main}})):
                    outcome(self, error, lambda: require_control_plane_matches_main(REPO, HEAD, MAIN))

    def test_release_tag_preflight_accepts_only_the_expected_commit(self) -> None:
        annotated = {
            TAG_REFS: [{"ref": "refs/tags/v1.2.3", "object": {"type": "tag", "sha": OLD}}],
            P + f"git/tags/{OLD}": {"object": {"type": "commit", "sha": MAIN}},
        }
        moved = {TAG_REFS: [{"ref": "refs/tags/v1.2.3", "object": {"type": "commit", "sha": HEAD}}]}
        for responses, error in (({TAG_REFS: []}, None), (annotated, None),
                                 (moved, "already exists")):
            with self.subTest(error=error), github(responses):
                outcome(self, error, lambda: require_compatible_release_tag(REPO, "v1.2.3", MAIN))

    def test_stable_tags_have_no_pr_and_prerelease_tags_may_name_one(self) -> None:
        for tag, pr, ok in (
            ("v0.5.2", 0, True), ("v10.12.30-rc.7", 7, True), ("v0.5.2-alpha.0", 1, True),
            ("v0.5.2-rc.1", 0, True), ("v0.5.2", 7, False), ("v0.5.2-rc.1", -1, False),
            ("0.5.2", 0, False), ("v0.5", 0, False), ("v0.5.2-preview.1", 7, False),
            ("v01.5.2", 0, False), ("v0.05.2-rc.1", 7, False), ("v0.5.2-alpha.00", 7, False),
        ):
            with self.subTest(tag=tag, pr=pr):
                self.assertEqual(outcome(self, None if ok else "stable tags",
                                         lambda: parse_release(tag, MAIN, pr)),
                                 Release(tag, MAIN, pr) if ok else None)
        for sha in ("main", "A" * 40, MAIN[:39], MAIN + "1"):
            with self.subTest(sha=sha), self.assertRaisesRegex(ControlPlaneError, "40-character"):
                parse_release("v0.5.2", sha, 0)


class EnvironmentTests(unittest.TestCase):
    def test_publishing_environment_needs_reviewers_and_main_only_deployments(self) -> None:
        for environment, policies, error in (
            (REVIEWED, MAIN_ONLY, None),
            (REVIEWED | {"protection_rules": []}, MAIN_ONLY, "require reviewers"),
            (REVIEWED | {"deployment_branch_policy": None}, MAIN_ONLY, "main"),
            (REVIEWED, {"branch_policies": [{"name": "*", "type": "branch"}]}, "main"),
            (REVIEWED, {"branch_policies": MAIN_ONLY["branch_policies"] * 2}, "main"),
        ):
            with self.subTest(error=error), github({POLICIES: policies, ENVIRONMENT: environment}):
                outcome(self, error, lambda: require_protected_environment(REPO))


class GateTests(unittest.TestCase):
    def test_newest_codeql_analysis_per_category_decides(self) -> None:
        for analyses, error in (
            ([analysis(10, 0, "transient"), analysis(11, 5)], None),
            ([analysis(20, 0), analysis(21, 6, "finalize failed")], "has errors"),
            ([analysis(30, 0, commit=OLD)], "missing"),
            ([], "missing"),
        ):
            with self.subTest(error=error, analyses=len(analyses)), github({CODEQL: pages(analyses)}):
                outcome(self, error, lambda: require_main_codeql(REPO, MAIN))

    def test_ci_gate_uses_newest_run_of_this_commit_pr_and_event_and_its_gate(self) -> None:
        failed = GATE | {"conclusion": "failure"}
        for runs, jobs, result in (
            ([ci_run(10, "failure"), ci_run(11, "success")], [GATE], 11),
            ([ci_run(10, "success"), ci_run(11, "failure")], [GATE], "latest CI run 11"),
            ([ci_run(11, "success")], [failed], "Gate in CI run 11"),
            ([ci_run(11, "success")], [], "Gate in CI run 11"),
            ([ci_run(11, "success")], [GATE, GATE], "Gate in CI run 11"),
            ([ci_run(11, "success", pr=999)], [GATE], "missing"),
            ([ci_run(11, "success") | {"pull_requests": [{"number": 101}] * 2}], [GATE],
             "missing"),
            ([ci_run(11, "success") | {"head_sha": OLD}], [GATE], "missing"),
            ([ci_run(11, "success") | {"event": "push"}], [GATE], "missing"),
            ([ci_run(11, "success") | {"event": "schedule"}], [GATE], "missing"),
            ([ci_run(11, "success") | {"head_branch": "main"}], [GATE], "missing"),
        ):
            responses = {ci_runs("pull_request", HEAD): pages({"workflow_runs": runs}),
                         P + "actions/runs/11/jobs?filter=latest&per_page=100":
                             pages({"jobs": jobs})}
            error = result if isinstance(result, str) else None
            with self.subTest(runs=runs, jobs=jobs), github(responses):
                self.assertEqual(outcome(self, error, lambda: require_ci_gate(
                    REPO, HEAD, event="pull_request", branch="fix/test", pr_number=101)),
                    None if error else result)

    def test_newest_pr_codeql_check_from_the_security_app_decides(self) -> None:
        older = {"id": 41, "status": "completed", "conclusion": "success",
                 "started_at": "2026-09-05T10:00:00Z", "pull_requests": [{"number": 101}]}
        for status, conclusion, started, pr, app, allowed in (
            ("in_progress", None, "2026-09-05T10:04:00Z", 101, APP, None),
            ("queued", None, None, 101, APP, None),
            ("completed", "failure", "2026-09-05T10:04:00Z", 101, APP, None),
            ("completed", "success", "2026-09-05T10:04:00Z", 101, APP, 42),
            ("completed", "failure", "2026-09-05T10:04:00Z", 999, APP, 41),
            ("completed", "failure", "2026-09-05T10:04:00Z", 101, {"slug": "other"}, 41),
        ):
            newer = {"id": 42, "status": status, "conclusion": conclusion, "started_at": started,
                     "pull_requests": [{"number": pr}], "app": app}
            checks = [{"name": "CodeQL", "app": APP} | item for item in (older, newer)]
            with (self.subTest(status=status, conclusion=conclusion, pr=pr, app=app),
                  github({CHECKS: pages({"check_runs": checks})})):
                self.assertEqual(outcome(self, None if allowed else "CodeQL", lambda: require_check_run(
                    REPO, HEAD, name="CodeQL", app_slug=APP["slug"], pr_number=101)), allowed)


class RequestTests(unittest.TestCase):
    def test_request_run_is_bound_to_main_owner_attempt_and_artifacts(self) -> None:
        name = "release-request-4242"
        for field, value, error in (
            (None, None, None),
            ("id", 4243, "is not a release-request.yml run"),
            ("workflow_id", 1, "is not a release-request.yml run"),
            ("display_title", "other", "dispatch inputs"),
            ("event", "push", "not dispatched from main"),
            ("head_branch", "feature/stale", "not dispatched from main"),
            ("head_sha", OLD, "main changed"),
            ("run_attempt", 2, "reruns"),
            ("status", "in_progress", "request run is in_progress"),
            ("conclusion", "failure", "request run is completed/failure"),
            ("actor", {"login": "other"}, "repository owner"),
            ("triggering_actor", {"login": "other"}, "repository owner"),
            ("artifact", artifacts(name, expired=True), "expected one unexpired"),
            ("artifact", artifacts(name, name), "expected one unexpired"),
            ("artifact", artifacts(name, size=4097), "exceeds"),
            # The artifact comes from the job named for it, written while that job ran.
            ("artifact", artifacts(name, created="09:59"), "not written while"),
            ("artifact", artifacts(name, updated="10:11"), "not written while"),
            ("artifact", artifacts(name, created="10:07"), "not written while"),
            ("artifact", artifacts(name, created="10:00", updated="10:10"), None),
            ("artifact", artifacts("release-request-4243"), "expected one unexpired"),
            # The run is its one job, which wrote the artifact.
            ("jobs", [], "single successful job"),
            ("jobs", [job("Build something else")], "single successful job"),
            ("jobs", [job(), job()], "single successful job"),
            ("jobs", [job(), job("Other", "skipped")], "single successful job"),
            ("jobs", [job(conclusion="failure")], "single successful job"),
            ("jobs", [job() | {"started_at": "yesterday"}], "not an ISO 8601 time"),
        ):
            run = dispatch_run(7001, 4242)
            files, jobs = artifacts(name), [job()]
            if field == "artifact":
                files = value
            elif field == "jobs":
                jobs = cast(list[dict[str, object]], value)
            elif field is not None:
                run[field] = value
            with (self.subTest(field=field, value=value, error=error),
                  github({REQUEST_WORKFLOW: {"id": 7001}, ARTIFACTS: files, REQUEST_RUN: run,
                          REQUEST_JOBS: pages({"jobs": jobs})})):
                outcome(self, error, lambda: require_dispatch_run(
                    REPO, "zR-JB", MAIN, 4242, "release-request.yml", "title", BUILD_JOB, name, 4096))

    def test_handoff_directories_hold_exact_regular_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "request.json").write_text("{}")
            exact_files(root, {"request.json"})
            (root / "extra").write_text("x")
            with self.assertRaisesRegex(ControlPlaneError, "files are"):
                exact_files(root, {"request.json"})
            (root / "extra").unlink()
            (root / "request.json").unlink()
            (root / "request.json").symlink_to(root)
            with self.assertRaisesRegex(ControlPlaneError, "not a regular file"):
                exact_files(root, {"request.json"})

    def test_paths_stay_inside_their_roots(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "runner"
            (root / "request").mkdir(parents=True)
            (root / "escape").symlink_to("/")
            self.assertEqual(confined_path(root / "request" / "new", root),
                             root.resolve() / "request" / "new")
            for path in (root, root / "request/../..", root / "escape", Path(directory)):
                with self.subTest(path=path), self.assertRaisesRegex(ControlPlaneError, "outside"):
                    confined_path(path, root)
            for env in ({"OUT_DIR": str(root / "out")}, {"RUNNER_TEMP": str(root)}):
                with (self.subTest(env=env), patch.dict(os.environ, env, clear=True),
                      self.assertRaisesRegex(ControlPlaneError, "RUNNER_TEMP are required")):
                    runner_path("OUT_DIR")
            with patch.dict(os.environ, {"RUNNER_TEMP": ""}):
                self.assertEqual(local_path("dist", os.getcwd()), Path("dist").resolve())
                self.assertEqual(local_path(root / "request", "/usr"), root.resolve() / "request")
                for path in ("/etc/passwd", root / "escape/etc"):
                    with self.subTest(path=path), self.assertRaisesRegex(ControlPlaneError, "outside"):
                        local_path(path, "/usr")

    def test_prepare_accepts_only_owner_dispatches_of_well_formed_requests(self) -> None:
        base = {
            "REPOSITORY": REPO, "REPOSITORY_OWNER": "zR-JB", "ACTOR": "zR-JB",
            "TRIGGERING_ACTOR": "zR-JB", "EVENT_NAME": "workflow_dispatch", "EVENT_SHA": MAIN,
            "REF": "refs/heads/main", "REQUEST_RUN_ID": "4242", "REQUEST_RUN_ATTEMPT": "1",
            "WORKFLOW_REF": f"{REPO}/.github/workflows/release-request.yml@refs/heads/main",
            "TAG": "v1.2.3", "PR": "", "SHA": "", "MODE": "publish",
        }
        prerelease = {"TAG": "v1.2.3-rc.1", "PR": "101", "SHA": HEAD}
        for change, error in (
            ({}, None), (prerelease, None),
            ({"SHA": HEAD}, "leave sha empty"), ({"PR": "101", "SHA": HEAD}, "stable tags"),
            ({"TAG": "v1.2.3-rc.1"}, "exact commit"), (prerelease | {"SHA": ""}, "exact commit"),
            ({"TAG": "v1.2.3-rc.1", "SHA": MAIN}, None), ({"TAG": "v1.2.3-rc.1", "SHA": HEAD}, "builds main"),
            ({"REF": "refs/heads/feature"}, "dispatched from main"),
            ({"EVENT_NAME": "push"}, "dispatched from main"),
            ({"WORKFLOW_REF": f"{REPO}/.github/workflows/release-request.yml@refs/heads/x"},
             "release request workflow on main"),
            ({"ACTOR": "other"}, "repository owner"),
            ({"TRIGGERING_ACTOR": "other"}, "repository owner"),
            ({"REQUEST_RUN_ATTEMPT": "2"}, "reruns"), ({"MODE": "force"}, "mode"),
        ):
            with tempfile.TemporaryDirectory() as directory, self.subTest(change=change):
                output = Path(directory) / "output"
                env = base | change | {"OUT_DIR": directory, "GITHUB_OUTPUT": str(output),
                                       "RUNNER_TEMP": tempfile.gettempdir()}
                with patch.dict(os.environ, env):
                    outcome(self, error, command_prepare)
                if error is not None:
                    continue
                request = json.loads((Path(directory) / "request.json").read_text())
                pr = "PR" in change
                self.assertEqual((request["schemaVersion"], request["sourceSha"], request["pr"]),
                                 (4, HEAD if pr else MAIN, 101 if pr else 0))
                self.assertIn(f"remote_sha={HEAD if pr else ''}\n", output.read_text())

    def test_publication_requires_main_or_pr_trust_for_the_release_kind(self) -> None:
        stable, prerelease = release_of(True), release_of(False)
        with github(trusted(True)):
            self.assertEqual(require_publishable(REPO, stable), (MAIN, 5151, ""))
        with github(trusted(True)):
            self.assertEqual(require_publishable(REPO, Release("v1.2.3-rc.1", MAIN, 0)), (MAIN, 5151, ""))
        with github(trusted(False)):
            self.assertEqual(require_publishable(REPO, prerelease), (MAIN, 5151, "77"))
        skipped = {JOBS: pages({"jobs": [
            GATE, {"name": "Core checks", "status": "completed", "conclusion": "skipped"}]})}
        tag = {TAG_REFS: [{"ref": "refs/tags/v1.2.3", "object": {"type": "commit", "sha": OLD}}]}
        for release, change, error in (
            (stable, skipped, "did not run every job: Core checks"),
            (stable, tag, "already exists"),
            (stable, {CODEQL: pages([analysis(99, 1, "failed")])}, "has errors"),
            (Release("v1.2.3", HEAD, 0), {}, "no longer current main"),
            (Release("v1.2.3-rc.1", HEAD, 0), {}, "no longer current main"),
            (prerelease, {tree(HEAD): {"tree": []}}, "PR changes scripts"),
        ):
            with (self.subTest(error=error), github(trusted(release.stable) | change),
                  self.assertRaisesRegex(ControlPlaneError, error)):
                require_publishable(REPO, release)

    def test_consumer_binds_the_request_artifact_to_its_trusted_run(self) -> None:
        request: dict[str, object] = {
            "schemaVersion": 4, "repository": REPO, "tag": "v1.2.3", "sourceSha": MAIN, "pr": 0,
            "mode": "publish", "requestRunId": 4242, "requestRunAttempt": 1,
        }
        prerelease = {"tag": "v1.2.3-rc.1", "sourceSha": HEAD, "pr": 101}
        base_env = {
            "REPOSITORY": REPO, "REPOSITORY_OWNER": "zR-JB", "PUBLISHER_SHA": MAIN,
            "WORKFLOW_REF": f"{REPO}/.github/workflows/release.yml@refs/heads/main",
            "REQUEST_RUN_ID": "4242",
        }
        for change, env, files, error in (
            ({}, {}, None, None), (prerelease, {}, None, None),
            # The artifact holds exactly the request and the release's staged files.
            ({}, {}, staged_files("1.2.3-rc.1"), "files are"), (prerelease, {}, staged_files("1.2.3"), "files are"),
            ({}, {"EXTRA": "release-assets-4242"}, None, "downloaded artifacts"),
            ({"schemaVersion": 3}, {}, None, "schemaVersion"),
            ({"rust": True}, {}, None, "keys are"),
            ({"sourceSha": HEAD}, {}, None, "trusted main commit"),
            ({"requestRunAttempt": True}, {}, None, "requestRunAttempt"),
            ({"requestRunId": 1}, {}, None, "requestRunId"),
            ({"tag": "v1.2.3-rc.1"}, {}, None, None),
            ({"tag": "v1.2.3-rc.1", "sourceSha": HEAD}, {}, None, "trusted main commit"),
            ({"mode": "force"}, {}, None, "mode"),
            ({"tag": "v1.2.4", "title": {"tag": "v1.2.3"}}, {}, None, "dispatch inputs"),
            (prerelease | {"title": {"pr": 0, "sourceSha": MAIN}}, {}, None, "dispatch inputs"),
            ({}, {"WORKFLOW_REF": f"{REPO}/.github/workflows/release.yml@refs/heads/x"}, None,
             "trusted main workflow"),
            ({}, {"HEAD": OLD}, None, "checked-out tooling"),
        ):
            with tempfile.TemporaryDirectory() as directory, self.subTest(change=change, env=env, files=files):
                root = Path(directory) / "request"
                candidate = root / "release-request-4242"
                candidate.mkdir(parents=True)
                record = request | {key: value for key, value in change.items() if key != "title"}
                for name in files or staged_files(str(record["tag"])[1:]):
                    (candidate / name).write_text("x")
                (candidate / "request.json").write_text(json.dumps(record))
                if "EXTRA" in env:
                    (root / env["EXTRA"]).mkdir()
                inputs = record | cast(dict[str, object], change.get("title", {}))
                title = request_title(str(inputs["mode"]), Release(
                    str(inputs["tag"]), str(inputs["sourceSha"]), cast(int, inputs["pr"])), MAIN)
                checkout = git_head(Path(directory), env.get("HEAD", MAIN))
                with (patch.dict(os.environ, base_env | env | checkout),
                      github(trusted(True) | {REQUEST_RUN: dispatch_run(31337, 4242, title)})):
                    verified = outcome(self, error, lambda: verify_request(root))
                if verified is not None:
                    release, publish = verified
                    self.assertEqual((release.sha, publish), ((request | change)["sourceSha"], True))


def write_request(root: Path, stable: bool, mode: str) -> tuple[Path, str]:
    """Write a request's artifact; return its directory and the image index digest."""
    release = release_of(stable)
    request_dir = root / "request"
    candidate = request_dir / "release-request-4242"
    digest = write_staged(candidate, release.version, release.sha, REPO)
    (candidate / "request.json").write_text(json.dumps({
        "schemaVersion": 4, "repository": REPO, "tag": release.tag, "sourceSha": release.sha,
        "pr": release.pr, "mode": mode, "requestRunId": 4242, "requestRunAttempt": 1,
    }))
    return request_dir, digest


def outputs(path: Path) -> dict[str, str]:
    return dict(line.split("=", 1) for line in path.read_text().splitlines())


Edit = Callable[[Path], None]


class CommandTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def test_verify_hands_off_only_a_verified_authorized_candidate(self) -> None:
        def checksum(candidate: Path) -> None:
            (candidate / f"{OCI}.sha256").write_text(f"0  {OCI}\n")

        def client(candidate: Path) -> None:
            write_client(candidate, "1.2.3", "linux/arm64", {"THIRD_PARTY_NOTICES.txt": b"other\n"})

        def offer(candidate: Path) -> None:
            (candidate / "graphite-meter_1.2.3-rc.1_third-party-source.tar.gz").write_bytes(b"x")

        unprotected = {ENVIRONMENT: REVIEWED | {"protection_rules": []}}
        # An earlier run published v1.2.3 with other assets.
        published = published_with("x.tar.gz", draft=False)
        rows: tuple[tuple[bool, str, Edit | None, dict[str, object], dict[str, str], str | None], ...] = (
            (True, "publish", None, {}, {}, None),
            (False, "validate", None, {}, {}, None),
            (False, "publish", None, {}, {}, None),
            (True, "publish", checksum, {}, {}, "does not match its checksum"),
            (True, "publish", None, {}, {"LIMIT": "10"}, "exceeds 10 bytes"),
            (True, "publish", client, {}, {}, "notices differ from its build's"),
            (False, "publish", offer, {}, {}, "files are"),
            (True, "publish", None, unprotected, {}, "require reviewers"),
            (True, "publish", None, {MAIN_COMMIT: {"sha": OLD}}, {}, "no longer current main"),
            (False, "publish", None, MOVED | {MAIN_COMMIT: {"sha": OLD}}, {}, "main moved during"),
            (True, "publish", None, published, {}, "already published with other assets"),
            (True, "validate", None, published, {}, "already published with other assets"),
            (True, "publish", None, published_with("x.tar.gz", draft=True), {}, None),
        )
        for stable, mode, edit, responses, env, error in rows:
            with self.subTest(stable=stable, mode=mode, error=error):
                root = self.root / str(len(list(self.root.iterdir())))
                request_dir, digest = write_request(root, stable, mode)
                if edit is not None:
                    edit(request_dir / "release-request-4242")
                release = release_of(stable)
                variables = {
                    "REPOSITORY": REPO, "REPOSITORY_OWNER": "zR-JB", "PUBLISHER_SHA": MAIN,
                    "WORKFLOW_REF": f"{REPO}/.github/workflows/release.yml@refs/heads/main",
                    "REQUEST_RUN_ID": "4242", "REQUEST_DIR": str(request_dir),
                    "HANDOFF_DIR": str(root / "handoff"),
                    "GITHUB_OUTPUT": str(root / "output"),
                    "GITHUB_STEP_SUMMARY": str(root / "summary"), "RUNNER_TEMP": str(self.root),
                } | git_head(root, MAIN) | env
                limit = int(env.get("LIMIT", 1 << 30))
                with (patch.dict(os.environ, variables), patch("assets.OCI_LIMIT", limit),
                      github(trusted(stable, mode) | responses)):
                    outcome(self, error, command_verify)
                if error is not None:
                    self.assertFalse((root / "handoff").exists())
                    continue
                result = outputs(root / "output")
                self.assertEqual((result["digest"], result["publish"], result["sha"]),
                                 (digest, str(mode == "publish").lower(), release.sha))
                self.assertEqual(file_sha256(root / "handoff/image" / OCI), result["oci_sha256"])
                if not stable:
                    self.assertEqual({path.name for path in (root / "handoff").iterdir()}, {"image"})
                    continue
                handoff = root / "handoff/assets"
                self.assertEqual(result["assets_sha256"], assets_sha256(handoff))
                self.assertEqual({path.name for path in handoff.iterdir()}, release_assets("1.2.3") | {"checksums.txt"})
                self.assertEqual((handoff / "checksums.txt").read_text(), "".join(
                    f"{file_sha256(handoff / name)}  {name}\n" for name in sorted(release_assets("1.2.3"))))

    def test_recheck_reauthorizes_the_exact_handoff_after_approval(self) -> None:
        handoff = self.root / "handoff"
        (handoff / "image").mkdir(parents=True)
        (handoff / "image" / OCI).write_bytes(b"verified")
        (handoff / "assets").mkdir()
        for name in release_assets("1.2.3"):
            (handoff / "assets" / name).write_text(name)
        write_checksums(handoff / "assets")
        closed = {PULL: PR | {"state": "closed"}}
        for stable, env, responses, error in (
            (True, {}, {}, None), (False, {}, {}, None),
            (True, {"OCI_SHA256": "0" * 64}, {}, "OCI handoff"),
            (True, {"ASSETS_SHA256": "0" * 64}, {}, "asset handoff"),
            (True, {"HEAD": OLD}, {}, "checked-out tooling"),
            (True, {}, published_with("x.tar.gz", draft=False), "already published with other assets"),
            (True, {}, published_with(*sorted(path.name for path in (handoff / "assets").iterdir()), draft=False,
                                      directory=handoff / "assets"), None),
            (False, {}, closed, "not open against main"),
            (False, {}, MOVED | {MAIN_COMMIT: {"sha": OLD}}, "main moved after verification"),
        ):
            release = release_of(stable)
            with (tempfile.TemporaryDirectory() as directory,
                  self.subTest(stable=stable, env=env, error=error)):
                variables = {
                    "MAIN_SHA": MAIN, "PR": str(release.pr or ""), "TAG": release.tag,
                    "SOURCE_SHA": release.sha, "REPOSITORY": REPO, "HANDOFF_DIR": str(handoff),
                    "OCI_SHA256": hashlib.sha256(b"verified").hexdigest(),
                    "ASSETS_SHA256": assets_sha256(handoff / "assets"),
                    "GITHUB_STEP_SUMMARY": str(Path(directory) / "summary"),
                    "RUNNER_TEMP": tempfile.gettempdir(),
                } | git_head(Path(directory), env.get("HEAD", MAIN)) | env
                with patch.dict(os.environ, variables), github(trusted(stable) | responses):
                    outcome(self, error, command_recheck)
                if error is None:
                    self.assertIn(f"authorized on main `{MAIN}`",
                                  (Path(directory) / "summary").read_text())


if __name__ == "__main__":
    unittest.main()
