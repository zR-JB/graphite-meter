#!/usr/bin/env python3
"""Low-authority request helpers and trusted publication checks for PR prereleases."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
from pathlib import Path

from github_api import ControlPlaneError, append_output, append_summary, int_field, str_field
from trust import (
    SEMVER_NUMBER,
    SHA_RE,
    env,
    env_int,
    env_sha,
    exact_files,
    read_record,
    refuse,
    require_check_run,
    require_checkout,
    require_ci_gate,
    require_control_plane_matches_main,
    require_current_main,
    require_dispatch_run,
    require_exact_current_main,
    require_pr,
)

N = SEMVER_NUMBER
PRERELEASE_RE = re.compile(rf"v{N}\.{N}\.{N}-(?:alpha|beta|rc)\.{N}")
OCI = "graphite-meter.oci.tar"
OCI_LIMIT = 1024 * 1024 * 1024
CANDIDATE_KEYS = {
    "schemaVersion", "repository", "pr", "headSha", "tag", "version", "requestRunId",
    "requestRunAttempt",
}


def sha256_file(path: Path) -> str:
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def require_tag(tag: str, version: str) -> None:
    if PRERELEASE_RE.fullmatch(tag) is None or version != tag[1:]:
        refuse("tag must be vMAJOR.MINOR.PATCH-{alpha,beta,rc}.N")


def require_fresh_pr(repository: str, pr: int, sha: str) -> tuple[str, int, int]:
    branch = require_pr(repository, pr, sha)
    main = require_current_main(repository, pr, sha)
    ci_run_id = require_ci_gate(
        repository, sha, event="pull_request", branch=branch, pr_number=pr,
    )
    codeql_id = require_check_run(
        repository, sha, name="CodeQL", app_slug="github-advanced-security", pr_number=pr,
    )
    return main, ci_run_id, codeql_id


def command_request_prepare() -> None:
    repository, owner = env("REPOSITORY"), env("REPOSITORY_OWNER")
    env_sha("EVENT_SHA")
    if env("EVENT_NAME") != "workflow_dispatch" or env("REF") != "refs/heads/main":
        refuse("prerelease requests must be dispatched from main")
    workflow = f"{repository}/.github/workflows/prerelease-request.yml@refs/heads/main"
    if env("WORKFLOW_REF") != workflow:
        refuse("workflow is not the prerelease request workflow on main")
    if env("ACTOR") != owner or env("TRIGGERING_ACTOR") != owner:
        refuse("only the repository owner may request a prerelease")
    if env_int("REQUEST_RUN_ATTEMPT") != 1:
        refuse("workflow reruns are not valid prerelease requests; start a fresh dispatch")
    sha, tag = env_sha("REQUESTED_SHA"), env("REQUESTED_TAG")
    require_tag(tag, tag[1:])
    append_output(pr=env_int("PR_NUMBER"), sha=sha, tag=tag, version=tag[1:])


def command_request_finalize() -> None:
    sha, tag, version = env_sha("HEAD_SHA"), env("TAG"), env("VERSION")
    require_tag(tag, version)
    if env_int("REQUEST_RUN_ATTEMPT") != 1:
        refuse("request run attempt must be 1")
    archive, out = Path(env("OCI_ARCHIVE")), Path(env("OUT_DIR"))
    if archive.is_symlink() or not archive.is_file() or archive.stat().st_size > OCI_LIMIT:
        refuse(f"candidate OCI archive must be a regular file of at most {OCI_LIMIT} bytes")
    out.mkdir(parents=True, exist_ok=True)
    if archive.resolve() != (out / OCI).resolve():
        shutil.copyfile(archive, out / OCI)
    digest = sha256_file(out / OCI)
    (out / f"{OCI}.sha256").write_text(f"{digest}  {OCI}\n", encoding="utf-8")
    candidate = {
        "schemaVersion": 1, "repository": env("REPOSITORY"), "pr": env_int("PR_NUMBER"),
        "headSha": sha, "tag": tag, "version": version,
        "requestRunId": env_int("REQUEST_RUN_ID"), "requestRunAttempt": 1,
    }
    (out / "candidate.json").write_text(json.dumps(candidate, indent=2, sort_keys=True) + "\n")
    append_summary(
        f"### Low-authority prerelease candidate built\n\nPR `#{candidate['pr']}` at `{sha}` "
        f"as `{tag}`, OCI SHA-256 `{digest}`. This run has no publication authority."
    )


def command_publish_resolve() -> None:
    repository = env("REPOSITORY")
    sha = env_sha("PUBLISHER_SHA")
    run_id = env_int("REQUEST_RUN_ID")
    workflow = f"{repository}/.github/workflows/prerelease-publish.yml@refs/heads/main"
    if env("WORKFLOW_REF") != workflow:
        refuse("prerelease publisher is not the trusted main workflow")
    require_exact_current_main(repository, sha)
    require_checkout(sha)
    artifact = f"prerelease-candidate-{run_id}"
    require_dispatch_run(
        repository, env("REPOSITORY_OWNER"), sha, run_id, "prerelease-request.yml", artifact,
        max_size=OCI_LIMIT + 1024 * 1024,
    )
    append_output(request_run_id=run_id, candidate_artifact=artifact)


def command_publish_validate() -> None:
    repository = env("REPOSITORY")
    publisher = env_sha("PUBLISHER_SHA")
    run_id = env_int("REQUEST_RUN_ID")
    candidate_dir, handoff = Path(env("CANDIDATE_DIR")), Path(env("HANDOFF_DIR"))
    exact_files(candidate_dir, {"candidate.json", OCI, f"{OCI}.sha256"})
    candidate = read_record(candidate_dir / "candidate.json", CANDIDATE_KEYS, {
        "schemaVersion": 1, "repository": repository, "requestRunId": run_id,
        "requestRunAttempt": 1,
    })
    pr = int_field(candidate, "pr", "candidate")
    sha = str_field(candidate, "headSha", "candidate")
    tag = str_field(candidate, "tag", "candidate")
    require_tag(tag, str_field(candidate, "version", "candidate"))
    if pr <= 0 or SHA_RE.fullmatch(sha) is None:
        refuse("candidate PR number or head SHA is invalid")
    oci = candidate_dir / OCI
    if oci.stat().st_size > OCI_LIMIT:
        refuse(f"candidate OCI archive exceeds {OCI_LIMIT} bytes")
    digest = sha256_file(oci)
    if (candidate_dir / f"{OCI}.sha256").read_text(encoding="utf-8") != f"{digest}  {OCI}\n":
        refuse("candidate OCI checksum is invalid")

    require_exact_current_main(repository, publisher)
    require_checkout(publisher)
    main, ci_run_id, codeql_id = require_fresh_pr(repository, pr, sha)
    if main != publisher:
        refuse("current main differs from the trusted publisher; start a fresh request")
    require_control_plane_matches_main(repository, sha, main)

    handoff.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(oci, handoff / OCI)
    append_output(pr=pr, sha=sha, version=tag[1:], tag=tag, oci_sha256=digest, main_sha=main)
    append_summary(
        f"### Prerelease candidate validation passed\n\nRequest run `{run_id}`, PR `#{pr}` at "
        f"`{sha}` on main `{main}`, CI run `{ci_run_id}`, CodeQL check `{codeql_id}`, "
        f"tag `{tag}`, OCI SHA-256 `{digest}`. Publication still requires approval."
    )


def command_publish_recheck() -> None:
    repository = env("REPOSITORY")
    pr = env_int("PR_NUMBER")
    sha, expected_main = env_sha("EXPECTED_SHA"), env_sha("EXPECTED_MAIN_SHA")
    require_exact_current_main(repository, expected_main)
    main, ci_run_id, codeql_id = require_fresh_pr(repository, pr, sha)
    if main != expected_main:
        refuse("current main changed after candidate validation; start a fresh request")
    append_output(ci_run_id=ci_run_id, codeql_check_id=codeql_id)
    append_summary(
        f"### Final prerelease recheck passed\n\nPR `#{pr}` is still at `{sha}` on main "
        f"`{main}` after approval, with CI run `{ci_run_id}` and CodeQL check `{codeql_id}`."
    )


COMMANDS = {
    "request-prepare": command_request_prepare,
    "request-finalize": command_request_finalize,
    "publish-resolve": command_publish_resolve,
    "publish-validate": command_publish_validate,
    "publish-recheck": command_publish_recheck,
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=COMMANDS)
    try:
        COMMANDS[parser.parse_args().command]()
    except (ControlPlaneError, OSError) as exc:
        raise SystemExit(f"PR prerelease refused: {exc}") from exc


if __name__ == "__main__":
    main()
