#!/usr/bin/env python3
"""Typed guard and post-approval recheck for stable releases."""

from __future__ import annotations

import argparse
import os
import re
import subprocess
from dataclasses import dataclass
from typing import NoReturn

from github_api import (
    APICall,
    GitHubAPIError,
    JsonShapeError,
    api as default_api,
    append_output,
    append_summary,
    expect_array,
    expect_object,
    object_field,
    str_field,
)
from trust import TrustError, require_ci_gate, require_exact_current_main, require_main_codeql

SEMVER_NUMBER = r"(?:0|[1-9][0-9]*)"
STABLE_SEMVER_RE = re.compile(rf"^v{SEMVER_NUMBER}\.{SEMVER_NUMBER}\.{SEMVER_NUMBER}$")
SEMVER_RE = re.compile(
    rf"^v{SEMVER_NUMBER}\.{SEMVER_NUMBER}\.{SEMVER_NUMBER}"
    rf"(?:-(?:alpha|beta|rc)\.{SEMVER_NUMBER})?$"
)
SHA_RE = re.compile(r"^[0-9a-f]{40}$")


@dataclass(frozen=True)
class ReleaseContext:
    repository: str
    sha: str
    tag: str
    ci_run_id: int
    publish: bool


def die(message: str) -> NoReturn:
    raise SystemExit(f"Release refused: {message}")


def env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        die(f"{name} is required")
    return value


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


def validate_context() -> ReleaseContext:
    repository = env("REPOSITORY")
    owner = env("REPOSITORY_OWNER")
    actor = env("ACTOR")
    triggering_actor = env("TRIGGERING_ACTOR")
    event_name = env("EVENT_NAME")
    event_sha = env("EVENT_SHA")
    ref = env("REF")
    workflow_ref = env("WORKFLOW_REF")
    tag = env("REQUESTED_VERSION")
    mode = env("RELEASE_MODE")

    if event_name != "workflow_dispatch":
        die("stable releases must be initiated with workflow_dispatch")
    if ref != "refs/heads/main":
        die("release workflow must be dispatched from main")
    expected_workflow_ref = f"{repository}/.github/workflows/release.yml@refs/heads/main"
    if workflow_ref != expected_workflow_ref:
        die("release workflow is not the protected main workflow")
    if mode not in {"validate", "publish"}:
        die("release mode must be validate or publish")
    if mode == "publish" and (actor != owner or triggering_actor != owner):
        die("stable publication must be initiated and rerun by the repository owner")
    if SHA_RE.fullmatch(event_sha) is None:
        die("event SHA is invalid")
    if STABLE_SEMVER_RE.fullmatch(tag) is None:
        die("stable release version must be vMAJOR.MINOR.PATCH")

    head = git("rev-parse", "HEAD")
    if head != event_sha:
        die(f"checked-out source {head} does not match event SHA {event_sha}")

    require_exact_current_main(repository, event_sha)
    require_compatible_release_tag(repository, tag, event_sha)
    ci_run_id = require_ci_gate(repository, event_sha, event="push", branch="main")
    require_main_codeql(repository, event_sha)
    return ReleaseContext(
        repository=repository,
        sha=event_sha,
        tag=tag,
        ci_run_id=ci_run_id,
        publish=mode == "publish",
    )


def command_guard() -> None:
    context = validate_context()
    version = context.tag[1:]
    major, minor, _patch = version.split(".")
    append_output(
        sha=context.sha,
        tag=context.tag,
        version=version,
        series=f"{major}.{minor}",
        publish=str(context.publish).lower(),
    )
    append_summary(
        f"""### Release source accepted

| Property | Value |
|---|---|
| Repository | `{context.repository}` |
| Source | `{context.sha}` |
| Version | `{context.tag}` |
| Mode | `{'publish' if context.publish else 'validate'}` |
| Main CI run | `{context.ci_run_id}` |

The source is the **current `main` tip** and has successful main CI Gate + CodeQL.
{'Publication still requires approval from the protected `ghcr-release` environment.' if context.publish else 'Validation mode cannot reach any write-permission job.'}
"""
    )
    print(f"::notice::release guard accepted {context.tag} from current main {context.sha}")


def command_recheck() -> None:
    context = validate_context()
    if not context.publish:
        die("final release recheck is valid only for publish mode")
    append_summary(
        f"""### Final publication trust recheck passed

`{context.tag}` is still bound to current `main` `{context.sha}` **after environment approval**.
Main CI run `{context.ci_run_id}` and CodeQL remain valid. Publication may proceed.
"""
    )
    print(f"::notice::final release trust recheck passed for {context.tag} @ {context.sha}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("guard", "recheck"))
    command = parser.parse_args().command
    try:
        {"guard": command_guard, "recheck": command_recheck}[command]()
    except (TrustError, GitHubAPIError) as exc:
        die(str(exc))


if __name__ == "__main__":
    main()
