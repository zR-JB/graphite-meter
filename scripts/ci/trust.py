#!/usr/bin/env python3
"""Trust predicates shared by stable releases and PR prereleases."""

from __future__ import annotations

import os
import re
import subprocess
from datetime import datetime
from pathlib import Path

from . import github_api as gh

SHA_RE = re.compile(r"[0-9a-f]{40}")
SEMVER_NUMBER = r"(?:0|[1-9][0-9]*)"
CONTROL_PLANE = (".github", ".githooks", "scripts", "mise.toml", "mise.lock")
DONE = ("completed", "success")
ENVIRONMENT = "ghcr-release"


def env(name: str) -> str:
    if not (value := os.environ.get(name)):
        gh.fail(f"{name} is required")
    return value


def env_int(name: str) -> int:
    if re.fullmatch(r"[1-9][0-9]*", value := env(name)) is None:
        gh.fail(f"{name} must be a positive integer")
    return int(value)


def env_sha(name: str) -> str:
    if SHA_RE.fullmatch(value := env(name)) is None:
        gh.fail(f"{name} must be a 40-character commit SHA")
    return value


def require_checkout(sha: str) -> None:
    head = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True, check=False)
    if head.returncode != 0 or head.stdout.strip() != sha:
        gh.fail(f"checked-out tooling does not match trusted commit {sha}")


def exact_files(directory: Path, names: set[str]) -> None:
    entries = list(directory.iterdir()) if directory.is_dir() else []
    if (found := {entry.name for entry in entries}) != names:
        gh.fail(f"{directory.name} files are {sorted(found)}; expected {sorted(names)}")
    for entry in entries:
        if entry.is_symlink() or not entry.is_file():
            gh.fail(f"{entry.name} is not a regular file")


def read_record(path: Path, keys: set[str], expected: gh.JsonObject) -> gh.JsonObject:
    record = gh.expect_object(gh.decode_json(path.read_text(encoding="utf-8"), path.name), path.name)
    if set(record) != keys:
        gh.fail(f"{path.name} keys are {sorted(record)}; expected {sorted(keys)}")
    for key, value in expected.items():
        if type(record[key]) is not type(value) or record[key] != value:
            gh.fail(f"{path.name} {key} does not match its trusted request run")
    return record


def _number(value: gh.JsonValue) -> int:
    return value if isinstance(value, int) and not isinstance(value, bool) else 0


def _text(value: gh.JsonValue) -> str:
    return value if isinstance(value, str) else ""


def _get(value: gh.JsonValue, key: str) -> gh.JsonValue:
    return value.get(key) if isinstance(value, dict) else None


def _bound_to_pr(item: gh.JsonObject, pr_number: int | None) -> bool:
    if pr_number is None:
        return True
    prs = gh.expect_array(item.get("pull_requests") or [], "pull_requests")
    numbers = [gh.expect_object(pr, "pull request").get("number") for pr in prs]
    return numbers.count(pr_number) == 1


def require_pr(repository: str, pr_number: int, sha: str) -> str:
    """Return the head branch of an open same-repository PR at exactly `sha`."""
    pr = gh.expect_object(gh.api(f"repos/{repository}/pulls/{pr_number}"), f"PR #{pr_number}")
    head = gh.object_field(pr, "head", f"PR #{pr_number}")
    if pr.get("state") != "open" or gh.object_field(pr, "base", "PR").get("ref") != "main":
        gh.fail(f"PR #{pr_number} is not open against main")
    if gh.object_field(head, "repo", "PR head").get("full_name") != repository:
        gh.fail("fork PRs cannot publish prereleases")
    if head.get("sha") != sha:
        gh.fail(f"PR #{pr_number} head SHA changed")
    return gh.str_field(head, "ref", "PR head")


def current_main(repository: str) -> str:
    sha = gh.str_field(gh.expect_object(gh.api(f"repos/{repository}/commits/main"), "main"), "sha", "main")
    if SHA_RE.fullmatch(sha) is None:
        gh.fail("could not resolve current main SHA")
    return sha


def require_exact_current_main(repository: str, sha: str) -> str:
    if (main := current_main(repository)) != sha:
        gh.fail(f"{sha} is no longer current main; current main is {main}")
    return main


def require_current_main(repository: str, pr_number: int, sha: str) -> str:
    """Require the PR head to contain current main and return that main SHA."""
    main = current_main(repository)
    comparison = gh.expect_object(gh.api(f"repos/{repository}/compare/{main}...{sha}"), "comparison")
    base = gh.str_field(gh.object_field(comparison, "merge_base_commit", "comparison"), "sha", "base")
    if gh.int_field(comparison, "behind_by", "comparison") != 0 or base != main:
        gh.fail(f"PR #{pr_number} is behind current main {main}; update it and let CI pass again")
    return main


def require_control_plane_matches_main(repository: str, pr_sha: str, main_sha: str) -> None:
    def entries(ref: str) -> dict[str, gh.JsonValue]:
        tree = gh.expect_object(gh.api(f"repos/{repository}/git/trees/{ref}"), f"tree at {ref}")
        items = [gh.expect_object(item, "entry") for item in gh.expect_array(tree.get("tree"), "tree")]
        return {_text(item.get("path")): item.get("sha") for item in items}

    pr, main = entries(pr_sha), entries(main_sha)
    if changed := [path for path in CONTROL_PLANE if pr.get(path) != main.get(path)]:
        gh.fail(f"PR changes {', '.join(changed)}; prereleases need main's CI control plane")


def timestamp(item: gh.JsonObject, key: str) -> datetime:
    try:
        return datetime.fromisoformat(gh.str_field(item, key, "GitHub record"))
    except ValueError as exc:
        raise gh.ControlPlaneError(f"{key} is not an ISO 8601 time") from exc


def require_dispatch_run(
    repository: str, owner: str, main_sha: str, run_id: int, workflow: str, title: str,
    artifacts: dict[str, tuple[str, int]],
) -> None:
    """Bind a request run to its workflow, inputs, main, one attempt, the owner and artifacts.

    `artifacts` maps each name to the job that writes it and a size limit. An artifact must have been
    written while its job ran, so a job that runs the requested source natively cannot replace another's.
    """
    workflow_id = gh.int_field(gh.expect_object(gh.api(f"repos/{repository}/actions/workflows/{workflow}"),
                                          workflow), "id", workflow)
    run = gh.expect_object(gh.api(f"repos/{repository}/actions/runs/{run_id}"), "request run")
    if run.get("id") != run_id or run.get("workflow_id") != workflow_id:
        gh.fail(f"run {run_id} is not a {workflow} run")
    if run.get("display_title") != title:
        gh.fail("request artifact does not match the dispatch inputs in the run title")
    if run.get("event") != "workflow_dispatch" or run.get("head_branch") != "main":
        gh.fail("request was not dispatched from main")
    if run.get("head_sha") != main_sha:
        gh.fail("main changed after the request; start a fresh request")
    if run.get("run_attempt") != 1:
        gh.fail("request reruns are not accepted; start a fresh dispatch")
    if (run.get("status"), run.get("conclusion")) != DONE:
        gh.fail(f"request run is {run.get('status')}/{run.get('conclusion')}")
    for key in ("actor", "triggering_actor"):
        if gh.object_field(run, key, "request run").get("login") != owner:
            gh.fail("request was not initiated by the repository owner")
    pages = gh.api(gh.query(f"repos/{repository}/actions/runs/{run_id}/artifacts", per_page=100),
                   paginate=True)
    unexpired = [item for item in gh.page_items(pages, "artifacts") if item.get("expired") is False]
    jobs = gh.page_items(gh.api(gh.query(f"repos/{repository}/actions/runs/{run_id}/jobs", filter="latest",
                                         per_page=100), paginate=True), "jobs")
    for name, (job, limit) in artifacts.items():
        if len(matches := [item for item in unexpired if item.get("name") == name]) != 1:
            gh.fail(f"expected one unexpired artifact {name}, found {len(matches)}")
        if not 0 <= gh.int_field(matches[0], "size_in_bytes", name) <= limit:
            gh.fail(f"artifact {name} exceeds {limit} bytes")
        if len(found := [item for item in jobs if item.get("name") == job]) != 1 or (
                found[0].get("status"), found[0].get("conclusion")) != DONE:
            gh.fail(f"request run has no single successful job {job!r}")
        if not (timestamp(found[0], "started_at") <= timestamp(matches[0], "created_at")
                <= timestamp(matches[0], "updated_at") <= timestamp(found[0], "completed_at")):
            gh.fail(f"artifact {name} was not written while its job {job!r} ran")


def require_ci_gate(
    repository: str, sha: str, *, event: str, branch: str, pr_number: int | None = None,
) -> int:
    """Require Gate in the newest `ci.yml` run for exactly this commit, branch and PR."""
    pages = gh.api(gh.query(f"repos/{repository}/actions/workflows/ci.yml/runs",
                         event=event, head_sha=sha, per_page=100), paginate=True)
    identity = (sha, branch, event)
    runs = [run for run in gh.page_items(pages, "workflow_runs") if _bound_to_pr(run, pr_number)
            and (run.get("head_sha"), run.get("head_branch"), run.get("event")) == identity]
    scope = f"PR #{pr_number}" if pr_number is not None else branch
    if not runs:
        gh.fail(f"CI for {scope} at {sha} is missing")
    run = max(runs, key=lambda item: (_number(item.get("run_number")),
                                      _number(item.get("run_attempt")),
                                      _text(item.get("updated_at"))))
    run_id = gh.int_field(run, "id", "CI run")
    if (run.get("status"), run.get("conclusion")) != DONE:
        gh.fail(f"latest CI run {run_id} for {scope} at {sha} is "
               f"{run.get('status')}/{run.get('conclusion')}")
    pages = gh.api(gh.query(f"repos/{repository}/actions/runs/{run_id}/jobs", filter="latest",
                         per_page=100), paginate=True)
    jobs = gh.page_items(pages, "jobs")
    gates = [job for job in jobs if job.get("name") == "Gate"]
    if [(gate.get("status"), gate.get("conclusion")) for gate in gates] != [DONE]:
        gh.fail(f"Gate in CI run {run_id} did not succeed")
    if event == "push" and (partial := [_text(job.get("name")) for job in jobs
                                        if (job.get("status"), job.get("conclusion")) != DONE]):
        gh.fail(f"CI run {run_id} on {branch} did not run every job: {', '.join(partial)}")
    return run_id


def require_check_run(
    repository: str, sha: str, *, name: str, app_slug: str, pr_number: int | None = None,
) -> int:
    pages = gh.api(gh.query(f"repos/{repository}/commits/{sha}/check-runs", per_page=100,
                         filter="all"), paginate=True)
    checks = [check for check in gh.page_items(pages, "check_runs")
              if check.get("name") == name and _get(check.get("app"), "slug") == app_slug
              and _bound_to_pr(check, pr_number)]
    scope = f"{name} for PR #{pr_number}" if pr_number is not None else name
    if not checks:
        gh.fail(f"{scope} at {sha} is missing")
    # Unfinished checks block; an older slow success must not hide a newer retry.
    check = max(checks, key=lambda item: (item.get("status") != "completed",
                                          _text(item.get("started_at")), _number(item.get("id"))))
    if (check.get("status"), check.get("conclusion")) != DONE:
        gh.fail(f"{scope} at {sha} is {check.get('status')}/{check.get('conclusion')}")
    return gh.int_field(check, "id", name)


def require_main_codeql(repository: str, sha: str) -> None:
    """Require the newest CodeQL analysis of every category at `sha` to be error-free."""
    pages = gh.api(gh.query(f"repos/{repository}/code-scanning/analyses", ref="refs/heads/main",
                         tool_name="CodeQL", per_page=100), paginate=True)
    def order(item: gh.JsonObject) -> tuple[str, int]:
        return _text(item.get("created_at")), _number(item.get("id"))

    matching = [item for item in gh.page_items(pages)
                if item.get("commit_sha") == sha and _get(item.get("tool"), "name") == "CodeQL"]
    identity = ("category", "analysis_key", "environment")
    newest = {tuple(_text(item.get(key)) for key in identity): item
              for item in sorted(matching, key=order)}
    if not newest:
        gh.fail(f"CodeQL analysis for {sha} is missing")
    if errors := [f"{key[0] or key[1]}: {item['error']}" for key, item in newest.items()
                  if _text(item.get("error"))]:
        gh.fail(f"latest CodeQL analysis for {sha} has errors: {'; '.join(errors)}")
    for item in newest.values():
        if warning := _text(item.get("warning")):
            print(f"::warning::CodeQL analysis warning for {sha}: {warning}")


def require_protected_environment(repository: str) -> None:
    """Require reviewers and a main-only deployment policy on the publishing environment."""
    path = f"repos/{repository}/environments/{ENVIRONMENT}"
    environment = gh.expect_object(gh.api(path), ENVIRONMENT)
    rules = [gh.expect_object(rule, "protection rule")
             for rule in gh.expect_array(environment.get("protection_rules") or [], "rules")]
    if not any(rule.get("type") == "required_reviewers" and rule.get("reviewers")
               for rule in rules):
        gh.fail(f"{ENVIRONMENT} must require reviewers")
    if _get(environment.get("deployment_branch_policy"), "custom_branch_policies") is not True:
        gh.fail(f"{ENVIRONMENT} must limit deployments to main")
    policies = gh.expect_object(gh.api(f"{path}/deployment-branch-policies"), "branch policies")
    branches = [(_get(item, "name"), _get(item, "type"))
                for item in gh.expect_array(policies.get("branch_policies"), "branch policies")]
    if branches != [("main", "branch")]:
        gh.fail(f"{ENVIRONMENT} must limit deployments to main")
