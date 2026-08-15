#!/usr/bin/env python3
"""Dependency-free repository policy checks for the GitHub Actions control plane.

GitHub repository settings are the authority for *which* external action
repositories may execute. This local checker deliberately does not mirror that
allowlist or maintain a second SHA database; it only enforces invariants that
are useful before GitHub schedules a workflow, including immutable 40-SHA refs.
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys
from typing import NoReturn

ROOT = pathlib.Path(__file__).resolve().parents[2]
USES = re.compile(r"^\s*(?:-\s*)?uses:\s*(\S+)\s*(?:#.*)?$")
LOCAL_ACTION = re.compile(r"^\./[A-Za-z0-9_./-]+$")
PINNED_ACTION = re.compile(
    r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*@[0-9a-f]{40}$"
)
CERT_NAME = re.compile(
    r"(^|/)(\.dev-certs|certs?|certificates?|letsencrypt)(/|$)|"
    r"\.(pem|key|crt|cer|der|csr|p12|pfx|pkcs8|jks|keystore)$",
    re.IGNORECASE,
)
PEM = re.compile(rb"-----BEGIN (?:CERTIFICATE|(?:[^ -]+ )*PRIVATE KEY)-----")


class PolicyError(RuntimeError):
    pass


def fail(message: str) -> NoReturn:
    raise PolicyError(message)


def workflow_files(root: pathlib.Path = ROOT) -> list[pathlib.Path]:
    files = list((root / ".github" / "workflows").glob("*.yml"))
    files += list((root / ".github" / "workflows").glob("*.yaml"))
    files += list((root / ".github" / "actions").glob("**/action.yml"))
    files += list((root / ".github" / "actions").glob("**/action.yaml"))
    return sorted({path for path in files if path.is_file()})


def check_external_action_shas(root: pathlib.Path = ROOT) -> None:
    violations: list[str] = []
    for path in workflow_files(root):
        for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            match = USES.match(line)
            if match is None:
                continue
            ref = match.group(1)
            if LOCAL_ACTION.fullmatch(ref) is not None:
                continue
            if PINNED_ACTION.fullmatch(ref) is None:
                violations.append(
                    f"{path.relative_to(root)}:{lineno}: external action must use a full 40-character commit SHA: {ref}"
                )
    if violations:
        fail("external action SHA policy failed:\n  " + "\n  ".join(violations))


def check_privileged_workflows(root: pathlib.Path = ROOT) -> None:
    workflows = root / ".github" / "workflows"

    for name in ("_publish-oci.yml", "_promote-oci.yml"):
        path = workflows / name
        if not path.is_file():
            fail(f"missing privileged reusable workflow {name}")
        text = path.read_text(encoding="utf-8")
        for label, needle in {
            "repository checkout": "actions/checkout@",
            "local action execution": "uses: ./",
            "Just execution": "just ",
            "repository script execution": "scripts/",
            "environment wait after final recheck": "environment:",
        }.items():
            if needle in text:
                fail(f"{name} contains forbidden {label}: {needle}")
        if "packages: write" not in text:
            fail(f"{name} must declare packages: write")
        if '-e IMAGE="$image"' not in text:
            fail(f"{name} must pass IMAGE explicitly to the Skopeo container")
        if re.search(r"@sha256:[0-9a-f]{64}", text) is None:
            fail(f"{name} must pin the Skopeo container by digest")

    oci = (workflows / "_publish-oci.yml").read_text(encoding="utf-8")
    if "group: publish-oci-${{ github.repository }}-${{ inputs.tag }}" not in oci:
        fail("_publish-oci.yml must serialize publication by exact destination tag")
    for required in (
        "source_sha:",
        "trusted_main_sha:",
        "pr_number:",
        "contents: read",
        "pull-requests: read",
        'gh api "repos/$REPOSITORY/commits/main"',
        'gh api "repos/$REPOSITORY/pulls/$PR_NUMBER"',
        'gh api "repos/$REPOSITORY/compare/$TRUSTED_MAIN_SHA...$SOURCE_SHA"',
        "live source freshness check passed immediately before registry authentication",
    ):
        if required not in oci:
            fail(f"_publish-oci.yml missing last-mile source freshness invariant: {required}")

    release_pub = workflows / "_publish-release.yml"
    text = release_pub.read_text(encoding="utf-8")
    for label, needle in {
        "repository checkout": "actions/checkout@",
        "local action execution": "uses: ./",
        "Just execution": "just ",
        "repository script execution": "scripts/",
        "package publication permission": "packages: write",
        "third-party release action": "softprops/",
        "environment wait after final recheck": "environment:",
    }.items():
        if needle in text:
            fail(f"_publish-release.yml contains forbidden {label}: {needle}")
    for required in (
        "contents: write",
        '"draft":false',
        '"make_latest":"legacy"',
        ".digest",
        "target_sha",
        "releases?per_page=100",
        ".upload_url",
        "https://uploads.github.com/",
        "auth_header",
    ):
        if required not in text:
            fail(f"_publish-release.yml missing invariant: {required}")
    for forbidden in (
        "releases/tags/$TAG",
        "gh release upload",
    ):
        if forbidden in text:
            fail(
                f"_publish-release.yml uses non-deterministic draft lookup/upload path: {forbidden}"
            )

    promote = (workflows / "_promote-oci.yml").read_text(encoding="utf-8")
    if "sort -V" not in promote or "highest_global" not in promote or "highest_series" not in promote:
        fail("_promote-oci.yml must prevent stable alias rollback using published SemVer ordering")
    if "group: promote-stable-oci-${{ github.repository }}" not in promote:
        fail("_promote-oci.yml must serialize stable alias movement")


def check_runner_labels(root: pathlib.Path = ROOT) -> None:
    violations: list[str] = []
    for path in sorted((root / ".github" / "workflows").glob("*.yml")):
        for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if "runs-on: ubuntu-latest" in line:
                violations.append(f"{path.relative_to(root)}:{lineno}")
    if violations:
        fail(
            "workflows must pin the Ubuntu major image (ubuntu-24.04), not ubuntu-latest:\n  "
            + "\n  ".join(violations)
        )


def check_oci_build_action(root: pathlib.Path = ROOT) -> None:
    path = root / ".github" / "actions" / "build-oci" / "action.yml"
    if not path.is_file():
        fail("missing .github/actions/build-oci/action.yml")
    text = path.read_text(encoding="utf-8")
    for required in (
        "platforms: linux/amd64,linux/arm64",
        "outputs: type=oci,dest=${{ inputs.output }}",
        "provenance: mode=max",
    ):
        if required not in text:
            fail(f"build-oci action missing explicit OCI provenance invariant: {required}")
    if "secrets." in text or "secrets[" in text:
        fail("build-oci action must not pass GitHub secrets into max-level provenance builds")


def check_candidate_boundary(root: pathlib.Path = ROOT) -> None:
    candidate = (root / ".github" / "workflows" / "prerelease-candidate.yml").read_text(
        encoding="utf-8"
    )
    if "pull_request_target:" in candidate or "pull_request:" not in candidate:
        fail("prerelease candidate must use pull_request and never pull_request_target")
    if re.search(r"(?m)^permissions:\s*$\n\s{2}contents:\s*read\s*$", candidate) is None:
        fail("prerelease candidate must declare only top-level contents: read")
    if re.search(r"(?m)^\s+[A-Za-z0-9_-]+:\s*write\s*$", candidate) is not None:
        fail("prerelease candidate must not grant write permissions")
    if "secrets." in candidate or "secrets[" in candidate:
        fail("prerelease candidate must not reference secrets")
    for required in (
        "path: .trusted-ci",
        "ref: ${{ github.event.pull_request.base.sha }}",
        "python3 .trusted-ci/scripts/ci/prerelease.py",
        "uses: ./.trusted-ci/.github/actions/setup-project",
        "uses: ./.trusted-ci/.github/actions/build-oci",
        "go-cache: 'false'",
        "bun-cache: 'false'",
    ):
        if required not in candidate:
            fail(f"prerelease candidate missing base-sourced helper invariant: {required}")
    for forbidden in (
        "uses: ./.github/actions/setup-project",
        "uses: ./.github/actions/build-oci",
        "run: python3 scripts/ci/prerelease.py",
    ):
        if forbidden in candidate:
            fail(f"prerelease candidate executes PR-controlled control plane: {forbidden}")


def check_trusted_checkout_refs(root: pathlib.Path = ROOT) -> None:
    publisher = (root / ".github" / "workflows" / "prerelease-publish.yml").read_text(
        encoding="utf-8"
    )
    if "workflow_run:" not in publisher:
        fail("prerelease-publish.yml must remain a workflow_run consumer")
    for line in publisher.splitlines():
        stripped = line.strip()
        if stripped.startswith("ref:") and stripped != "ref: ${{ github.sha }}":
            fail(f"prerelease publisher has non-trusted checkout ref: {stripped}")

    for required in (
        "source_sha: ${{ needs.resolve.outputs.sha }}",
        "trusted_main_sha: ${{ needs.validate.outputs.main_sha }}",
        "pr_number: ${{ needs.resolve.outputs.pr }}",
    ):
        if required not in publisher:
            fail(f"prerelease publisher missing last-mile freshness input: {required}")

    approval_pos = publisher.find("approval:")
    recheck_pos = publisher.find("recheck:")
    publish_pos = publisher.find("publish:")
    if min(approval_pos, recheck_pos, publish_pos) < 0 or not (
        approval_pos < recheck_pos < publish_pos
    ):
        fail("prerelease publication must approve, then recheck, then publish")


def check_release_workflow(root: pathlib.Path = ROOT) -> None:
    release = (root / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8")
    if re.search(r"(?m)^\s{2}push:\s*$", release) is not None or "tags:" in release:
        fail("stable release workflow must not be triggered by tag pushes")
    for required in (
        "workflow_dispatch:",
        "group: stable-release-${{ github.repository }}",
        "environment: ghcr-release",
        "python3 scripts/ci/release.py recheck",
        "target_sha: ${{ needs.guard.outputs.sha }}",
        "REF: ${{ github.ref }}",
        "WORKFLOW_REF: ${{ github.workflow_ref }}",
        "REPOSITORY_OWNER: ${{ github.repository_owner }}",
        "ACTOR: ${{ github.actor }}",
        "TRIGGERING_ACTOR: ${{ github.triggering_actor }}",
        "source_sha: ${{ needs.guard.outputs.sha }}",
        "trusted_main_sha: ${{ needs.guard.outputs.sha }}",
    ):
        if required not in release:
            fail(f"release.yml missing invariant: {required}")

    for line in release.splitlines():
        stripped = line.strip()
        if stripped.startswith("ref: ${{") and stripped not in {
            "ref: ${{ github.sha }}",
            "ref: ${{ needs.guard.outputs.sha }}",
        }:
            fail(f"release workflow has unexpected dynamic checkout ref: {stripped}")

    order = [
        release.find("approval:"),
        release.find("recheck:"),
        release.find("publish-image:"),
        release.find("publish-release:"),
        release.find("promote:"),
    ]
    if any(value < 0 for value in order) or order != sorted(order):
        fail("stable release order must be approval -> recheck -> exact image -> release -> aliases")


def check_prerelease_request_workflow(root: pathlib.Path = ROOT) -> None:
    request = (root / ".github" / "workflows" / "prerelease-request.yml").read_text(
        encoding="utf-8"
    )
    for required in (
        "workflow_dispatch:",
        "ref: ${{ github.sha }}",
        "EVENT_SHA: ${{ github.sha }}",
        "REF: ${{ github.ref }}",
        "WORKFLOW_REF: ${{ github.workflow_ref }}",
        "python3 scripts/ci/prerelease.py request-prepare",
    ):
        if required not in request:
            fail(f"prerelease-request.yml missing current-main request invariant: {required}")


def tracked_files(root: pathlib.Path = ROOT) -> list[str]:
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode == 0:
        return [entry.decode("utf-8") for entry in result.stdout.split(b"\0") if entry]
    # Staged-tree pre-commit checks run from a git archive with no .git directory.
    return [str(path.relative_to(root)) for path in root.rglob("*") if path.is_file()]


def check_certificates(root: pathlib.Path = ROOT) -> None:
    names = tracked_files(root)
    bad_names = [name for name in names if CERT_NAME.search(name)]
    if bad_names:
        fail("tracked TLS certificate/key paths found:\n  " + "\n  ".join(bad_names))
    bad_content: list[str] = []
    for name in names:
        path = root / name
        try:
            data = path.read_bytes()
        except OSError:
            continue
        if PEM.search(data):
            bad_content.append(name)
    if bad_content:
        fail("tracked PEM certificate/private-key material found:\n  " + "\n  ".join(bad_content))


def check_repository(root: pathlib.Path = ROOT) -> None:
    check_external_action_shas(root)
    check_privileged_workflows(root)
    check_runner_labels(root)
    check_oci_build_action(root)
    check_candidate_boundary(root)
    check_trusted_checkout_refs(root)
    check_release_workflow(root)
    check_prerelease_request_workflow(root)
    check_certificates(root)


def main() -> None:
    try:
        check_repository()
    except PolicyError as exc:
        print(f"workflow policy: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc
    print("workflow policy: ok")


if __name__ == "__main__":
    main()
