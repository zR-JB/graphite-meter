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

from .github_api import (
    ControlPlaneError, JsonObject, confined_path, file_sha256, local_path, runner_path,
)
from .fixtures import (
    AMD, Answers, engine, git_head, github, outcome, pages, write_archive, write_oci, write_release_assets,
)
from .release import (
    BUILD_JOB,
    COMMANDS,
    OCI,
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
)
from .trust import (
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
        "category": "go", "analysis_key": "default", "environment": "", "error": error,
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


def job(name: str, start: str, end: str) -> dict[str, object]:
    return {"name": name, "status": "completed", "conclusion": "success",
            "started_at": f"2026-08-15T{start}:00Z", "completed_at": f"2026-08-15T{end}:00Z"}


# An unrelated job cannot impersonate the expected artifact-producing job.
OTHER_JOB = "Unrelated job"
REQUEST_JOB_RUNS = pages({"jobs": [job(BUILD_JOB, "10:00", "10:20"), job(OTHER_JOB, "10:21", "10:40")]})


def artifacts(*names: str, size: int = 1024, expired: bool = False, written: str = "") -> object:
    """Each artifact as the job that writes it uploads it, unless it was `written` at another time."""
    def at(name: str) -> str:
        return f"2026-08-15T{written or ('10:30' if 'other' in name else '10:10')}:00Z"

    return pages({"artifacts": [{"name": name, "expired": expired, "size_in_bytes": size,
                                 "created_at": at(name), "updated_at": at(name)} for name in names]})


def release_of(stable: bool) -> Release:
    return Release("v1.2.3", MAIN, 0) if stable else Release("v1.2.3-rc.1", HEAD, 101)


def trusted(stable: bool, mode: str = "publish") -> dict[str, object]:
    """Every GitHub answer that authorizes a stable release or a PR #101 prerelease."""
    names = ["release-request-4242", "release-assets-4242"]
    responses: dict[str, object] = {
        MAIN_COMMIT: {"sha": MAIN}, REQUEST_WORKFLOW: {"id": 31337},
        REQUEST_RUN: dispatch_run(31337, 4242, request_title(mode, release_of(stable), MAIN)),
        ARTIFACTS: artifacts(*names), REQUEST_JOBS: REQUEST_JOB_RUNS, JOBS: pages({"jobs": [GATE]}),
    }
    if mode == "publish":
        responses |= {ENVIRONMENT: REVIEWED, POLICIES: MAIN_ONLY}
    if stable:
        run = ci_run(5151, "success") | {"head_sha": MAIN, "head_branch": "main",
                                          "event": "push", "pull_requests": []}
        return responses | {
            TAG_REFS: [], ci_runs("push", MAIN): pages({"workflow_runs": [run]}),
            CODEQL: pages([analysis(99, 1)]),
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
        paths = (*control, "go", "client")
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

    def test_stable_tags_have_no_pr_and_prerelease_tags_need_one(self) -> None:
        for tag, pr, ok in (
            ("v0.5.2", 0, True), ("v10.12.30-rc.7", 7, True), ("v0.5.2-alpha.0", 1, True),
            ("v0.5.2", 7, False), ("v0.5.2-rc.1", 0, False), ("v0.5.2-rc.1", -1, False),
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
        name, other = "release-request-4242", "other-asset-4242"
        expected = {name: (BUILD_JOB, 4096), other: (OTHER_JOB, 4096)}
        only_build = pages({"jobs": [job(BUILD_JOB, "10:00", "10:20")]})
        failed = pages({"jobs": [job(BUILD_JOB, "10:00", "10:20"),
                                 job(OTHER_JOB, "10:21", "10:40") | {"conclusion": "failure"}]})
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
            ("artifact", artifacts(name, other, expired=True), "expected one unexpired"),
            ("artifact", artifacts(name, name, other), "expected one unexpired"),
            ("artifact", artifacts(name, other, size=4097), "exceeds"),
            # Another job cannot replace the build job's artifacts,
            # and its own must come from its run.
            ("artifact", artifacts(name, other, written="10:30"), f"{name} was not written while"),
            ("artifact", artifacts(name, other, written="10:10"), f"{other} was not written while"),
            ("artifact", artifacts(name, other, written="10:41"), f"{name} was not written while"),
            ("jobs", only_build, "no single successful job 'Unrelated job'"),
            ("jobs", failed, "no single successful job 'Unrelated job'"),
        ):
            run, files, jobs = dispatch_run(7001, 4242), artifacts(name, other), REQUEST_JOB_RUNS
            if field == "artifact":
                files = value
            elif field == "jobs":
                jobs = value
            elif field is not None:
                run[field] = value
            with (self.subTest(field=field, error=error),
                  github({REQUEST_WORKFLOW: {"id": 7001}, ARTIFACTS: files, REQUEST_JOBS: jobs, REQUEST_RUN: run})):
                outcome(self, error, lambda: require_dispatch_run(
                    REPO, "zR-JB", MAIN, 4242, "release-request.yml", "title", expected))

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
                self.assertEqual(local_path("go/dist", os.getcwd()), Path("go/dist").resolve())
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
            ({"TAG": "v1.2.3-rc.1"}, "stable tags"), (prerelease | {"SHA": ""}, "SHA is required"),
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
                self.assertEqual((request["sourceSha"], request["pr"]),
                                 (HEAD, 101) if change else (MAIN, 0))
                self.assertIn(f"remote_sha={HEAD if change else ''}\n", output.read_text())

    def test_publication_requires_main_or_pr_trust_for_the_release_kind(self) -> None:
        stable, prerelease = release_of(True), release_of(False)
        with github(trusted(True)):
            self.assertEqual(require_publishable(REPO, stable), (MAIN, 5151, ""))
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
            (prerelease, {tree(HEAD): {"tree": []}}, "PR changes scripts"),
        ):
            with (self.subTest(error=error), github(trusted(release.stable) | change),
                  self.assertRaisesRegex(ControlPlaneError, error)):
                require_publishable(REPO, release)

    def test_consumer_binds_the_request_artifact_to_its_trusted_run(self) -> None:
        request: dict[str, object] = {
            "schemaVersion": 3, "rust": "none", "repository": REPO, "tag": "v1.2.3", "sourceSha": MAIN, "pr": 0,
            "mode": "publish", "requestRunId": 4242, "requestRunAttempt": 1,
        }
        prerelease = {"tag": "v1.2.3-rc.1", "sourceSha": HEAD, "pr": 101}
        base_env = {
            "REPOSITORY": REPO, "REPOSITORY_OWNER": "zR-JB", "PUBLISHER_SHA": MAIN,
            "WORKFLOW_REF": f"{REPO}/.github/workflows/release.yml@refs/heads/main",
            "REQUEST_RUN_ID": "4242",
        }
        for change, env, artifacts_present, error in (
            ({}, {}, True, None), (prerelease, {}, True, None),
            (prerelease, {}, False, "downloaded artifacts"), ({}, {}, False, "downloaded artifacts"),
            ({"sourceSha": HEAD}, {}, True, "trusted main commit"),
            ({"requestRunAttempt": True}, {}, True, "requestRunAttempt"),
            ({"requestRunId": 1}, {}, True, "requestRunId"),
            ({"tag": "v1.2.3-rc.1"}, {}, True, "stable tags"),
            ({"mode": "force"}, {}, True, "mode"),
            ({"tag": "v1.2.4", "title": {"tag": "v1.2.3"}}, {}, True, "dispatch inputs"),
            (prerelease | {"title": {"pr": 0, "sourceSha": MAIN}}, {}, False, "dispatch inputs"),
            ({}, {"WORKFLOW_REF": f"{REPO}/.github/workflows/release.yml@refs/heads/x"}, True,
             "trusted main workflow"),
            ({}, {"PUBLISHER_SHA": OLD, "HEAD": OLD}, True, "no longer current main"),
            ({}, {"HEAD": OLD}, True, "checked-out tooling"),
        ):
            with tempfile.TemporaryDirectory() as directory, self.subTest(change=change, env=env):
                root = Path(directory) / "request"
                candidate = root / "release-request-4242"
                candidate.mkdir(parents=True)
                for name in (OCI, f"{OCI}.sha256"):
                    (candidate / name).write_text("x")
                record = request | {key: value for key, value in change.items() if key != "title"}
                (candidate / "request.json").write_text(json.dumps(record))
                if artifacts_present:
                    (root / "release-assets-4242").mkdir()
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


def write_request(root: Path, stable: bool, mode: str) -> tuple[Path, JsonObject]:
    release = release_of(stable)
    request_dir = root / "request"
    candidate = request_dir / "release-request-4242"
    candidate.mkdir(parents=True)
    (candidate / "request.json").write_text(json.dumps({
        "schemaVersion": 3, "rust": "none", "repository": REPO, "tag": release.tag, "sourceSha": release.sha,
        "pr": release.pr, "mode": mode, "requestRunId": 4242, "requestRunAttempt": 1,
    }))
    oci = write_oci(candidate / OCI, REPO, release.sha, remote=not stable)
    (candidate / f"{OCI}.sha256").write_text(f"{file_sha256(candidate / OCI)}  {OCI}\n")
    write_release_assets(request_dir / "release-assets-4242", release.version, tuis=stable)
    return request_dir, oci


def outputs(path: Path) -> dict[str, str]:
    return dict(line.split("=", 1) for line in path.read_text().splitlines())


Edit = Callable[[Path], None]


class CommandTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def test_verify_hands_off_only_a_verified_authorized_candidate(self) -> None:
        def checksum(request_dir: Path) -> None:
            (request_dir / "release-request-4242" / f"{OCI}.sha256").write_text(f"0  {OCI}\n")

        def asset(request_dir: Path) -> None:
            (request_dir / "release-assets-4242" / "checksums.txt").write_text(
                "0" * 64 + "  graphite-meter_1.2.3_third-party-source.tar.gz\n")

        unprotected = {ENVIRONMENT: REVIEWED | {"protection_rules": []}}
        rows: tuple[tuple[bool, str, Edit | None, dict[str, object], dict[str, str],
                          str | None], ...] = (
            (True, "publish", None, {}, {}, None),
            (False, "validate", None, {}, {}, None),
            (True, "publish", checksum, {}, {}, "request checksum"),
            (True, "publish", None, {}, {"LIMIT": "10"}, "exceeds 10 bytes"),
            (True, "publish", asset, {}, {}, "checksum mismatch"),
            (True, "publish", None, {}, {"FAKE_DIGEST": "latest"}, "digest is 'latest'"),
            (True, "publish", None, unprotected, {}, "require reviewers"),
            (False, "publish", None, MOVED | {MAIN_COMMIT: Answers([{"sha": MAIN}, {"sha": OLD}])},
             {}, "main moved during"),
        )
        for stable, mode, edit, responses, env, error in rows:
            with self.subTest(stable=stable, mode=mode, error=error):
                root = self.root / str(len(list(self.root.iterdir())))
                request_dir, oci = write_request(root, stable, mode)
                if edit is not None:
                    edit(request_dir)
                release = release_of(stable)
                variables = {
                    "REPOSITORY": REPO, "REPOSITORY_OWNER": "zR-JB", "PUBLISHER_SHA": MAIN,
                    "WORKFLOW_REF": f"{REPO}/.github/workflows/release.yml@refs/heads/main",
                    "REQUEST_RUN_ID": "4242", "REQUEST_DIR": str(request_dir),
                    "HANDOFF_DIR": str(root / "handoff"),
                    "GITHUB_OUTPUT": str(root / "output"),
                    "GITHUB_STEP_SUMMARY": str(root / "summary"), "RUNNER_TEMP": str(self.root),
                } | engine(root, REPO, release.version, release.sha, oci) | git_head(root, MAIN) | env
                limit = int(env.get("LIMIT", 1 << 30))
                with (patch.dict(os.environ, variables), patch("scripts.ci.release.OCI_LIMIT", limit),
                      github(trusted(stable, mode) | responses)):
                    outcome(self, error, command_verify)
                if error is not None:
                    self.assertFalse((root / "handoff").exists())
                    continue
                result = outputs(root / "output")
                self.assertEqual((result["digest"], result["publish"], result["sha"]),
                                 (AMD, str(mode == "publish").lower(), release.sha))
                self.assertEqual(file_sha256(root / "handoff/image" / OCI), result["oci_sha256"])
                self.assertEqual(result["assets_sha256"], assets_sha256(request_dir / "release-assets-4242"))

    def test_a_prerelease_stages_the_image_source_offer_it_exported(self) -> None:
        name = "graphite-meter_1.2.3-rc.1_third-party-source.tar.gz"
        export, staged = self.root / "export", self.root / "staged"
        write_release_assets(export, "1.2.3-rc.1", tuis=False)
        environment = {"RUNNER_TEMP": str(self.root), "VERSION": "1.2.3-rc.1",
                       "SOURCE_EXPORT": str(export), "RELEASE_ASSETS": str(staged)}
        with patch.dict(os.environ, environment):
            COMMANDS["stage-source"]()
            self.assertEqual(sorted(path.name for path in staged.iterdir()), ["checksums.txt", name])
            write_archive(export / name, {"graphite-meter_1.2.3-rc.1_third-party-source/README.txt": b"x"})
            with self.assertRaisesRegex(ControlPlaneError, "source-offer metadata"):
                COMMANDS["stage-source"]()
        with patch.dict(os.environ, environment | {"VERSION": "/../../x"}):
            with self.assertRaisesRegex(ControlPlaneError, "is outside"):
                COMMANDS["stage-source"]()

    def test_recheck_reauthorizes_the_exact_handoff_after_approval(self) -> None:
        handoff = self.root / "handoff"
        (handoff / "image").mkdir(parents=True)
        (handoff / "image" / OCI).write_bytes(b"verified")
        (handoff / "rust-image").mkdir()
        (handoff / "rust-image" / OCI).write_bytes(b"verified Rust")
        write_release_assets(handoff / "assets", "1.2.3")
        closed = {PULL: PR | {"state": "closed"}}
        for stable, env, responses, error in (
            (True, {}, {}, None), (False, {}, {}, None),
            (False, {"RUST": "tui"}, {}, None),
            (False, {"RUST": "server", "RUST_OCI_SHA256": hashlib.sha256(b"verified Rust").hexdigest()}, {}, None),
            (False, {"RUST": "tui", "ASSETS_SHA256": "0" * 64}, {}, "asset handoff"),
            (False, {"RUST": "server", "RUST_OCI_SHA256": "0" * 64}, {}, "Rust OCI handoff"),
            (True, {"OCI_SHA256": "0" * 64}, {}, "OCI handoff"),
            (True, {"ASSETS_SHA256": "0" * 64}, {}, "asset handoff"),
            (True, {"HEAD": OLD}, {}, "checked-out tooling"),
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
