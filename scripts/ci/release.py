#!/usr/bin/env python3
"""Trusted stable-release request validation and post-approval recheck."""

from __future__ import annotations

import argparse
import re
from dataclasses import dataclass
from pathlib import Path

from github_api import (
    APICall,
    ControlPlaneError,
    api as default_api,
    append_output,
    append_summary,
    expect_array,
    expect_object,
    object_field,
    str_field,
)
from trust import (
    SEMVER_NUMBER,
    env,
    env_int,
    env_sha,
    exact_files,
    read_record,
    refuse,
    require_checkout,
    require_ci_gate,
    require_dispatch_run,
    require_exact_current_main,
    require_main_codeql,
)

STABLE_SEMVER_RE = re.compile(rf"v{SEMVER_NUMBER}\.{SEMVER_NUMBER}\.{SEMVER_NUMBER}")
REQUEST_KEYS = {
    "schemaVersion", "repository", "sourceSha", "version", "mode", "requestRunId",
    "requestRunAttempt",
}


@dataclass(frozen=True)
class ReleaseContext:
    repository: str
    sha: str
    tag: str
    ci_run_id: int
    publish: bool
    request_run_id: int


def require_stable_tag(tag: str) -> None:
    if STABLE_SEMVER_RE.fullmatch(tag) is None:
        refuse("stable release version must be vMAJOR.MINOR.PATCH")


def require_compatible_release_tag(
    repository: str, tag: str, expected_sha: str, *, api: APICall = default_api,
) -> None:
    """Refuse before publication if the exact tag already names another commit."""
    refs = expect_array(api(f"repos/{repository}/git/matching-refs/tags/{tag}"), tag)
    exact = [expect_object(ref, tag) for ref in refs
             if isinstance(ref, dict) and ref.get("ref") == f"refs/tags/{tag}"]
    if not exact:
        return
    if len(exact) != 1:
        refuse(f"multiple exact refs unexpectedly match {tag}")
    target = object_field(exact[0], "object", tag)
    if target.get("type") == "tag":
        annotated = api(f"repos/{repository}/git/tags/{str_field(target, 'sha', tag)}")
        target = object_field(expect_object(annotated, tag), "object", tag)
    if target.get("type") != "commit":
        refuse(f"{tag} does not reference a commit")
    if (sha := str_field(target, "sha", tag)) != expected_sha:
        refuse(f"{tag} already exists at {sha}, expected {expected_sha}")


def validate_request_context(*, api: APICall = default_api) -> ReleaseContext:
    repository = env("REPOSITORY")
    sha = env_sha("PUBLISHER_SHA")
    run_id = env_int("REQUEST_RUN_ID")
    if env("WORKFLOW_REF") != f"{repository}/.github/workflows/release.yml@refs/heads/main":
        refuse("stable release consumer is not the trusted main workflow")
    require_exact_current_main(repository, sha, api=api)
    require_checkout(sha)
    require_dispatch_run(
        repository, env("REPOSITORY_OWNER"), sha, run_id, "release-request.yml",
        f"stable-release-request-{run_id}", max_size=64 * 1024, api=api,
    )
    directory = Path(env("REQUEST_DIR"))
    exact_files(directory, {"request.json"})
    request = read_record(directory / "request.json", REQUEST_KEYS, {
        "schemaVersion": 1, "repository": repository, "sourceSha": sha,
        "requestRunId": run_id, "requestRunAttempt": 1,
    })
    tag = str_field(request, "version", "request")
    require_stable_tag(tag)
    if request["mode"] not in ("validate", "publish"):
        refuse("stable release mode must be validate or publish")
    require_compatible_release_tag(repository, tag, sha, api=api)
    ci_run_id = require_ci_gate(repository, sha, event="push", branch="main", api=api)
    require_main_codeql(repository, sha, api=api)
    return ReleaseContext(repository, sha, tag, ci_run_id, request["mode"] == "publish", run_id)


def command_guard() -> None:
    context = validate_request_context()
    version = context.tag[1:]
    append_output(
        tag=context.tag, version=version, series=version.rsplit(".", 1)[0],
        publish=str(context.publish).lower(),
    )
    append_summary(
        f"### Stable release request accepted\n\n"
        f"Request run `{context.request_run_id}` binds `{context.tag}` to current main "
        f"`{context.sha}` with CI run `{context.ci_run_id}` and current CodeQL. "
        + ("Publication still requires `ghcr-release` approval." if context.publish
           else "Validation mode cannot reach a write-permission job.")
    )


def command_recheck() -> None:
    repository = env("REPOSITORY")
    sha = env_sha("SOURCE_SHA")
    tag = env("REQUESTED_VERSION")
    require_stable_tag(tag)
    require_checkout(sha)
    require_exact_current_main(repository, sha)
    require_compatible_release_tag(repository, tag, sha)
    ci_run_id = require_ci_gate(repository, sha, event="push", branch="main")
    require_main_codeql(repository, sha)
    append_output(ci_run_id=ci_run_id)
    append_summary(
        f"### Final release trust recheck passed\n\n`{tag}` is still current main `{sha}` "
        f"after approval, with CI run `{ci_run_id}` and current CodeQL."
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("guard", "recheck"))
    try:
        {"guard": command_guard, "recheck": command_recheck}[parser.parse_args().command]()
    except (ControlPlaneError, OSError) as exc:
        raise SystemExit(f"Release refused: {exc}") from exc


if __name__ == "__main__":
    main()
