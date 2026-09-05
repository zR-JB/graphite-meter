#!/usr/bin/env python3
"""Typed trust predicates for CI, PR prereleases, and stable releases."""

from __future__ import annotations

from github_api import (
    APICall,
    JsonArray,
    JsonObject,
    JsonShapeError,
    JsonValue,
    api as default_api,
    array_field,
    expect_array,
    expect_object,
    int_field,
    object_field,
    query,
    str_field,
)


class TrustError(RuntimeError):
    pass


def _object(value: JsonValue, context: str) -> JsonObject:
    try:
        return expect_object(value, context)
    except JsonShapeError as exc:
        raise TrustError(str(exc)) from exc


def _array(value: JsonValue, context: str) -> JsonArray:
    try:
        return expect_array(value, context)
    except JsonShapeError as exc:
        raise TrustError(str(exc)) from exc


def _object_field(value: JsonObject, key: str, context: str) -> JsonObject:
    try:
        return object_field(value, key, context)
    except JsonShapeError as exc:
        raise TrustError(str(exc)) from exc


def _optional_array(value: JsonObject, key: str, context: str) -> JsonArray:
    item = value.get(key)
    if item is None:
        return []
    return _array(item, f"{context}.{key}")


def _flatten_objects(pages: JsonValue, key: str) -> list[JsonObject]:
    result: list[JsonObject] = []
    for index, page_value in enumerate(_array(pages, "GitHub pagination response")):
        page = _object(page_value, f"GitHub pagination page {index}")
        try:
            items = array_field(page, key, f"GitHub pagination page {index}")
        except JsonShapeError as exc:
            raise TrustError(str(exc)) from exc
        for item_index, item in enumerate(items):
            result.append(_object(item, f"{key}[{item_index}]"))
    return result


def _flatten_arrays(pages: JsonValue) -> list[JsonObject]:
    result: list[JsonObject] = []
    for page_index, page_value in enumerate(_array(pages, "GitHub pagination response")):
        page = _array(page_value, f"GitHub pagination page {page_index}")
        for item_index, item in enumerate(page):
            result.append(_object(item, f"GitHub pagination page {page_index}[{item_index}]"))
    return result


def require_pr(
    repository: str,
    pr_number: int,
    expected_sha: str,
    *,
    api: APICall = default_api,
) -> JsonObject:
    pr = _object(api(f"repos/{repository}/pulls/{pr_number}"), f"PR #{pr_number}")
    head = _object_field(pr, "head", f"PR #{pr_number}")
    base = _object_field(pr, "base", f"PR #{pr_number}")
    head_repo = _object_field(head, "repo", f"PR #{pr_number}.head")

    if pr.get("state") != "open":
        raise TrustError(f"PR #{pr_number} is not open")
    if base.get("ref") != "main":
        raise TrustError(f"PR #{pr_number} does not target main")
    if head_repo.get("full_name") != repository:
        raise TrustError("fork PRs cannot publish prereleases")
    if head.get("sha") != expected_sha:
        raise TrustError(f"PR #{pr_number} head SHA changed")
    return pr


def require_exact_current_main(
    repository: str,
    expected_sha: str,
    *,
    api: APICall = default_api,
) -> str:
    """Require an exact SHA to still be the repository's current main tip."""
    main = _object(api(f"repos/{repository}/commits/main"), "current main commit")
    try:
        main_sha = str_field(main, "sha", "current main commit")
    except JsonShapeError as exc:
        raise TrustError(str(exc)) from exc
    if len(main_sha) != 40:
        raise TrustError("could not resolve current main SHA")
    if main_sha != expected_sha:
        raise TrustError(
            f"release source {expected_sha} is no longer current main; current main is {main_sha}"
        )
    return main_sha


def require_current_main(
    repository: str,
    pr_number: int,
    expected_sha: str,
    *,
    api: APICall = default_api,
) -> str:
    """Require the PR head to contain the repository's current main tip."""
    main = _object(api(f"repos/{repository}/commits/main"), "current main commit")
    try:
        main_sha = str_field(main, "sha", "current main commit")
    except JsonShapeError as exc:
        raise TrustError(str(exc)) from exc
    if len(main_sha) != 40:
        raise TrustError("could not resolve current main SHA")

    comparison = _object(
        api(f"repos/{repository}/compare/{main_sha}...{expected_sha}"),
        "main/PR comparison",
    )
    try:
        behind_by = int_field(comparison, "behind_by", "main/PR comparison")
        merge_base = object_field(comparison, "merge_base_commit", "main/PR comparison")
        merge_base_sha = str_field(merge_base, "sha", "main/PR comparison.merge_base_commit")
    except JsonShapeError as exc:
        raise TrustError(str(exc)) from exc

    if behind_by != 0:
        raise TrustError(
            f"PR #{pr_number} is behind current main {main_sha} by {behind_by} commit(s); "
            "update the branch and let CI pass again"
        )
    if merge_base_sha != main_sha:
        raise TrustError(f"PR #{pr_number} does not contain current main {main_sha}")
    return main_sha


def require_file_matches_main(
    repository: str,
    path: str,
    pr_sha: str,
    main_sha: str,
    *,
    api: APICall = default_api,
) -> str:
    """Require a security-sensitive PR file to be byte-identical to current main."""

    def blob_sha(ref: str) -> str:
        value = _object(
            api(query(f"repos/{repository}/contents/{path}", ref=ref)),
            f"{path} at {ref}",
        )
        try:
            sha = str_field(value, "sha", f"{path} at {ref}")
        except JsonShapeError as exc:
            raise TrustError(str(exc)) from exc
        if len(sha) != 40:
            raise TrustError(f"could not resolve {path} at {ref}")
        return sha

    main_blob = blob_sha(main_sha)
    pr_blob = blob_sha(pr_sha)
    if pr_blob != main_blob:
        raise TrustError(
            f"PR changes {path}; prerelease authorization requires this CI control-plane file to match current main"
        )
    return main_blob




def workflow_id(
    repository: str,
    path: str,
    *,
    api: APICall = default_api,
) -> int:
    workflow = _object(api(f"repos/{repository}/actions/workflows/{path}"), f"workflow {path}")
    try:
        return int_field(workflow, "id", f"workflow {path}")
    except JsonShapeError as exc:
        raise TrustError(str(exc)) from exc


def actor_login(run: JsonObject, key: str, context: str) -> str:
    try:
        actor = object_field(run, key, context)
        return str_field(actor, "login", f"{context}.{key}")
    except JsonShapeError as exc:
        raise TrustError(str(exc)) from exc


def run_artifacts(
    repository: str,
    run_id: int,
    *,
    api: APICall = default_api,
) -> list[JsonObject]:
    pages = api(
        query(f"repos/{repository}/actions/runs/{run_id}/artifacts", per_page=100),
        paginate=True,
    )
    return _flatten_objects(pages, "artifacts")


def require_exact_artifact(
    repository: str,
    run_id: int,
    name: str,
    *,
    max_size: int,
    required: bool,
    api: APICall = default_api,
) -> None:
    matches = [
        artifact
        for artifact in run_artifacts(repository, run_id, api=api)
        if artifact.get("name") == name and artifact.get("expired") is False
    ]
    if not matches and not required:
        return
    if len(matches) != 1:
        raise TrustError(
            f"expected exactly one non-expired artifact named {name}, found {len(matches)}"
        )
    try:
        size = int_field(matches[0], "size_in_bytes", f"artifact {name}")
    except JsonShapeError as exc:
        raise TrustError(str(exc)) from exc
    if size < 0:
        raise TrustError(f"artifact {name} has an invalid size")
    if size > max_size:
        raise TrustError(f"artifact {name} is too large ({size} bytes; limit {max_size})")


def require_ci_gate(
    repository: str,
    sha: str,
    *,
    event: str,
    branch: str,
    pr_number: int | None = None,
    api: APICall = default_api,
) -> int:
    """Require Gate from an exact successful `ci.yml` workflow run."""
    run_pages = api(
        query(
            f"repos/{repository}/actions/workflows/ci.yml/runs",
            event=event,
            head_sha=sha,
            per_page=100,
        ),
        paginate=True,
    )
    runs = _flatten_objects(run_pages, "workflow_runs")

    def belongs_to_pr(run: JsonObject) -> bool:
        if pr_number is None:
            return True
        matches = 0
        for item in _optional_array(run, "pull_requests", "workflow run"):
            pr = _object(item, "workflow run pull request")
            if pr.get("number") == pr_number:
                matches += 1
        return matches == 1

    bound_runs = [
        run
        for run in runs
        if run.get("head_sha") == sha
        and run.get("head_branch") == branch
        and run.get("event") == event
        and belongs_to_pr(run)
    ]
    scope = f"PR #{pr_number}" if pr_number is not None else branch
    if not bound_runs:
        raise TrustError(f"CI for {scope} at {sha} is missing")

    def run_order(run: JsonObject) -> tuple[int, int, str]:
        run_number = run.get("run_number")
        run_attempt = run.get("run_attempt")
        updated_at = run.get("updated_at")
        return (
            run_number if isinstance(run_number, int) and not isinstance(run_number, bool) else 0,
            run_attempt if isinstance(run_attempt, int) and not isinstance(run_attempt, bool) else 0,
            updated_at if isinstance(updated_at, str) else "",
        )

    selected = max(bound_runs, key=run_order)
    try:
        run_id = int_field(selected, "id", "selected CI run")
    except JsonShapeError as exc:
        raise TrustError(str(exc)) from exc
    status = selected.get("status")
    conclusion = selected.get("conclusion")
    if status != "completed" or conclusion != "success":
        raise TrustError(
            f"latest CI run {run_id} for {scope} at {sha} is {status}/{conclusion}; "
            "an older successful run cannot authorize publication"
        )

    job_pages = api(
        query(f"repos/{repository}/actions/runs/{run_id}/jobs", filter="latest", per_page=100),
        paginate=True,
    )
    jobs = _flatten_objects(job_pages, "jobs")
    gates = [job for job in jobs if job.get("name") == "Gate"]
    if len(gates) != 1:
        raise TrustError(f"CI run {run_id} has {len(gates)} Gate jobs")
    gate = gates[0]
    if gate.get("status") != "completed" or gate.get("conclusion") != "success":
        raise TrustError(
            f"Gate in CI run {run_id} is {gate.get('status')}/{gate.get('conclusion')}"
        )
    return run_id


def require_check_run(
    repository: str,
    sha: str,
    *,
    name: str,
    app_slug: str,
    pr_number: int | None = None,
    api: APICall = default_api,
) -> int:
    pages = api(
        query(f"repos/{repository}/commits/{sha}/check-runs", per_page=100, filter="all"),
        paginate=True,
    )
    checks = _flatten_objects(pages, "check_runs")

    def belongs_to_pr(check: JsonObject) -> bool:
        if pr_number is None:
            return True
        matches = 0
        for item in _optional_array(check, "pull_requests", "check run"):
            pr = _object(item, "check run pull request")
            if pr.get("number") == pr_number:
                matches += 1
        return matches == 1

    matches: list[JsonObject] = []
    for check in checks:
        app_value = check.get("app")
        if not isinstance(app_value, dict):
            continue
        if (
            check.get("name") == name
            and app_value.get("slug") == app_slug
            and belongs_to_pr(check)
        ):
            matches.append(check)
    scope = f" for PR #{pr_number}" if pr_number is not None else ""
    if not matches:
        raise TrustError(f"{name}{scope} at {sha} is missing")

    def check_order(check: JsonObject) -> tuple[bool, str, int]:
        # Any unfinished matching check blocks publication, including queued
        # runs without a start time. Completed retries are ordered by start;
        # an older slow success must not hide a newer failed attempt.
        started = check.get("started_at")
        check_id = check.get("id")
        return (
            check.get("status") != "completed",
            started if isinstance(started, str) else "",
            check_id if isinstance(check_id, int) and not isinstance(check_id, bool) else 0,
        )

    selected = max(matches, key=check_order)
    if selected.get("status") != "completed" or selected.get("conclusion") != "success":
        raise TrustError(
            f"{name}{scope} at {sha} is {selected.get('status')}/{selected.get('conclusion')}"
        )
    try:
        return int_field(selected, "id", f"{name} check")
    except JsonShapeError as exc:
        raise TrustError(str(exc)) from exc


def require_main_codeql(
    repository: str,
    sha: str,
    *,
    api: APICall = default_api,
) -> None:
    pages = api(
        query(
            f"repos/{repository}/code-scanning/analyses",
            ref="refs/heads/main",
            tool_name="CodeQL",
            per_page=100,
        ),
        paginate=True,
    )

    analyses: list[JsonObject] = []
    for analysis in _flatten_arrays(pages):
        tool_value = analysis.get("tool")
        if not isinstance(tool_value, dict):
            continue
        if analysis.get("commit_sha") == sha and tool_value.get("name") == "CodeQL":
            analyses.append(analysis)
    if not analyses:
        raise TrustError(f"CodeQL analysis for {sha} is missing")

    # The endpoint is historical and a rerun can leave an older failed analysis
    # beside a newer success for the same category. Authorize only the newest
    # exact-SHA result in each analysis identity; never let an older success hide
    # a newer failure, and never let an older transient failure brick the SHA.
    def identity(analysis: JsonObject) -> tuple[str, str, str]:
        values: list[str] = []
        for key in ("category", "analysis_key", "environment"):
            value = analysis.get(key)
            values.append(value if isinstance(value, str) else "")
        return values[0], values[1], values[2]

    def order(analysis: JsonObject) -> tuple[str, int]:
        created = analysis.get("created_at")
        analysis_id = analysis.get("id")
        return (
            created if isinstance(created, str) else "",
            analysis_id if isinstance(analysis_id, int) and not isinstance(analysis_id, bool) else 0,
        )

    newest: dict[tuple[str, str, str], JsonObject] = {}
    for analysis in analyses:
        key = identity(analysis)
        previous = newest.get(key)
        if previous is None or order(analysis) > order(previous):
            newest[key] = analysis

    errors = [
        analysis
        for analysis in newest.values()
        if isinstance(analysis.get("error"), str) and analysis.get("error") != ""
    ]
    if errors:
        details: list[str] = []
        for analysis in errors:
            category = analysis.get("category")
            analysis_key = analysis.get("analysis_key")
            error = analysis.get("error")
            label = category if isinstance(category, str) and category else analysis_key
            details.append(f"{label if isinstance(label, str) and label else 'unknown'}: {error}")
        raise TrustError(f"latest CodeQL analysis for {sha} has errors: {'; '.join(details)}")

    for analysis in newest.values():
        warning = analysis.get("warning")
        if isinstance(warning, str) and warning:
            print(f"::warning::CodeQL analysis warning for {sha}: {warning}")
