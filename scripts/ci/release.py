#!/usr/bin/env python3
"""Trusted stable-release request validation and post-approval recheck."""

from __future__ import annotations

import argparse
import os
import re
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import NoReturn

from github_api import (
    APICall,
    GitHubAPIError,
    JsonObject,
    JsonShapeError,
    api as default_api,
    append_output,
    append_summary,
    decode_json,
    expect_array,
    expect_object,
    int_field,
    object_field,
    str_field,
)
from trust import (
    TrustError,
    actor_login,
    require_ci_gate,
    require_exact_artifact,
    require_exact_current_main,
    require_main_codeql,
    workflow_id,
)

SEMVER_NUMBER = r"(?:0|[1-9][0-9]*)"
STABLE_SEMVER_RE = re.compile(rf"^v{SEMVER_NUMBER}\.{SEMVER_NUMBER}\.{SEMVER_NUMBER}$")
SEMVER_RE = re.compile(
    rf"^v{SEMVER_NUMBER}\.{SEMVER_NUMBER}\.{SEMVER_NUMBER}"
    rf"(?:-(?:alpha|beta|rc)\.{SEMVER_NUMBER})?$"
)
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
REQUEST_KEYS = {
    "schemaVersion",
    "repository",
    "sourceSha",
    "version",
    "mode",
    "requestRunId",
    "requestRunAttempt",
}
REQUEST_ARTIFACT_LIMIT = 64 * 1024


@dataclass(frozen=True)
class ReleaseContext:
    repository: str
    sha: str
    tag: str
    ci_run_id: int
    publish: bool
    request_run_id: int


def die(message: str) -> NoReturn:
    raise SystemExit(f"Release refused: {message}")


def env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        die(f"{name} is required")
    return value


def env_int(name: str) -> int:
    value = env(name)
    if re.fullmatch(r"[1-9][0-9]*", value) is None:
        die(f"{name} must be a positive integer")
    return int(value)


def git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        die(result.stderr.strip() or f"git {' '.join(args)} failed")
    return result.stdout.strip()


def require_compatible_release_tag(
    repository: str,
    tag: str,
    expected_sha: str,
    *,
    api: APICall = default_api,
) -> None:
    """Fail before publication if an exact release tag already points elsewhere."""
    try:
        refs = expect_array(
            api(f"repos/{repository}/git/matching-refs/tags/{tag}"),
            f"matching refs for {tag}",
        )
        exact = [
            expect_object(value, f"matching ref for {tag}")
            for value in refs
            if isinstance(value, dict) and value.get("ref") == f"refs/tags/{tag}"
        ]
        if not exact:
            return
        if len(exact) != 1:
            die(f"multiple exact refs unexpectedly match {tag}")
        obj = object_field(exact[0], "object", f"tag ref {tag}")
        object_type = str_field(obj, "type", f"tag ref {tag}.object")
        object_sha = str_field(obj, "sha", f"tag ref {tag}.object")
        if object_type == "commit":
            target = object_sha
        elif object_type == "tag":
            annotated = expect_object(
                api(f"repos/{repository}/git/tags/{object_sha}"),
                f"annotated tag {tag}",
            )
            annotated_obj = object_field(annotated, "object", f"annotated tag {tag}")
            if str_field(annotated_obj, "type", f"annotated tag {tag}.object") != "commit":
                die(f"{tag} does not ultimately reference a commit")
            target = str_field(annotated_obj, "sha", f"annotated tag {tag}.object")
        else:
            die(f"{tag} has unexpected object type {object_type}")
    except JsonShapeError as exc:
        die(str(exc))
    if target != expected_sha:
        die(f"{tag} already exists at {target}, expected {expected_sha}")


def exact_request_file(request_dir: Path) -> Path:
    if not request_dir.is_dir():
        die("stable release request directory is missing")
    entries = list(request_dir.iterdir())
    if len(entries) != 1 or entries[0].name != "request.json":
        die(
            "stable release request files are "
            f"{sorted(path.name for path in entries)}; expected ['request.json']"
        )
    path = entries[0]
    if not path.is_file() or path.is_symlink():
        die("stable release request.json is not a regular file")
    return path


def read_request(path: Path) -> JsonObject:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        die(f"cannot read stable release request: {exc}")
    try:
        request = expect_object(decode_json(text, "stable release request"), "stable release request")
    except JsonShapeError as exc:
        die(str(exc))
    if set(request) != REQUEST_KEYS:
        die(
            f"stable release request keys are {sorted(request)}; expected {sorted(REQUEST_KEYS)}"
        )
    return request


def validate_request_context(*, api: APICall = default_api) -> ReleaseContext:
    repository = env("REPOSITORY")
    owner = env("REPOSITORY_OWNER")
    publisher_sha = env("PUBLISHER_SHA")
    workflow_ref = env("WORKFLOW_REF")
    request_run_id = env_int("REQUEST_RUN_ID")
    request_dir = Path(env("REQUEST_DIR"))

    if SHA_RE.fullmatch(publisher_sha) is None:
        die("trusted publisher SHA is invalid")
    expected_workflow_ref = f"{repository}/.github/workflows/release.yml@refs/heads/main"
    if workflow_ref != expected_workflow_ref:
        die("stable release consumer is not the trusted main workflow")

    # workflow_run executes default-branch tooling. Require that exact tooling SHA
    # to still be current before trusting any request artifact.
    require_exact_current_main(repository, publisher_sha, api=api)
    if git("rev-parse", "HEAD") != publisher_sha:
        die("checked-out release tooling does not match trusted publisher SHA")

    expected_request_workflow_id = workflow_id(repository, "release-request.yml", api=api)
    run = expect_object(
        api(f"repos/{repository}/actions/runs/{request_run_id}"),
        "stable release request workflow run",
    )
    try:
        if int_field(run, "id", "stable release request workflow run") != request_run_id:
            die("stable release request run ID changed")
        if int_field(run, "workflow_id", "stable release request workflow run") != expected_request_workflow_id:
            die("stable release request did not originate from release-request.yml")
        event = str_field(run, "event", "stable release request workflow run")
        head_branch = str_field(run, "head_branch", "stable release request workflow run")
        head_sha = str_field(run, "head_sha", "stable release request workflow run")
        status = str_field(run, "status", "stable release request workflow run")
        conclusion = str_field(run, "conclusion", "stable release request workflow run")
        run_attempt = int_field(run, "run_attempt", "stable release request workflow run")
    except JsonShapeError as exc:
        die(str(exc))

    if event != "workflow_dispatch" or head_branch != "main":
        die("stable release request was not dispatched from main")
    if head_sha != publisher_sha:
        die(
            "main changed between the manual request and trusted release consumer; "
            "start a fresh stable release request"
        )
    if run_attempt != 1:
        die("stable release request reruns are not accepted; start a fresh dispatch")
    if status != "completed" or conclusion != "success":
        die(f"stable release request is {status}/{conclusion}")
    if actor_login(run, "actor", "stable release request workflow run") != owner or actor_login(
        run, "triggering_actor", "stable release request workflow run"
    ) != owner:
        die("stable release request was not initiated by the repository owner")

    artifact_name = f"stable-release-request-{request_run_id}"
    require_exact_artifact(
        repository,
        request_run_id,
        artifact_name,
        max_size=REQUEST_ARTIFACT_LIMIT,
        required=True,
        api=api,
    )
    request = read_request(exact_request_file(request_dir))
    try:
        schema_version = int_field(request, "schemaVersion", "stable release request")
        request_repository = str_field(request, "repository", "stable release request")
        request_sha = str_field(request, "sourceSha", "stable release request")
        tag = str_field(request, "version", "stable release request")
        mode = str_field(request, "mode", "stable release request")
        artifact_run_id = int_field(request, "requestRunId", "stable release request")
        artifact_attempt = int_field(request, "requestRunAttempt", "stable release request")
    except JsonShapeError as exc:
        die(str(exc))

    if schema_version != 1:
        die("unsupported stable release request schema")
    if request_repository != repository:
        die("stable release request repository mismatch")
    if request_sha != publisher_sha:
        die("stable release request source SHA does not match trusted current main")
    if artifact_run_id != request_run_id or artifact_attempt != 1:
        die("stable release request run metadata mismatch")
    if STABLE_SEMVER_RE.fullmatch(tag) is None:
        die("stable release version must be vMAJOR.MINOR.PATCH")
    if mode not in {"validate", "publish"}:
        die("stable release mode must be validate or publish")

    require_compatible_release_tag(repository, tag, publisher_sha, api=api)
    ci_run_id = require_ci_gate(repository, publisher_sha, event="push", branch="main", api=api)
    require_main_codeql(repository, publisher_sha, api=api)
    return ReleaseContext(
        repository=repository,
        sha=publisher_sha,
        tag=tag,
        ci_run_id=ci_run_id,
        publish=mode == "publish",
        request_run_id=request_run_id,
    )


def command_guard() -> None:
    context = validate_request_context()
    version = context.tag[1:]
    major, minor, _patch = version.split(".")
    append_output(
        tag=context.tag,
        version=version,
        series=f"{major}.{minor}",
        publish=str(context.publish).lower(),
    )
    append_summary(
        f"""### Stable release request accepted

| Property | Value |
|---|---|
| Request run | `{context.request_run_id}` |
| Repository | `{context.repository}` |
| Source | `{context.sha}` |
| Version | `{context.tag}` |
| Mode | `{'publish' if context.publish else 'validate'}` |
| Main CI run | `{context.ci_run_id}` |

The manual request was bound to **current `main`**, then revalidated by the trusted default-branch `workflow_run` consumer. Main CI Gate + current CodeQL analyses are valid.
{'Publication still requires approval from the protected `ghcr-release` environment.' if context.publish else 'Validation mode cannot reach any write-permission job.'}
"""
    )
    print(
        f"::notice::trusted release consumer accepted request {context.request_run_id} "
        f"for {context.tag} @ {context.sha}"
    )


def command_recheck() -> None:
    repository = env("REPOSITORY")
    source_sha = env("SOURCE_SHA")
    tag = env("REQUESTED_VERSION")
    if SHA_RE.fullmatch(source_sha) is None:
        die("release source SHA is invalid")
    if STABLE_SEMVER_RE.fullmatch(tag) is None:
        die("stable release version must be vMAJOR.MINOR.PATCH")
    if git("rev-parse", "HEAD") != source_sha:
        die("checked-out release tooling does not match the authorized source SHA")

    require_exact_current_main(repository, source_sha)
    require_compatible_release_tag(repository, tag, source_sha)
    ci_run_id = require_ci_gate(repository, source_sha, event="push", branch="main")
    require_main_codeql(repository, source_sha)
    append_output(ci_run_id=ci_run_id)
    append_summary(
        f"""### Final publication trust recheck passed

`{tag}` is still bound to current `main` `{source_sha}` **after environment approval**.
Main CI run `{ci_run_id}` and the latest exact-SHA CodeQL analyses remain valid. Publication may proceed.
"""
    )
    print(f"::notice::final release trust recheck passed for {tag} @ {source_sha}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("guard", "recheck"))
    command = parser.parse_args().command
    try:
        {"guard": command_guard, "recheck": command_recheck}[command]()
    except (TrustError, GitHubAPIError, JsonShapeError) as exc:
        die(str(exc))


if __name__ == "__main__":
    main()
