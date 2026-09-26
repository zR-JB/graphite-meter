#!/usr/bin/env python3
"""Trust predicates shared by stable releases and PR prereleases."""

from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path
from typing import NoReturn

from github_api import (
    APICall,
    ControlPlaneError,
    JsonObject,
    JsonValue,
    api as default_api,
    decode_json,
    expect_array,
    expect_object,
    int_field,
    object_field,
    query,
    str_field,
)

SHA_RE = re.compile(r"[0-9a-f]{40}")
SEMVER_NUMBER = r"(?:0|[1-9][0-9]*)"
CONTROL_PLANE = (".github", ".githooks", "scripts", "mise.toml", "mise.lock")
DONE = ("completed", "success")
ENVIRONMENT = "ghcr-release"


class TrustError(ControlPlaneError):
    pass


def refuse(message: str) -> NoReturn:
    raise TrustError(message)


def env(name: str) -> str:
    if not (value := os.environ.get(name)):
        refuse(f"{name} is required")
    return value


def env_int(name: str) -> int:
    if re.fullmatch(r"[1-9][0-9]*", value := env(name)) is None:
        refuse(f"{name} must be a positive integer")
    return int(value)


def env_sha(name: str) -> str:
    if SHA_RE.fullmatch(value := env(name)) is None:
        refuse(f"{name} must be a 40-character commit SHA")
    return value


def require_checkout(sha: str) -> None:
    head = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True, check=False)
    if head.returncode != 0 or head.stdout.strip() != sha:
        refuse(f"checked-out tooling does not match trusted commit {sha}")


def exact_files(directory: Path, names: set[str]) -> None:
    entries = list(directory.iterdir()) if directory.is_dir() else []
    if (found := {entry.name for entry in entries}) != names:
        refuse(f"{directory.name} files are {sorted(found)}; expected {sorted(names)}")
    for entry in entries:
        if entry.is_symlink() or not entry.is_file():
            refuse(f"{entry.name} is not a regular file")


def read_record(path: Path, keys: set[str], expected: JsonObject) -> JsonObject:
    record = expect_object(decode_json(path.read_text(encoding="utf-8"), path.name), path.name)
    if set(record) != keys:
        refuse(f"{path.name} keys are {sorted(record)}; expected {sorted(keys)}")
    for key, value in expected.items():
        if type(record[key]) is not type(value) or record[key] != value:
            refuse(f"{path.name} {key} does not match its trusted request run")
    return record


def _objects(pages: JsonValue, key: str | None = None) -> list[JsonObject]:
    items: list[JsonObject] = []
    for page in expect_array(pages, "GitHub pages"):
        values = page if key is None else expect_object(page, "GitHub page").get(key)
        items += [expect_object(item, "item") for item in expect_array(values, "page")]
    return items


def _number(value: JsonValue) -> int:
    return value if isinstance(value, int) and not isinstance(value, bool) else 0


def _text(value: JsonValue) -> str:
    return value if isinstance(value, str) else ""


def _get(value: JsonValue, key: str) -> JsonValue:
    return value.get(key) if isinstance(value, dict) else None


def _bound_to_pr(item: JsonObject, pr_number: int | None) -> bool:
    if pr_number is None:
        return True
    prs = expect_array(item.get("pull_requests") or [], "pull_requests")
    numbers = [expect_object(pr, "pull request").get("number") for pr in prs]
    return numbers.count(pr_number) == 1


def require_pr(repository: str, pr_number: int, sha: str, *, api: APICall = default_api) -> str:
    """Return the head branch of an open same-repository PR at exactly `sha`."""
    pr = expect_object(api(f"repos/{repository}/pulls/{pr_number}"), f"PR #{pr_number}")
    head = object_field(pr, "head", f"PR #{pr_number}")
    if pr.get("state") != "open" or object_field(pr, "base", "PR").get("ref") != "main":
        refuse(f"PR #{pr_number} is not open against main")
    if object_field(head, "repo", "PR head").get("full_name") != repository:
        refuse("fork PRs cannot publish prereleases")
    if head.get("sha") != sha:
        refuse(f"PR #{pr_number} head SHA changed")
    return str_field(head, "ref", "PR head")


def current_main(repository: str, *, api: APICall = default_api) -> str:
    sha = str_field(expect_object(api(f"repos/{repository}/commits/main"), "main"), "sha", "main")
    if SHA_RE.fullmatch(sha) is None:
        refuse("could not resolve current main SHA")
    return sha


def require_exact_current_main(repository: str, sha: str, *, api: APICall = default_api) -> str:
    if (main := current_main(repository, api=api)) != sha:
        refuse(f"{sha} is no longer current main; current main is {main}")
    return main


def require_current_main(
    repository: str, pr_number: int, sha: str, *, api: APICall = default_api,
) -> str:
    """Require the PR head to contain current main and return that main SHA."""
    main = current_main(repository, api=api)
    comparison = expect_object(api(f"repos/{repository}/compare/{main}...{sha}"), "comparison")
    base = str_field(object_field(comparison, "merge_base_commit", "comparison"), "sha", "base")
    if int_field(comparison, "behind_by", "comparison") != 0 or base != main:
        refuse(f"PR #{pr_number} is behind current main {main}; update it and let CI pass again")
    return main


def require_control_plane_matches_main(
    repository: str, pr_sha: str, main_sha: str, *, api: APICall = default_api,
) -> None:
    def entries(ref: str) -> dict[str, JsonValue]:
        tree = expect_object(api(f"repos/{repository}/git/trees/{ref}"), f"tree at {ref}")
        items = [expect_object(item, "entry") for item in expect_array(tree.get("tree"), "tree")]
        return {_text(item.get("path")): item.get("sha") for item in items}

    pr, main = entries(pr_sha), entries(main_sha)
    if changed := [path for path in CONTROL_PLANE if pr.get(path) != main.get(path)]:
        refuse(f"PR changes {', '.join(changed)}; prereleases need main's CI control plane")


def require_dispatch_run(
    repository: str, owner: str, main_sha: str, run_id: int, workflow: str, title: str,
    artifacts: dict[str, int], *, api: APICall = default_api,
) -> None:
    """Bind a request run to its workflow, inputs, main, one attempt, the owner and artifacts."""
    workflow_id = int_field(expect_object(api(f"repos/{repository}/actions/workflows/{workflow}"),
                                          workflow), "id", workflow)
    run = expect_object(api(f"repos/{repository}/actions/runs/{run_id}"), "request run")
    if run.get("id") != run_id or run.get("workflow_id") != workflow_id:
        refuse(f"run {run_id} is not a {workflow} run")
    if run.get("display_title") != title:
        refuse("request artifact does not match the dispatch inputs in the run title")
    if run.get("event") != "workflow_dispatch" or run.get("head_branch") != "main":
        refuse("request was not dispatched from main")
    if run.get("head_sha") != main_sha:
        refuse("main changed after the request; start a fresh request")
    if run.get("run_attempt") != 1:
        refuse("request reruns are not accepted; start a fresh dispatch")
    if (run.get("status"), run.get("conclusion")) != DONE:
        refuse(f"request run is {run.get('status')}/{run.get('conclusion')}")
    for key in ("actor", "triggering_actor"):
        if object_field(run, key, "request run").get("login") != owner:
            refuse("request was not initiated by the repository owner")
    pages = api(query(f"repos/{repository}/actions/runs/{run_id}/artifacts", per_page=100),
                paginate=True)
    unexpired = [item for item in _objects(pages, "artifacts") if item.get("expired") is False]
    for name, limit in artifacts.items():
        if len(matches := [item for item in unexpired if item.get("name") == name]) != 1:
            refuse(f"expected one unexpired artifact {name}, found {len(matches)}")
        if not 0 <= int_field(matches[0], "size_in_bytes", name) <= limit:
            refuse(f"artifact {name} exceeds {limit} bytes")


def require_ci_gate(
    repository: str, sha: str, *, event: str, branch: str, pr_number: int | None = None,
    api: APICall = default_api,
) -> int:
    """Require Gate in the newest `ci.yml` run for exactly this commit, branch and PR."""
    pages = api(query(f"repos/{repository}/actions/workflows/ci.yml/runs",
                      event=event, head_sha=sha, per_page=100), paginate=True)
    identity = (sha, branch, event)
    runs = [run for run in _objects(pages, "workflow_runs") if _bound_to_pr(run, pr_number)
            and (run.get("head_sha"), run.get("head_branch"), run.get("event")) == identity]
    scope = f"PR #{pr_number}" if pr_number is not None else branch
    if not runs:
        refuse(f"CI for {scope} at {sha} is missing")
    run = max(runs, key=lambda item: (_number(item.get("run_number")),
                                      _number(item.get("run_attempt")),
                                      _text(item.get("updated_at"))))
    run_id = int_field(run, "id", "CI run")
    if (run.get("status"), run.get("conclusion")) != DONE:
        refuse(f"latest CI run {run_id} for {scope} at {sha} is "
               f"{run.get('status')}/{run.get('conclusion')}")
    pages = api(query(f"repos/{repository}/actions/runs/{run_id}/jobs", filter="latest",
                      per_page=100), paginate=True)
    jobs = _objects(pages, "jobs")
    gates = [job for job in jobs if job.get("name") == "Gate"]
    if [(gate.get("status"), gate.get("conclusion")) for gate in gates] != [DONE]:
        refuse(f"Gate in CI run {run_id} did not succeed")
    if event == "push" and (partial := [_text(job.get("name")) for job in jobs
                                        if (job.get("status"), job.get("conclusion")) != DONE]):
        refuse(f"CI run {run_id} on {branch} did not run every job: {', '.join(partial)}")
    return run_id


def require_check_run(
    repository: str, sha: str, *, name: str, app_slug: str, pr_number: int | None = None,
    api: APICall = default_api,
) -> int:
    pages = api(query(f"repos/{repository}/commits/{sha}/check-runs", per_page=100,
                      filter="all"), paginate=True)
    checks = [check for check in _objects(pages, "check_runs")
              if check.get("name") == name and _get(check.get("app"), "slug") == app_slug
              and _bound_to_pr(check, pr_number)]
    scope = f"{name} for PR #{pr_number}" if pr_number is not None else name
    if not checks:
        refuse(f"{scope} at {sha} is missing")
    # Unfinished checks block; an older slow success must not hide a newer retry.
    check = max(checks, key=lambda item: (item.get("status") != "completed",
                                          _text(item.get("started_at")), _number(item.get("id"))))
    if (check.get("status"), check.get("conclusion")) != DONE:
        refuse(f"{scope} at {sha} is {check.get('status')}/{check.get('conclusion')}")
    return int_field(check, "id", name)


def require_main_codeql(repository: str, sha: str, *, api: APICall = default_api) -> None:
    """Require the newest CodeQL analysis of every category at `sha` to be error-free."""
    pages = api(query(f"repos/{repository}/code-scanning/analyses", ref="refs/heads/main",
                      tool_name="CodeQL", per_page=100), paginate=True)
    def order(item: JsonObject) -> tuple[str, int]:
        return _text(item.get("created_at")), _number(item.get("id"))

    matching = [item for item in _objects(pages)
                if item.get("commit_sha") == sha and _get(item.get("tool"), "name") == "CodeQL"]
    identity = ("category", "analysis_key", "environment")
    newest = {tuple(_text(item.get(key)) for key in identity): item
              for item in sorted(matching, key=order)}
    if not newest:
        refuse(f"CodeQL analysis for {sha} is missing")
    if errors := [f"{key[0] or key[1]}: {item['error']}" for key, item in newest.items()
                  if _text(item.get("error"))]:
        refuse(f"latest CodeQL analysis for {sha} has errors: {'; '.join(errors)}")
    for item in newest.values():
        if warning := _text(item.get("warning")):
            print(f"::warning::CodeQL analysis warning for {sha}: {warning}")


def require_protected_environment(repository: str, *, api: APICall = default_api) -> None:
    """Require reviewers and a main-only deployment policy on the publishing environment."""
    path = f"repos/{repository}/environments/{ENVIRONMENT}"
    environment = expect_object(api(path), ENVIRONMENT)
    rules = [expect_object(rule, "protection rule")
             for rule in expect_array(environment.get("protection_rules") or [], "rules")]
    if not any(rule.get("type") == "required_reviewers" and rule.get("reviewers")
               for rule in rules):
        refuse(f"{ENVIRONMENT} must require reviewers")
    if _get(environment.get("deployment_branch_policy"), "custom_branch_policies") is not True:
        refuse(f"{ENVIRONMENT} must limit deployments to main")
    policies = expect_object(api(f"{path}/deployment-branch-policies"), "branch policies")
    branches = [(_get(item, "name"), _get(item, "type"))
                for item in expect_array(policies.get("branch_policies"), "branch policies")]
    if branches != [("main", "branch")]:
        refuse(f"{ENVIRONMENT} must limit deployments to main")
