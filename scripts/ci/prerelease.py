#!/usr/bin/env python3
"""Typed control plane for off-main PR prerelease images.

Trust boundary:
- `request-*` runs from trusted current main and creates a one-use request.
- `candidate-*` runs with read-only permissions against PR payload, while this
  control-plane code and local CI actions are checked out from the trusted PR base.
- `publish-*` runs from trusted main after the candidate workflow and revalidates
  mutable trust state before and after environment approval.
- package publication itself is isolated in a no-checkout reusable workflow.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
from pathlib import Path
from typing import NoReturn

from github_api import (
    GitHubAPIError,
    JsonObject,
    JsonShapeError,
    api,
    append_output,
    append_summary,
    array_field,
    decode_json,
    expect_array,
    expect_object,
    int_field,
    object_field,
    optional_str_field,
    query,
    str_field,
)
from trust import (
    TrustError,
    require_check_run,
    require_ci_gate,
    require_current_main,
    require_exact_current_main,
    require_file_matches_main,
    require_pr,
)

SEMVER_NUMBER = r"(?:0|[1-9][0-9]*)"
PRERELEASE_RE = re.compile(
    rf"^v{SEMVER_NUMBER}\.{SEMVER_NUMBER}\.{SEMVER_NUMBER}"
    rf"-(alpha|beta|rc)\.{SEMVER_NUMBER}$"
)
LABEL_RE = re.compile(r"^gm-prerelease-([1-9][0-9]*)$")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")

# A prerelease may execute arbitrary application/test payload from the PR, but
# the orchestration that defines what a successful CI Gate means must remain
# identical to current main. Control-plane changes merge first, then payload PRs
# rebase before prerelease authorization.
PRERELEASE_CI_CONTROL_PLANE: tuple[str, ...] = (
    ".github/workflows/ci.yml",
    ".github/workflows/prerelease-candidate.yml",
    ".github/ci-paths.yml",
    ".github/actions/setup-project/action.yml",
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
REQUEST_ARTIFACT_LIMIT = 64 * 1024
CANDIDATE_ARTIFACT_LIMIT = 512 * 1024 * 1024
OCI_LIMIT = 1024 * 1024 * 1024

REQUEST_KEYS = {
    "schemaVersion",
    "repository",
    "pr",
    "headSha",
    "tag",
    "version",
    "requestRunId",
    "requestRunAttempt",
}
CANDIDATE_KEYS = {
    "schemaVersion",
    "repository",
    "pr",
    "headSha",
    "tag",
    "version",
    "requestRunId",
    "candidateRunId",
    "candidateRunAttempt",
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
    try:
        return int(value)
    except ValueError:
        die(f"{name} must be an integer")


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
    except JsonShapeError as exc:
        die(str(exc))
    return expect_object(value, str(path))


def compact_label_description(tag: str, sha: str) -> str:
    description = json.dumps({"t": tag, "s": sha}, separators=(",", ":"), sort_keys=True)
    if len(description) > 100:
        die("internal prerelease label metadata exceeds GitHub's 100-character limit")
    return description


def parse_label_description(description: str) -> tuple[str, str]:
    try:
        value = expect_object(decode_json(description, "request label description"), "request label description")
    except JsonShapeError as exc:
        die(str(exc))
    if set(value) != {"s", "t"}:
        die("request label description must contain exactly 's' and 't'")
    tag = str_field(value, "t", "request label description")
    sha = str_field(value, "s", "request label description")
    if PRERELEASE_RE.fullmatch(tag) is None:
        die("request label tag is invalid")
    if SHA_RE.fullmatch(sha) is None:
        die("request label SHA is invalid")
    return tag, sha


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def workflow_id(repository: str, path: str) -> int:
    value = expect_object(
        api(f"repos/{repository}/actions/workflows/{path}"),
        f"workflow {path}",
    )
    return int_field(value, "id", f"workflow {path}")


def optional_object_array(value: JsonObject, key: str, context: str) -> list[JsonObject]:
    raw = value.get(key)
    if raw is None:
        return []
    items = expect_array(raw, f"{context}.{key}")
    return [expect_object(item, f"{context}.{key}[{index}]") for index, item in enumerate(items)]


def run_artifacts(repository: str, run_id: int) -> list[JsonObject]:
    pages = expect_array(
        api(
            query(f"repos/{repository}/actions/runs/{run_id}/artifacts", per_page=100),
            paginate=True,
        ),
        "artifact pagination response",
    )
    result: list[JsonObject] = []
    for page_index, page_value in enumerate(pages):
        page = expect_object(page_value, f"artifact pagination page {page_index}")
        artifacts = array_field(page, "artifacts", f"artifact pagination page {page_index}")
        for artifact_index, artifact_value in enumerate(artifacts):
            result.append(
                expect_object(
                    artifact_value,
                    f"artifact pagination page {page_index}.artifacts[{artifact_index}]",
                )
            )
    return result


def require_exact_artifact(
    repository: str,
    run_id: int,
    name: str,
    *,
    max_size: int,
    required: bool,
) -> None:
    matches: list[JsonObject] = []
    for artifact in run_artifacts(repository, run_id):
        artifact_name = artifact.get("name")
        expired = artifact.get("expired")
        if artifact_name == name and expired is False:
            matches.append(artifact)

    if not matches and not required:
        return
    if len(matches) != 1:
        die(f"expected exactly one non-expired artifact named {name}, found {len(matches)}")
    size = int_field(matches[0], "size_in_bytes", f"artifact {name}")
    if size < 0:
        die(f"artifact {name} has an invalid size")
    if size > max_size:
        die(f"artifact {name} is too large ({size} bytes; limit {max_size})")


def pr_head_ref(pr: JsonObject, pr_number: int) -> str:
    head = object_field(pr, "head", f"PR #{pr_number}")
    return str_field(head, "ref", f"PR #{pr_number}.head")


def actor_login(run: JsonObject, key: str, context: str) -> str:
    actor = object_field(run, key, context)
    return str_field(actor, "login", f"{context}.{key}")


def require_prerelease_ci_control_plane(
    repository: str,
    pr_sha: str,
    main_sha: str,
) -> None:
    for path in PRERELEASE_CI_CONTROL_PLANE:
        require_file_matches_main(repository, path, pr_sha, main_sha)


def command_request_prepare() -> None:
    repository = env("REPOSITORY")
    owner = env("REPOSITORY_OWNER")
    actor = env("ACTOR")
    triggering_actor = env("TRIGGERING_ACTOR")
    event_name = env("EVENT_NAME")
    event_sha = env("EVENT_SHA")
    ref = env("REF")
    workflow_ref = env("WORKFLOW_REF")
    run_id = env_int("REQUEST_RUN_ID")
    run_attempt = env_int("REQUEST_RUN_ATTEMPT")
    pr_number = env_int("PR_NUMBER")
    requested_sha = env("REQUESTED_SHA")
    requested_tag = env("REQUESTED_TAG")
    out_dir = Path(env("OUT_DIR"))

    if event_name != "workflow_dispatch":
        die("only workflow_dispatch may request a prerelease")
    if ref != "refs/heads/main":
        die("the trusted prerelease request must run from main")
    if SHA_RE.fullmatch(event_sha) is None:
        die("request workflow event SHA is invalid")
    expected_ref = f"{repository}/.github/workflows/prerelease-request.yml@refs/heads/main"
    if workflow_ref != expected_ref:
        die("workflow is not the trusted prerelease request workflow on main")
    if actor != owner or triggering_actor != owner:
        die("only the repository owner may request an off-main prerelease")
    if run_attempt != 1:
        die("workflow reruns are not valid prerelease requests; start a fresh dispatch")
    if SHA_RE.fullmatch(requested_sha) is None:
        die("PR head SHA is invalid")
    if PRERELEASE_RE.fullmatch(requested_tag) is None:
        die("tag must be strict SemVer vMAJOR.MINOR.PATCH-{alpha,beta,rc}.N")

    require_exact_current_main(repository, event_sha)
    pr = require_pr(repository, pr_number, requested_sha)
    existing: list[str] = []
    for label in optional_object_array(pr, "labels", f"PR #{pr_number}"):
        name = label.get("name")
        if isinstance(name, str) and LABEL_RE.fullmatch(name) is not None:
            existing.append(name)
    if existing:
        die(
            "PR already has prerelease request label(s): "
            + ", ".join(sorted(existing))
            + "; remove the stale request before starting another"
        )

    current_main = require_current_main(repository, pr_number, requested_sha)
    require_prerelease_ci_control_plane(repository, requested_sha, current_main)
    ci_run_id = require_ci_gate(
        repository,
        requested_sha,
        event="pull_request",
        branch=pr_head_ref(pr, pr_number),
        pr_number=pr_number,
    )
    codeql_check_id = require_check_run(
        repository,
        requested_sha,
        name="CodeQL",
        app_slug="github-advanced-security",
    )

    version = requested_tag[1:]
    label = f"gm-prerelease-{run_id}"
    description = compact_label_description(requested_tag, requested_sha)
    artifact_name = f"prerelease-request-{run_id}"
    request: JsonObject = {
        "schemaVersion": 1,
        "repository": repository,
        "pr": pr_number,
        "headSha": requested_sha,
        "tag": requested_tag,
        "version": version,
        "requestRunId": run_id,
        "requestRunAttempt": run_attempt,
    }
    write_json(out_dir / "request.json", request)
    append_output(
        label=label,
        label_description=description,
        request_artifact=artifact_name,
        pr=pr_number,
        sha=requested_sha,
        tag=requested_tag,
        version=version,
    )
    append_summary(
        f"""### PR prerelease trust checks passed

| Property | Value |
|---|---|
| PR | `#{pr_number}` |
| Exact head | `{requested_sha}` |
| Current main | `{current_main}` |
| CI Gate run | `{ci_run_id}` |
| CodeQL check | `{codeql_check_id}` |
| Requested tag | `{requested_tag}` |

The request metadata stores only the immutable publication request. Mutable trust state is rechecked by the trusted publisher before and after environment approval.
"""
    )


def command_request_label() -> None:
    repository = env("REPOSITORY")
    pr_number = env_int("PR_NUMBER")
    label = env("LABEL_NAME")
    description = env("LABEL_DESCRIPTION")
    tag, sha = parse_label_description(description)
    if LABEL_RE.fullmatch(label) is None:
        die("temporary request label name is invalid")

    api(
        f"repos/{repository}/labels",
        method="POST",
        payload={"name": label, "color": "b60205", "description": description},
    )
    append_summary(
        f"""### One-use prerelease request ready

Apply this temporary label to PR #{pr_number}:

`{label}`

Requested image: `{tag[1:]}`

Requested PR SHA: `{sha}`

The low-trust candidate starts only when the repository owner applies this label. The trusted publisher removes it after the candidate transaction finishes.
"""
    )
    print(f"Temporary prerelease request label created: {label}")


def command_candidate_prepare() -> None:
    repository = env("REPOSITORY")
    owner = env("REPOSITORY_OWNER")
    actor = env("ACTOR")
    triggering_actor = env("TRIGGERING_ACTOR")
    run_attempt = env_int("CANDIDATE_RUN_ATTEMPT")
    label = env("LABEL_NAME")
    description = env("LABEL_DESCRIPTION")
    pr_number = env_int("PR_NUMBER")
    pr_base = env("PR_BASE")
    head_repository = env("HEAD_REPOSITORY")
    head_sha = env("PR_HEAD_SHA")

    if actor != owner or triggering_actor != owner:
        die("the prerelease request label must be applied by the repository owner")
    if run_attempt != 1:
        die("candidate workflow reruns are not valid; start a fresh prerelease request")
    if pr_base != "main" or head_repository != repository:
        die("candidate must be a same-repository PR targeting main")
    if SHA_RE.fullmatch(head_sha) is None:
        die("candidate PR head SHA is invalid")
    match = LABEL_RE.fullmatch(label)
    if match is None:
        die("label is not a prerelease request")
    tag, sha = parse_label_description(description)
    if sha != head_sha:
        die("request label SHA does not match the current PR head")

    append_output(
        request_run_id=match.group(1),
        pr=pr_number,
        sha=sha,
        tag=tag,
        version=tag[1:],
    )


def command_candidate_finalize() -> None:
    repository = env("REPOSITORY")
    pr_number = env_int("PR_NUMBER")
    head_sha = env("HEAD_SHA")
    tag = env("TAG")
    version = env("VERSION")
    request_run_id = env_int("REQUEST_RUN_ID")
    candidate_run_id = env_int("CANDIDATE_RUN_ID")
    candidate_run_attempt = env_int("CANDIDATE_RUN_ATTEMPT")
    out_dir = Path(env("OUT_DIR"))
    oci = Path(env("OCI_ARCHIVE"))

    if SHA_RE.fullmatch(head_sha) is None:
        die("candidate head SHA is invalid")
    if PRERELEASE_RE.fullmatch(tag) is None or version != tag[1:]:
        die("candidate tag/version is invalid")
    if candidate_run_attempt != 1:
        die("candidate run attempt must be 1")
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
        "candidateRunId": candidate_run_id,
        "candidateRunAttempt": candidate_run_attempt,
    }
    write_json(out_dir / "candidate.json", candidate)
    append_summary(
        f"""### Low-trust prerelease candidate built

| Property | Value |
|---|---|
| PR | `#{pr_number}` |
| Head SHA | `{head_sha}` |
| Requested tag | `{tag}` |
| Candidate OCI SHA-256 | `{digest}` |

This artifact is **untrusted data**. It has no publication authority and is independently validated by trusted-main code.
"""
    )


def command_publish_resolve() -> None:
    repository = env("REPOSITORY")
    owner = env("REPOSITORY_OWNER")
    candidate_run_id = env_int("CANDIDATE_RUN_ID")

    expected_candidate_workflow_id = workflow_id(repository, "prerelease-candidate.yml")
    run = expect_object(
        api(f"repos/{repository}/actions/runs/{candidate_run_id}"),
        "candidate workflow run",
    )
    if int_field(run, "workflow_id", "candidate workflow run") != expected_candidate_workflow_id:
        die("triggering run is not the prerelease candidate workflow")
    if str_field(run, "event", "candidate workflow run") != "pull_request":
        die("candidate workflow was not triggered by pull_request")
    head_repository = object_field(run, "head_repository", "candidate workflow run")
    if str_field(head_repository, "full_name", "candidate workflow run.head_repository") != repository:
        die("candidate workflow did not run from this repository")
    candidate_run_attempt = int_field(run, "run_attempt", "candidate workflow run")
    if candidate_run_attempt != 1:
        die("candidate workflow reruns are not accepted")
    if actor_login(run, "actor", "candidate workflow run") != owner or actor_login(
        run, "triggering_actor", "candidate workflow run"
    ) != owner:
        die("candidate workflow was not initiated by the repository owner")

    prs = optional_object_array(run, "pull_requests", "candidate workflow run")
    if len(prs) != 1:
        die("candidate workflow is not bound to exactly one pull request")
    pr_number = int_field(prs[0], "number", "candidate workflow run.pull_requests[0]")
    head_sha = str_field(run, "head_sha", "candidate workflow run")
    if SHA_RE.fullmatch(head_sha) is None:
        die("candidate workflow head SHA is invalid")

    pr = require_pr(repository, pr_number, head_sha)
    matching_labels: list[tuple[str, str]] = []
    for label in optional_object_array(pr, "labels", f"PR #{pr_number}"):
        name = label.get("name")
        if not isinstance(name, str) or LABEL_RE.fullmatch(name) is None:
            continue
        description = optional_str_field(label, "description", f"PR #{pr_number} label") or ""
        try:
            tag, sha = parse_label_description(description)
        except (SystemExit, JsonShapeError):
            continue
        if sha == head_sha:
            matching_labels.append((name, tag))
    if len(matching_labels) != 1:
        die(
            f"candidate PR has {len(matching_labels)} matching prerelease request labels; expected exactly one"
        )
    label, label_tag = matching_labels[0]
    label_match = LABEL_RE.fullmatch(label)
    if label_match is None:
        die("matching prerelease label unexpectedly became invalid")
    request_run_id = int(label_match.group(1))

    expected_request_workflow_id = workflow_id(repository, "prerelease-request.yml")
    request_run = expect_object(
        api(f"repos/{repository}/actions/runs/{request_run_id}"),
        "request workflow run",
    )
    if int_field(request_run, "workflow_id", "request workflow run") != expected_request_workflow_id:
        die("request label does not point to the prerelease request workflow")
    if str_field(request_run, "event", "request workflow run") != "workflow_dispatch":
        die("request workflow did not run as a trusted main dispatch")
    if str_field(request_run, "head_branch", "request workflow run") != "main":
        die("request workflow did not run as a trusted main dispatch")
    if int_field(request_run, "run_attempt", "request workflow run") != 1:
        die("request workflow reruns are not accepted")
    if str_field(request_run, "status", "request workflow run") != "completed" or str_field(
        request_run, "conclusion", "request workflow run"
    ) != "success":
        die("request workflow did not complete successfully")
    if actor_login(request_run, "actor", "request workflow run") != owner or actor_login(
        request_run, "triggering_actor", "request workflow run"
    ) != owner:
        die("request workflow was not initiated by the repository owner")

    request_artifact = f"prerelease-request-{request_run_id}"
    require_exact_artifact(
        repository,
        request_run_id,
        request_artifact,
        max_size=REQUEST_ARTIFACT_LIMIT,
        required=True,
    )

    job_pages = expect_array(
        api(
            query(f"repos/{repository}/actions/runs/{candidate_run_id}/jobs", filter="latest", per_page=100),
            paginate=True,
        ),
        "candidate jobs pagination response",
    )
    jobs: list[JsonObject] = []
    for page_index, page_value in enumerate(job_pages):
        page = expect_object(page_value, f"candidate jobs page {page_index}")
        for job_index, job_value in enumerate(
            array_field(page, "jobs", f"candidate jobs page {page_index}")
        ):
            jobs.append(expect_object(job_value, f"candidate jobs page {page_index}.jobs[{job_index}]"))
    candidate_jobs = [job for job in jobs if job.get("name") == "Build PR prerelease candidate"]
    candidate_success = (
        len(candidate_jobs) == 1
        and candidate_jobs[0].get("status") == "completed"
        and candidate_jobs[0].get("conclusion") == "success"
        and run.get("status") == "completed"
        and run.get("conclusion") == "success"
    )

    candidate_artifact = f"prerelease-candidate-{request_run_id}"
    require_exact_artifact(
        repository,
        candidate_run_id,
        candidate_artifact,
        max_size=CANDIDATE_ARTIFACT_LIMIT,
        required=candidate_success,
    )

    append_output(
        pr=pr_number,
        sha=head_sha,
        label=label,
        label_tag=label_tag,
        request_run_id=request_run_id,
        request_artifact=request_artifact,
        candidate_run_id=candidate_run_id,
        candidate_run_attempt=candidate_run_attempt,
        candidate_artifact=candidate_artifact,
        candidate_success=str(candidate_success).lower(),
    )


def exact_file_set(directory: Path, expected: set[str], label: str) -> None:
    if not directory.is_dir():
        die(f"{label} directory is missing")
    actual = {path.name for path in directory.iterdir()}
    if actual != expected:
        die(f"{label} files are {sorted(actual)}; expected {sorted(expected)}")
    for name in expected:
        path = directory / name
        if not path.is_file() or path.is_symlink():
            die(f"{label} entry {name} is not a regular file")


def require_exact_keys(value: JsonObject, expected: set[str], context: str) -> None:
    actual = set(value)
    if actual != expected:
        die(f"{context} keys are {sorted(actual)}; expected {sorted(expected)}")


def require_expected_fields(value: JsonObject, expected: JsonObject, context: str) -> None:
    for key, expected_value in expected.items():
        if value.get(key) != expected_value:
            die(f"{context} field {key} does not match the trusted workflow")


def command_publish_validate() -> None:
    repository = env("REPOSITORY")
    pr_number = env_int("PR_NUMBER")
    expected_sha = env("EXPECTED_SHA")
    label_tag = env("LABEL_TAG")
    request_run_id = env_int("REQUEST_RUN_ID")
    candidate_run_id = env_int("CANDIDATE_RUN_ID")
    candidate_run_attempt = env_int("CANDIDATE_RUN_ATTEMPT")
    request_dir = Path(env("REQUEST_DIR"))
    candidate_dir = Path(env("CANDIDATE_DIR"))
    handoff_dir = Path(env("HANDOFF_DIR"))

    if SHA_RE.fullmatch(expected_sha) is None:
        die("expected PR SHA is invalid")
    if PRERELEASE_RE.fullmatch(label_tag) is None:
        die("requested prerelease tag is invalid")
    if candidate_run_attempt != 1:
        die("candidate run attempt must be 1")

    exact_file_set(request_dir, {"request.json"}, "request artifact")
    exact_file_set(
        candidate_dir,
        {"candidate.json", "graphite-meter.oci.tar", "graphite-meter.oci.tar.sha256"},
        "candidate artifact",
    )

    request = read_json(request_dir / "request.json")
    candidate = read_json(candidate_dir / "candidate.json")
    require_exact_keys(request, REQUEST_KEYS, "request artifact")
    require_exact_keys(candidate, CANDIDATE_KEYS, "candidate artifact")

    request_expected: JsonObject = {
        "schemaVersion": 1,
        "repository": repository,
        "pr": pr_number,
        "headSha": expected_sha,
        "tag": label_tag,
        "version": label_tag[1:],
        "requestRunId": request_run_id,
        "requestRunAttempt": 1,
    }
    require_expected_fields(request, request_expected, "request artifact")

    candidate_expected: JsonObject = {
        "schemaVersion": 1,
        "repository": repository,
        "pr": pr_number,
        "headSha": expected_sha,
        "tag": label_tag,
        "version": label_tag[1:],
        "requestRunId": request_run_id,
        "candidateRunId": candidate_run_id,
        "candidateRunAttempt": candidate_run_attempt,
    }
    require_expected_fields(candidate, candidate_expected, "candidate artifact")

    oci = candidate_dir / "graphite-meter.oci.tar"
    if not oci.is_file() or oci.is_symlink():
        die("candidate OCI handoff is not a regular file")
    if oci.stat().st_size > OCI_LIMIT:
        die(f"candidate OCI archive exceeds {OCI_LIMIT} bytes")
    actual_digest = sha256_file(oci)
    try:
        checksum_text = (candidate_dir / "graphite-meter.oci.tar.sha256").read_text(encoding="utf-8")
    except OSError as exc:
        die(f"cannot read candidate OCI checksum: {exc}")
    expected_checksum = f"{actual_digest}  graphite-meter.oci.tar\n"
    if checksum_text != expected_checksum:
        die("candidate OCI checksum is invalid")

    # Validate mutable state before asking a human to approve publication.
    pr = require_pr(repository, pr_number, expected_sha)
    current_main = require_current_main(repository, pr_number, expected_sha)
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
    )

    handoff_dir.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(oci, handoff_dir / "graphite-meter.oci.tar")
    validation: JsonObject = {
        "schemaVersion": 1,
        "repository": repository,
        "pr": pr_number,
        "headSha": expected_sha,
        "tag": label_tag,
        "version": label_tag[1:],
        "requestRunId": request_run_id,
        "candidateRunId": candidate_run_id,
        "candidateRunAttempt": candidate_run_attempt,
        "currentMainSha": current_main,
        "ociSha256": actual_digest,
    }
    write_json(handoff_dir / "trusted-validation.json", validation)
    append_output(version=label_tag[1:], tag=label_tag, oci_sha256=actual_digest, main_sha=current_main)
    append_summary(
        f"""### Candidate validation passed

| Property | Value |
|---|---|
| PR | `#{pr_number}` |
| Exact head | `{expected_sha}` |
| Current main | `{current_main}` |
| CI Gate run | `{ci_run_id}` |
| CodeQL check | `{codeql_check_id}` |
| Requested tag | `{label_tag}` |
| OCI SHA-256 | `{actual_digest}` |

Exact artifact schema/file set, checksum, PR/current-main relationship, current-main CI control-plane identity, Gate, and CodeQL all passed. Publication still requires environment approval and a final post-approval recheck.
"""
    )
    print(f"::notice::validated prerelease {label_tag} for PR #{pr_number} @ {expected_sha}")


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
    )
    append_summary(
        f"""### Final prerelease publication recheck passed

PR `#{pr_number}` is still at `{expected_sha}` and the trusted main tip is still exactly `{current_main}` **after environment approval**. CI Gate run `{ci_run_id}` and CodeQL check `{codeql_check_id}` remain valid.
"""
    )
    print(f"::notice::final prerelease trust recheck passed for PR #{pr_number} @ {expected_sha}")


def command_cleanup() -> None:
    repository = env("REPOSITORY")
    pr_number = env_int("PR_NUMBER")
    label = env("LABEL_NAME")
    expected_sha = env("EXPECTED_SHA")
    if LABEL_RE.fullmatch(label) is None:
        print("::warning::cleanup skipped: invalid prerelease label name")
        return

    try:
        metadata_value = api(f"repos/{repository}/labels/{label}")
    except GitHubAPIError as exc:
        print(f"::warning::cleanup could not read {label}: {exc}")
        return
    try:
        metadata = expect_object(metadata_value, f"label {label}")
        description = optional_str_field(metadata, "description", f"label {label}") or ""
        _, sha = parse_label_description(description)
    except (JsonShapeError, SystemExit) as exc:
        print(f"::warning::cleanup left {label} untouched: {exc}")
        return
    if sha != expected_sha:
        print(f"::warning::cleanup left {label} untouched: SHA metadata changed")
        return

    for path in (
        f"repos/{repository}/issues/{pr_number}/labels/{label}",
        f"repos/{repository}/labels/{label}",
    ):
        try:
            api(path, method="DELETE")
        except GitHubAPIError as exc:
            print(f"::warning::cleanup failed for {path}: {exc}")
    print(f"cleanup finished for {label}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "command",
        choices=(
            "request-prepare",
            "request-label",
            "candidate-prepare",
            "candidate-finalize",
            "publish-resolve",
            "publish-validate",
            "publish-recheck",
            "cleanup",
        ),
    )
    command = parser.parse_args().command
    functions = {
        "request-prepare": command_request_prepare,
        "request-label": command_request_label,
        "candidate-prepare": command_candidate_prepare,
        "candidate-finalize": command_candidate_finalize,
        "publish-resolve": command_publish_resolve,
        "publish-validate": command_publish_validate,
        "publish-recheck": command_publish_recheck,
        "cleanup": command_cleanup,
    }
    try:
        functions[command]()
    except (TrustError, GitHubAPIError, JsonShapeError) as exc:
        die(str(exc))


if __name__ == "__main__":
    main()
