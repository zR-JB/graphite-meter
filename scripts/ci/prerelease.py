#!/usr/bin/env python3
"""Typed control plane for PR prerelease image publication.

Trust boundary:
- `Request PR prerelease` is a low-authority workflow_dispatch producer. It may
  execute the selected ref's request-side helpers, but has only contents:read,
  no secrets/write token, no shared dependency caches, and no publication path.
- `Publish verified PR prerelease` is a workflow_run consumer that executes from
  current default-branch tooling. It accepts only a successful request run from
  exact current main, validates the candidate artifact as untrusted data, and
  rechecks PR/CI/CodeQL state before and after environment approval.
- package publication itself is isolated in a no-checkout reusable workflow.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
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
    expect_object,
    int_field,
    str_field,
)
from trust import (
    TrustError,
    actor_login,
    require_check_run,
    require_ci_gate,
    require_current_main,
    require_exact_artifact,
    require_exact_current_main,
    require_file_matches_main,
    require_pr,
    workflow_id,
)

SEMVER_NUMBER = r"(?:0|[1-9][0-9]*)"
PRERELEASE_RE = re.compile(
    rf"^v{SEMVER_NUMBER}\.{SEMVER_NUMBER}\.{SEMVER_NUMBER}"
    rf"-(alpha|beta|rc)\.{SEMVER_NUMBER}$"
)
SHA_RE = re.compile(r"^[0-9a-f]{40}$")

# A prerelease may change arbitrary application source, but the orchestration
# that defines Gate/publication authority must match exact current main. Pipeline
# changes merge first; payload PRs then rebase before prerelease authorization.
PRERELEASE_CI_CONTROL_PLANE: tuple[str, ...] = (
    ".github/workflows/ci.yml",
    ".github/workflows/prerelease-request.yml",
    ".github/workflows/prerelease-publish.yml",
    ".github/workflows/release-request.yml",
    ".github/workflows/release.yml",
    ".github/workflows/_publish-oci.yml",
    ".github/workflows/_publish-release.yml",
    ".github/workflows/_promote-oci.yml",
    ".github/ci-paths.yml",
    ".github/actions/setup-project/action.yml",
    ".github/actions/build-oci/action.yml",
    "justfile",
    "scripts/ci/github_api.py",
    "scripts/ci/prerelease.py",
    "scripts/ci/release.py",
    "scripts/ci/test_pipeline.py",
    "scripts/ci/trust.py",
    "scripts/ci/verify_oci.py",
    "scripts/ci/verify_release_assets.py",
    "scripts/ci/workflow_policy.py",
)
OCI_LIMIT = 1024 * 1024 * 1024
CANDIDATE_ARTIFACT_LIMIT = OCI_LIMIT + 1024 * 1024
CANDIDATE_KEYS = {
    "schemaVersion",
    "repository",
    "pr",
    "headSha",
    "tag",
    "version",
    "requestRunId",
    "requestRunAttempt",
}


def die(message: str) -> NoReturn:
    raise SystemExit(f"PR prerelease refused: {message}")


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


def write_json(path: Path, value: JsonObject) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def read_json(path: Path) -> JsonObject:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        die(f"cannot read {path}: {exc}")
    try:
        value = decode_json(text, str(path))
        return expect_object(value, str(path))
    except JsonShapeError as exc:
        die(str(exc))


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def pr_head_ref(pr: JsonObject, pr_number: int) -> str:
    try:
        head = expect_object(pr.get("head"), f"PR #{pr_number}.head")
        return str_field(head, "ref", f"PR #{pr_number}.head")
    except JsonShapeError as exc:
        die(str(exc))


def require_prerelease_ci_control_plane(
    repository: str,
    pr_sha: str,
    main_sha: str,
    *,
    api: APICall = default_api,
) -> None:
    for path in PRERELEASE_CI_CONTROL_PLANE:
        require_file_matches_main(repository, path, pr_sha, main_sha, api=api)


def exact_file_set(directory: Path, expected: set[str], label: str) -> None:
    if not directory.is_dir():
        die(f"{label} directory is missing")
    entries = list(directory.iterdir())
    actual = {path.name for path in entries}
    if actual != expected or len(entries) != len(expected):
        die(f"{label} files are {sorted(path.name for path in entries)}; expected {sorted(expected)}")
    for name in expected:
        path = directory / name
        if not path.is_file() or path.is_symlink():
            die(f"{label} entry {name} is not a regular file")


def require_exact_keys(value: JsonObject, expected: set[str], context: str) -> None:
    actual = set(value)
    if actual != expected:
        die(f"{context} keys are {sorted(actual)}; expected {sorted(expected)}")


def command_request_prepare() -> None:
    """Fail early for a normal main dispatch; trusted publication revalidates all fields."""
    repository = env("REPOSITORY")
    owner = env("REPOSITORY_OWNER")
    actor = env("ACTOR")
    triggering_actor = env("TRIGGERING_ACTOR")
    event_name = env("EVENT_NAME")
    event_sha = env("EVENT_SHA")
    ref = env("REF")
    workflow_ref = env("WORKFLOW_REF")
    run_attempt = env_int("REQUEST_RUN_ATTEMPT")
    pr_number = env_int("PR_NUMBER")
    requested_sha = env("REQUESTED_SHA")
    requested_tag = env("REQUESTED_TAG")

    if event_name != "workflow_dispatch":
        die("only workflow_dispatch may request a prerelease")
    if ref != "refs/heads/main":
        die("prerelease requests must be dispatched from main")
    if SHA_RE.fullmatch(event_sha) is None:
        die("request workflow event SHA is invalid")
    expected_ref = f"{repository}/.github/workflows/prerelease-request.yml@refs/heads/main"
    if workflow_ref != expected_ref:
        die("workflow is not the prerelease request workflow on main")
    if actor != owner or triggering_actor != owner:
        die("only the repository owner may request an off-main prerelease")
    if run_attempt != 1:
        die("workflow reruns are not valid prerelease requests; start a fresh dispatch")
    if SHA_RE.fullmatch(requested_sha) is None:
        die("PR head SHA is invalid")
    if PRERELEASE_RE.fullmatch(requested_tag) is None:
        die("tag must be strict SemVer vMAJOR.MINOR.PATCH-{alpha,beta,rc}.N")

    append_output(
        pr=pr_number,
        sha=requested_sha,
        tag=requested_tag,
        version=requested_tag[1:],
    )


def command_request_finalize() -> None:
    repository = env("REPOSITORY")
    pr_number = env_int("PR_NUMBER")
    head_sha = env("HEAD_SHA")
    tag = env("TAG")
    version = env("VERSION")
    request_run_id = env_int("REQUEST_RUN_ID")
    request_run_attempt = env_int("REQUEST_RUN_ATTEMPT")
    out_dir = Path(env("OUT_DIR"))
    oci = Path(env("OCI_ARCHIVE"))

    if SHA_RE.fullmatch(head_sha) is None:
        die("candidate head SHA is invalid")
    if PRERELEASE_RE.fullmatch(tag) is None or version != tag[1:]:
        die("candidate tag/version is invalid")
    if request_run_attempt != 1:
        die("request run attempt must be 1")
    if not oci.is_file() or oci.is_symlink():
        die("candidate OCI archive is missing or not a regular file")
    if oci.stat().st_size > OCI_LIMIT:
        die(f"candidate OCI archive exceeds {OCI_LIMIT} bytes")

    out_dir.mkdir(parents=True, exist_ok=True)
    destination = out_dir / "graphite-meter.oci.tar"
    if oci.resolve() != destination.resolve():
        shutil.copyfile(oci, destination)
    digest = sha256_file(destination)
    (out_dir / "graphite-meter.oci.tar.sha256").write_text(
        f"{digest}  graphite-meter.oci.tar\n", encoding="utf-8"
    )
    candidate: JsonObject = {
        "schemaVersion": 1,
        "repository": repository,
        "pr": pr_number,
        "headSha": head_sha,
        "tag": tag,
        "version": version,
        "requestRunId": request_run_id,
        "requestRunAttempt": request_run_attempt,
    }
    write_json(out_dir / "candidate.json", candidate)
    append_summary(
        f"""### Low-authority prerelease candidate built

| Property | Value |
|---|---|
| PR | `#{pr_number}` |
| Requested head | `{head_sha}` |
| Requested tag | `{tag}` |
| Candidate OCI SHA-256 | `{digest}` |

This run has no publication authority. The trusted default-branch consumer independently binds the workflow run to current `main`, validates the PR/Gate/CodeQL state, and verifies this OCI archive as untrusted data.
"""
    )


def validate_request_run(
    repository: str,
    owner: str,
    publisher_sha: str,
    request_run_id: int,
    *,
    api: APICall = default_api,
) -> str:
    expected_workflow_id = workflow_id(repository, "prerelease-request.yml", api=api)
    try:
        run = expect_object(
            api(f"repos/{repository}/actions/runs/{request_run_id}"),
            "prerelease request workflow run",
        )
        if int_field(run, "id", "prerelease request workflow run") != request_run_id:
            die("prerelease request run ID changed")
        if int_field(run, "workflow_id", "prerelease request workflow run") != expected_workflow_id:
            die("triggering run is not prerelease-request.yml")
        event = str_field(run, "event", "prerelease request workflow run")
        head_branch = str_field(run, "head_branch", "prerelease request workflow run")
        head_sha = str_field(run, "head_sha", "prerelease request workflow run")
        status = str_field(run, "status", "prerelease request workflow run")
        conclusion = str_field(run, "conclusion", "prerelease request workflow run")
        run_attempt = int_field(run, "run_attempt", "prerelease request workflow run")
    except JsonShapeError as exc:
        die(str(exc))

    if event != "workflow_dispatch" or head_branch != "main":
        die("prerelease request/candidate was not dispatched from main")
    if head_sha != publisher_sha:
        die(
            "main changed between the prerelease request and trusted consumer; "
            "update the PR and start a fresh prerelease request"
        )
    if run_attempt != 1:
        die("prerelease request reruns are not accepted; start a fresh dispatch")
    if status != "completed" or conclusion != "success":
        die(f"prerelease request/candidate run is {status}/{conclusion}")
    if actor_login(run, "actor", "prerelease request workflow run") != owner or actor_login(
        run, "triggering_actor", "prerelease request workflow run"
    ) != owner:
        die("prerelease request was not initiated by the repository owner")

    artifact_name = f"prerelease-candidate-{request_run_id}"
    require_exact_artifact(
        repository,
        request_run_id,
        artifact_name,
        max_size=CANDIDATE_ARTIFACT_LIMIT,
        required=True,
        api=api,
    )
    return artifact_name


def command_publish_resolve() -> None:
    repository = env("REPOSITORY")
    owner = env("REPOSITORY_OWNER")
    publisher_sha = env("PUBLISHER_SHA")
    workflow_ref = env("WORKFLOW_REF")
    request_run_id = env_int("REQUEST_RUN_ID")

    if SHA_RE.fullmatch(publisher_sha) is None:
        die("trusted publisher SHA is invalid")
    expected_ref = f"{repository}/.github/workflows/prerelease-publish.yml@refs/heads/main"
    if workflow_ref != expected_ref:
        die("prerelease publisher is not the trusted main workflow")
    require_exact_current_main(repository, publisher_sha)
    if git("rev-parse", "HEAD") != publisher_sha:
        die("checked-out publisher tooling does not match trusted current main")

    artifact_name = validate_request_run(repository, owner, publisher_sha, request_run_id)
    append_output(request_run_id=request_run_id, candidate_artifact=artifact_name)


def command_publish_validate() -> None:
    repository = env("REPOSITORY")
    publisher_sha = env("PUBLISHER_SHA")
    request_run_id = env_int("REQUEST_RUN_ID")
    candidate_dir = Path(env("CANDIDATE_DIR"))
    handoff_dir = Path(env("HANDOFF_DIR"))

    if SHA_RE.fullmatch(publisher_sha) is None:
        die("trusted publisher SHA is invalid")
    exact_file_set(
        candidate_dir,
        {"candidate.json", "graphite-meter.oci.tar", "graphite-meter.oci.tar.sha256"},
        "candidate artifact",
    )
    candidate = read_json(candidate_dir / "candidate.json")
    require_exact_keys(candidate, CANDIDATE_KEYS, "candidate artifact")
    try:
        schema_version = int_field(candidate, "schemaVersion", "candidate artifact")
        candidate_repository = str_field(candidate, "repository", "candidate artifact")
        pr_number = int_field(candidate, "pr", "candidate artifact")
        expected_sha = str_field(candidate, "headSha", "candidate artifact")
        tag = str_field(candidate, "tag", "candidate artifact")
        version = str_field(candidate, "version", "candidate artifact")
        artifact_run_id = int_field(candidate, "requestRunId", "candidate artifact")
        artifact_attempt = int_field(candidate, "requestRunAttempt", "candidate artifact")
    except JsonShapeError as exc:
        die(str(exc))

    if schema_version != 1:
        die("unsupported candidate artifact schema")
    if candidate_repository != repository:
        die("candidate artifact repository mismatch")
    if pr_number <= 0:
        die("candidate artifact PR number is invalid")
    if SHA_RE.fullmatch(expected_sha) is None:
        die("candidate artifact head SHA is invalid")
    if PRERELEASE_RE.fullmatch(tag) is None or version != tag[1:]:
        die("candidate artifact tag/version is invalid")
    if artifact_run_id != request_run_id or artifact_attempt != 1:
        die("candidate artifact request-run metadata mismatch")

    oci = candidate_dir / "graphite-meter.oci.tar"
    if oci.stat().st_size > OCI_LIMIT:
        die(f"candidate OCI archive exceeds {OCI_LIMIT} bytes")
    actual_digest = sha256_file(oci)
    try:
        checksum_text = (candidate_dir / "graphite-meter.oci.tar.sha256").read_text(encoding="utf-8")
    except OSError as exc:
        die(f"cannot read candidate OCI checksum: {exc}")
    if checksum_text != f"{actual_digest}  graphite-meter.oci.tar\n":
        die("candidate OCI checksum is invalid")

    # The privileged consumer never checks out PR source. It only reads GitHub
    # provenance and validates the untrusted OCI artifact on a fresh runner.
    require_exact_current_main(repository, publisher_sha)
    if git("rev-parse", "HEAD") != publisher_sha:
        die("checked-out validation tooling does not match trusted current main")
    pr = require_pr(repository, pr_number, expected_sha)
    current_main = require_current_main(repository, pr_number, expected_sha)
    if current_main != publisher_sha:
        die("current main differs from trusted publisher tooling; start a fresh prerelease request")
    require_prerelease_ci_control_plane(repository, expected_sha, current_main)
    ci_run_id = require_ci_gate(
        repository,
        expected_sha,
        event="pull_request",
        branch=pr_head_ref(pr, pr_number),
        pr_number=pr_number,
    )
    codeql_check_id = require_check_run(
        repository,
        expected_sha,
        name="CodeQL",
        app_slug="github-advanced-security",
        pr_number=pr_number,
    )

    handoff_dir.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(oci, handoff_dir / "graphite-meter.oci.tar")
    append_output(
        pr=pr_number,
        sha=expected_sha,
        version=version,
        tag=tag,
        oci_sha256=actual_digest,
        main_sha=current_main,
    )
    append_summary(
        f"""### Prerelease candidate validation passed

| Property | Value |
|---|---|
| Request run | `{request_run_id}` |
| PR | `#{pr_number}` |
| Exact head | `{expected_sha}` |
| Current main | `{current_main}` |
| CI Gate run | `{ci_run_id}` |
| CodeQL check | `{codeql_check_id}` |
| Requested tag | `{tag}` |
| OCI SHA-256 | `{actual_digest}` |

The request run was previously bound to exact current-main workflow provenance. Exact artifact files/checksum, PR/current-main relationship, CI control-plane identity, Gate, and PR-bound CodeQL all pass. Publication still requires environment approval and a final post-approval recheck.
"""
    )
    print(f"::notice::validated prerelease {tag} for PR #{pr_number} @ {expected_sha}")


def command_publish_recheck() -> None:
    repository = env("REPOSITORY")
    pr_number = env_int("PR_NUMBER")
    expected_sha = env("EXPECTED_SHA")
    expected_main_sha = env("EXPECTED_MAIN_SHA")
    if SHA_RE.fullmatch(expected_sha) is None:
        die("expected PR SHA is invalid")
    if SHA_RE.fullmatch(expected_main_sha) is None:
        die("expected main SHA is invalid")

    require_exact_current_main(repository, expected_main_sha)
    pr = require_pr(repository, pr_number, expected_sha)
    current_main = require_current_main(repository, pr_number, expected_sha)
    if current_main != expected_main_sha:
        die("current main changed after candidate validation; start a fresh prerelease request")
    ci_run_id = require_ci_gate(
        repository,
        expected_sha,
        event="pull_request",
        branch=pr_head_ref(pr, pr_number),
        pr_number=pr_number,
    )
    codeql_check_id = require_check_run(
        repository,
        expected_sha,
        name="CodeQL",
        app_slug="github-advanced-security",
        pr_number=pr_number,
    )
    append_output(ci_run_id=ci_run_id, codeql_check_id=codeql_check_id)
    append_summary(
        f"""### Final prerelease publication recheck passed

PR `#{pr_number}` is still at `{expected_sha}` and trusted main is still exactly `{current_main}` **after environment approval**. CI Gate run `{ci_run_id}` and PR-bound CodeQL check `{codeql_check_id}` remain valid.
"""
    )
    print(f"::notice::final prerelease trust recheck passed for PR #{pr_number} @ {expected_sha}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "command",
        choices=(
            "request-prepare",
            "request-finalize",
            "publish-resolve",
            "publish-validate",
            "publish-recheck",
        ),
    )
    command = parser.parse_args().command
    commands = {
        "request-prepare": command_request_prepare,
        "request-finalize": command_request_finalize,
        "publish-resolve": command_publish_resolve,
        "publish-validate": command_publish_validate,
        "publish-recheck": command_publish_recheck,
    }
    try:
        commands[command]()
    except (TrustError, GitHubAPIError, JsonShapeError) as exc:
        die(str(exc))


if __name__ == "__main__":
    main()
