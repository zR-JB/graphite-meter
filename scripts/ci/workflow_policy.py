#!/usr/bin/env python3
"""Dependency-free repository policy checks for the GitHub Actions control plane.

GitHub repository settings are the authority for *which* external action
repositories may execute. This local checker deliberately does not mirror that
allowlist or maintain a second SHA database; it only enforces invariants that
are useful before GitHub schedules a workflow, including immutable 40-SHA refs.
"""

from __future__ import annotations

import json
import pathlib
import re
import shlex
import subprocess
import sys
from typing import NoReturn

from toolchains import check as check_toolchain_literals, pin, skopeo_version

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
        if text.count('"$SKOPEO_IMAGE" -ec') != 1:
            fail(f"{name} must execute Skopeo through the exact \"$SKOPEO_IMAGE\" runtime")
        if not re.search(
            r"(?m)^    env:\n"
            r"      SKOPEO_IMAGE: " + re.escape(pin("images.skopeo", root)) + r"$",
            text,
        ):
            fail(
                f"{name} must declare its digest-pinned SKOPEO_IMAGE "
                "in the job env mapping"
            )
        if "SKOPEO_VERSION" in text or "skopeo --version" in text:
            fail(f"{name} must not declare or parse SKOPEO_VERSION")

    oci = (workflows / "_publish-oci.yml").read_text(encoding="utf-8")
    if "group: publish-oci-${{ github.repository }}-${{ inputs.tag }}" not in oci:
        fail("_publish-oci.yml must serialize publication by exact destination tag")
    for required in (
        "source_sha:",
        "trusted_main_sha:",
        "pr_number:",
        "expected_ci_run_id:",
        "expected_codeql_check_id:",
        "contents: read",
        "pull-requests: read",
        "checks: read",
        "security-events: read",
        'gh api "repos/$REPOSITORY/commits/main"',
        'gh api "repos/$REPOSITORY/actions/runs/$EXPECTED_CI_RUN_ID"',
        'gh api "repos/$REPOSITORY/pulls/$PR_NUMBER"',
        'gh api "repos/$REPOSITORY/compare/$TRUSTED_MAIN_SHA...$SOURCE_SHA"',
        'code-scanning/analyses?ref=refs/heads/main&tool_name=CodeQL',
        'commits/$SOURCE_SHA/check-runs?per_page=100',
        "live source, CI, and CodeQL freshness checks passed immediately before registry authentication",
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
        "release handoff contains a non-regular entry",
        "release handoff contains an unsafe asset name",
        'source_asset="graphite-meter_${version}_third-party-source.tar.gz"',
        "source_notice=$(printf '%s\\n\\n%s\\n\\n%s'",
        "Source code (zip)",
        "Source code (tar.gz)",
        "generate_release_notes:true,body:$body",
        "source-availability notice is missing or stale",
        "published release lost its source-availability notice",
    ):
        if required not in text:
            fail(f"_publish-release.yml missing invariant: {required}")
    for forbidden in (
        "releases/tags/$TAG",
        "gh release upload",
        "--show-error -L",
        "--location",
        "--location-trusted",
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


def check_skopeo_contract_consistency(root: pathlib.Path = ROOT) -> None:
    """Require every Skopeo consumer to use the exact immutable image contract."""
    consumers = (
        "ci.yml",
        "release.yml",
        "prerelease-publish.yml",
        "_publish-oci.yml",
        "_promote-oci.yml",
    )
    workflows = root / ".github" / "workflows"
    for name in consumers:
        path = workflows / name
        text = path.read_text(encoding="utf-8")
        images = [
            line.split(":", 1)[1].strip()
            for line in text.splitlines()
            if line.lstrip().startswith("SKOPEO_IMAGE:")
        ]
        verifier = name in ("ci.yml", "release.yml", "prerelease-publish.yml")
        expected_images = [pin("images.skopeo", root)]
        if verifier:
            expected_images.append("${{ env.SKOPEO_IMAGE }}")
        if images != expected_images:
            fail(f"{name} has a non-exact SKOPEO_IMAGE assignment")
        if verifier and ("SKOPEO_VERSION: " + skopeo_version(root)) not in text:
            fail(f"{name} is missing the exact SKOPEO_VERSION declaration")
        if not verifier and "SKOPEO_VERSION" in text:
            fail(f"{name} must not declare SKOPEO_VERSION")
    ci = (root / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
    if "python3 scripts/ci/verify_oci.py --check-skopeo" not in ci:
        fail("CI release checks must execute the pinned Skopeo runtime contract")


def check_ci_path_map(root: pathlib.Path = ROOT) -> None:
    path = root / ".github" / "ci-paths.yml"
    text = path.read_text(encoding="utf-8")
    for section in ("smoke", "release"):
        match = re.search(
            rf"(?ms)^{section}:\n(?P<body>.*?)(?=^[A-Za-z0-9_-]+:|\Z)",
            text,
        )
        if match is None or ".dockerignore" not in match.group("body"):
            fail(f"CI path map must run {section} checks when .dockerignore changes")
    security = re.search(
        r"(?ms)^security:\n(?P<body>.*?)(?=^[A-Za-z0-9_-]+:|\Z)", text
    )
    security_paths = ("client/package.json", "client/bun.lock", "client/bunfig.toml")
    if security is None or any(path not in security.group("body") for path in security_paths):
        fail("CI security paths must include the client manifest, lockfile, and Bun config")

    ci = (root / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
    if "just client-audit" not in ci:
        fail("CI security job must run the networked Bun audit")
    just = (root / "justfile").read_text(encoding="utf-8")
    if "    just security\n    just client-audit\n" not in just:
        fail("local CI-equivalent gate must run both Go and Bun vulnerability scans")


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
        "cache-image: 'false'",
        "cache-binary: 'false'",
        "buildkitd-flags: --log-level=info",
        "no-cache: true",
        "source-sha:",
        "default: ''",
        '[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]',
        'context="https://github.com/${REPOSITORY}.git#${SOURCE_SHA}"',
        "context: ${{ steps.source.outputs.context }}",
        "file: container/Dockerfile",
        "CLIENT_VERSION=${{ inputs.version }}",
        "GM_CLIENT_BUILD_PROFILE=prod",
        "GM_CLIENT_REVISION=${{ inputs.revision }}",
        "github-token: ''",
        "DOCKER_BUILD_RECORD_UPLOAD: 'false'",
        "bun=$(tr -d '[:space:]' < .bun-version)",
    ):
        if required not in text:
            fail(f"build-oci action missing explicit OCI provenance invariant: {required}")
    if re.search(
        r"(?m)^\s*image:\s*docker\.io/tonistiigi/binfmt@sha256:[0-9a-f]{64}\s*$",
        text,
    ) is None:
        fail("build-oci action must pin the privileged binfmt/QEMU image by digest")
    if re.search(r"(?m)^\s*buildkitd-flags:.*allow-insecure-entitlement", text) is not None:
        fail("build-oci action must not enable BuildKit insecure entitlements")

    for forbidden in (
        "source-dir:",
        "inputs.source-dir",
        "GIT_AUTH_TOKEN",
        "cache-from:",
        "cache-to:",
    ):
        if forbidden in text:
            fail(f"build-oci action contains forbidden mutable/cache/token path: {forbidden}")
    if "secrets." in text or "secrets[" in text:
        fail("build-oci action must not pass GitHub secrets into max-level provenance builds")


def check_setup_project_cache_boundary(root: pathlib.Path = ROOT) -> None:
    setup = (root / ".github" / "actions" / "setup-project" / "action.yml").read_text(
        encoding="utf-8"
    )
    if "no-cache: ${{ inputs.bun-cache != 'true' }}" not in setup:
        fail(
            "setup-project must disable setup-bun executable caching when bun-cache is false"
        )


def check_candidate_boundary(root: pathlib.Path = ROOT) -> None:
    """Keep the manual prerelease producer low-authority and PR-source-free on the runner."""
    workflows = root / ".github" / "workflows"
    if (workflows / "prerelease-candidate.yml").exists():
        fail("standalone label-triggered prerelease candidate workflow must remain removed")
    request = (workflows / "prerelease-request.yml").read_text(encoding="utf-8")
    if "workflow_dispatch:" not in request or "pull_request_target:" in request:
        fail("prerelease request/candidate producer must remain workflow_dispatch-only")
    if re.search(r"(?m)^permissions:\s*$\n\s{2}contents:\s*read\s*$", request) is None:
        fail("prerelease request/candidate producer must declare only top-level contents: read")
    if re.search(r"(?m)^\s+[A-Za-z0-9_-]+:\s*write\s*$", request) is not None:
        fail("prerelease request/candidate producer must not grant write permissions")
    if "secrets." in request or "secrets[" in request:
        fail("prerelease request/candidate producer must not reference secrets")

    for required in (
        "if: ${{ github.ref == format('refs/heads/{0}', github.event.repository.default_branch) }}",
        "ref: ${{ github.sha }}",
        ".bun-version",
        "uses: ./.github/actions/build-oci",
        "source-sha: ${{ steps.request.outputs.sha }}",
        "client-validate: '1'",
        "REQUESTED_SHA: ${{ inputs.sha }}",
        "run: python3 scripts/ci/prerelease.py request-prepare",
        "run: python3 scripts/ci/prerelease.py request-finalize",
        "prerelease-candidate-${{ github.run_id }}",
    ):
        if required not in request:
            fail(f"prerelease request/candidate producer missing isolation invariant: {required}")

    if request.count("uses: actions/checkout@") != 1:
        fail("prerelease request/candidate producer must checkout trusted tooling exactly once")
    if request.count("${{ inputs.sha }}") != 1:
        fail("raw prerelease SHA input must reach only trusted request validation")
    validation_pos = request.find("run: python3 scripts/ci/prerelease.py request-prepare")
    build_pos = request.find("uses: ./.github/actions/build-oci")
    if validation_pos < 0 or build_pos < 0 or validation_pos > build_pos:
        fail("prerelease request SHA must be validated before the remote BuildKit build")

    for forbidden in (
        "ref: ${{ inputs.sha }}",
        "path: source",
        "source-dir:",
        "uses: ./.github/actions/setup-project",
        "uses: ./source/",
        "run: source/",
        "run: ./source/",
        "run: just ",
        "actions: read",
        "checks: read",
        "pull-requests: read",
        "security-events: read",
        "issues: write",
        "gm-prerelease-",
    ):
        if forbidden in request:
            fail(f"prerelease request/candidate producer contains forbidden path: {forbidden}")

    allowed_runs = {
        "run: python3 scripts/ci/prerelease.py request-prepare",
        "run: python3 scripts/ci/prerelease.py request-finalize",
    }
    actual_runs = {
        line.strip() for line in request.splitlines() if line.strip().startswith("run:")
    }
    if actual_runs != allowed_runs:
        fail(
            "prerelease request/candidate host run steps must be limited to request shape/finalize helpers; "
            f"got {sorted(actual_runs)}"
        )


def check_trusted_checkout_refs(root: pathlib.Path = ROOT) -> None:
    publisher = (root / ".github" / "workflows" / "prerelease-publish.yml").read_text(
        encoding="utf-8"
    )
    if "workflow_run:" not in publisher or 'workflows: ["Request PR prerelease"]' not in publisher:
        fail("prerelease-publish.yml must remain a workflow_run consumer of the low-authority request")
    for line in publisher.splitlines():
        stripped = line.strip()
        if stripped.startswith("ref:") and stripped != "ref: ${{ github.sha }}":
            fail(f"prerelease publisher has non-trusted checkout ref: {stripped}")
    for forbidden in (
        "path: source",
        "github.event.pull_request.head.sha",
        "uses: ./source/",
        "prerelease-candidate.yml",
        "LABEL_",
        "issues: write",
    ):
        if forbidden in publisher:
            fail(f"prerelease publisher must never checkout/execute PR source or manage request labels: {forbidden}")

    for required in (
        "PUBLISHER_SHA: ${{ github.sha }}",
        "REQUEST_RUN_ID: ${{ github.event.workflow_run.id }}",
        "source_sha: ${{ needs.validate.outputs.sha }}",
        "trusted_main_sha: ${{ needs.validate.outputs.main_sha }}",
        "pr_number: ${{ needs.validate.outputs.pr }}",
        "expected_ci_run_id: ${{ needs.recheck.outputs.ci_run_id }}",
        "expected_codeql_check_id: ${{ needs.recheck.outputs.codeql_check_id }}",
    ):
        if required not in publisher:
            fail(f"prerelease publisher missing trusted-consumer/freshness input: {required}")

    handoff_start = publisher.find("- name: Upload trusted publication handoff")
    if handoff_start < 0:
        fail("prerelease publisher is missing the trusted publication handoff")
    handoff_end = publisher.find("\n      - ", handoff_start + 1)
    handoff = publisher[handoff_start:] if handoff_end < 0 else publisher[handoff_start:handoff_end]
    if "retention-days: 35" not in handoff:
        fail("trusted prerelease handoff must survive the maximum environment approval window")

    approval_pos = publisher.find("approval:")
    recheck_pos = publisher.find("recheck:")
    publish_pos = publisher.find("publish:")
    if min(approval_pos, recheck_pos, publish_pos) < 0 or not (
        approval_pos < recheck_pos < publish_pos
    ):
        fail("prerelease publication must approve, then recheck, then publish")


def check_release_request_workflow(root: pathlib.Path = ROOT) -> None:
    path = root / ".github" / "workflows" / "release-request.yml"
    if not path.is_file():
        fail("missing low-authority stable release request workflow")
    request = path.read_text(encoding="utf-8")
    for label, needle in {
        "repository checkout": "actions/checkout@",
        "local action execution": "uses: ./",
        "repository script execution": "scripts/",
        "Just execution": "just ",
        "write permission": ": write",
        "environment approval": "environment:",
        "secret reference": "secrets.",
    }.items():
        if needle in request:
            fail(f"release-request.yml contains forbidden {label}: {needle}")
    for required in (
        "workflow_dispatch:",
        "permissions: {}",
        "REF: ${{ github.ref }}",
        "SOURCE_SHA: ${{ github.sha }}",
        "RUN_ATTEMPT: ${{ github.run_attempt }}",
        "[[ \"$REF\" == refs/heads/main ]]",
        "stable-release-request-${{ github.run_id }}",
        "retention-days: 1",
    ):
        if required not in request:
            fail(f"release-request.yml missing low-authority request invariant: {required}")


def check_release_workflow(root: pathlib.Path = ROOT) -> None:
    release = (root / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8")
    if re.search(r"(?m)^\s{2}push:\s*$", release) is not None or "tags:" in release:
        fail("stable release consumer must not be triggered by tag pushes")
    if "workflow_dispatch:" in release:
        fail("write-capable stable release consumer must not be directly workflow_dispatch-triggered")
    if "needs.guard.outputs.sha" in release:
        fail("stable release source identity must use immutable workflow_run github.sha directly, not a guard output")

    for required in (
        "workflow_run:",
        'workflows: ["Request stable release"]',
        "types: [completed]",
        "group: stable-release-${{ github.repository }}",
        "environment: ghcr-release",
        "python3 scripts/ci/release.py recheck",
        "target_sha: ${{ github.sha }}",
        "PUBLISHER_SHA: ${{ github.sha }}",
        "WORKFLOW_REF: ${{ github.workflow_ref }}",
        "REQUEST_RUN_ID: ${{ github.event.workflow_run.id }}",
        "stable-release-request-${{ github.event.workflow_run.id }}",
        "run-id: ${{ github.event.workflow_run.id }}",
        "source_sha: ${{ github.sha }}",
        "trusted_main_sha: ${{ github.sha }}",
        "expected_ci_run_id: ${{ needs.recheck.outputs.ci_run_id }}",
    ):
        if required not in release:
            fail(f"release.yml missing trusted-consumer invariant: {required}")

    if "just release-check" in release:
        fail(
            "stable release build must verify the exact built payload, not rebuild a second "
            "representative release-check payload"
        )
    native_verify = release.find("python3 scripts/ci/verify_release_assets.py")
    oci_build = release.find("uses: ./.github/actions/build-oci")
    if native_verify < 0 or oci_build < 0 or native_verify > oci_build:
        fail("stable release must fail-fast on exact native artifact verification before OCI build")

    for step_name in (
        "Upload verified release image handoff",
        "Upload verified release asset handoff",
    ):
        start = release.find(f"- name: {step_name}")
        if start < 0:
            fail(f"release.yml missing handoff step: {step_name}")
        end = release.find("\n      - ", start + 1)
        step = release[start:] if end < 0 else release[start:end]
        if "if: needs.guard.outputs.publish == 'true'" not in step:
            fail(f"stable validate mode must not upload publication handoff: {step_name}")
        if "retention-days: 35" not in step:
            fail(f"publication handoff must survive delayed environment approval: {step_name}")

    for line in release.splitlines():
        stripped = line.strip()
        if stripped.startswith("ref: ${{") and stripped != "ref: ${{ github.sha }}":
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
        "if: ${{ github.ref == format('refs/heads/{0}', github.event.repository.default_branch) }}",
        "ref: ${{ github.sha }}",
        "EVENT_SHA: ${{ github.sha }}",
        "REF: ${{ github.ref }}",
        "WORKFLOW_REF: ${{ github.workflow_ref }}",
        "REQUESTED_SHA: ${{ inputs.sha }}",
        "source-sha: ${{ steps.request.outputs.sha }}",
        "python3 scripts/ci/prerelease.py request-prepare",
        "python3 scripts/ci/prerelease.py request-finalize",
    ):
        if required not in request:
            fail(f"prerelease-request.yml missing low-authority request invariant: {required}")
    for forbidden in ("issues: write", "request-label", "pull_request:", "pull_request_target:"):
        if forbidden in request:
            fail(f"prerelease-request.yml contains obsolete/privileged trigger path: {forbidden}")


def check_precommit_boundary(root: pathlib.Path = ROOT) -> None:
    hook = root / ".githooks" / "pre-commit"
    if not hook.is_file():
        fail("missing .githooks/pre-commit")
    hook_text = hook.read_text(encoding="utf-8")
    for required in (
        "git show :scripts/ci/precommit.py",
        'python3 "$tmp"',
    ):
        if required not in hook_text:
            fail(
                "pre-commit hook must execute the typed implementation from the staged index: "
                + required
            )
    if "python3 scripts/ci/precommit.py" in hook_text:
        fail("pre-commit hook must not execute a possibly-unstaged working-tree implementation")
    if hook.stat().st_mode & 0o111 == 0:
        fail(".githooks/pre-commit must remain executable")


def check_toolchain_consumers(root: pathlib.Path = ROOT) -> None:
    """Runtime setup consumes native selectors, never another literal version."""
    selectors = {
        "go-version-file": "go/go.mod",
        "bun-version-file": ".bun-version",
        "python-version-file": ".python-version",
    }
    for path in workflow_files(root):
        text = path.read_text(encoding="utf-8")
        if re.search(r"(?m)(?:^|[\s{])(?:go-version|bun-version|python-version):", text):
            fail(f"{path.relative_to(root)} must use native toolchain version files")
        for field, value in re.findall(r"(?m)^\s*(go-version-file|bun-version-file|python-version-file): (.+)$", text):
            if value != selectors[field]:
                fail(f"{path.relative_to(root)} must read {field} from {selectors[field]}")
    setup = (root / ".github/actions/setup-project/action.yml").read_text(encoding="utf-8")
    for declaration in (
        "just=$(python3 scripts/ci/toolchains.py get tools.just)",
        "just-version: ${{ steps.versions.outputs.just }}",
        "python-version-file: .python-version",
        "go-version-file: go/go.mod",
        "bun-version-file: .bun-version",
    ):
        if declaration not in setup:
            fail(f"setup-project must consume the toolchain owner: {declaration}")


def check_browser_ci(root: pathlib.Path = ROOT) -> None:
    """Keep browser identity and process cleanup explicit; suites verify the harness itself."""
    ci = (root / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    expected = "${{ steps.toolchain.outputs.chrome-version }}"
    versions = re.findall(r"(?m)^\s+(?:chrome-version|GM_EXPECTED_CHROME_VERSION): (.+)$", ci)
    setup = (root / ".github/actions/setup-project/action.yml").read_text(encoding="utf-8")
    if (
        versions != [expected] * 4
        or ci.count("      - id: toolchain\n") != 2
        or "chrome=$(python3 scripts/ci/toolchains.py get browser.chrome)" not in setup
        or "value: ${{ steps.versions.outputs.chrome }}" not in setup
    ):
        fail("both browser jobs must install and verify the manifest-pinned Chromium version")
    if ci.count("run: cd client && bun run check:webview") != 2:
        fail("each browser job must perform the runtime launch preflight")
    scripts = json.loads((root / "client/package.json").read_text(encoding="utf-8"))["scripts"]
    for name in ("test:browser", "test:e2e", "test:bench"):
        if "--no-orphans" not in shlex.split(scripts[name]):
            fail(f"{name} must clean up child processes with --no-orphans")


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
    # Bundle/staged-archive validation may run without a .git directory.
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
        except OSError as exc:
            fail(f"cannot read tracked file {name!r} during certificate scan: {exc}")
        if PEM.search(data):
            bad_content.append(name)
    if bad_content:
        fail("tracked PEM certificate/private-key material found:\n  " + "\n  ".join(bad_content))


def check_repository(root: pathlib.Path = ROOT) -> None:
    try:
        check_toolchain_literals(root)
    except (ValueError, OSError) as exc:
        fail(str(exc))
    check_toolchain_consumers(root)
    check_external_action_shas(root)
    check_privileged_workflows(root)
    check_skopeo_contract_consistency(root)
    check_ci_path_map(root)
    check_runner_labels(root)
    check_oci_build_action(root)
    check_setup_project_cache_boundary(root)
    check_candidate_boundary(root)
    check_trusted_checkout_refs(root)
    check_release_request_workflow(root)
    check_release_workflow(root)
    check_prerelease_request_workflow(root)
    check_precommit_boundary(root)
    check_browser_ci(root)
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
