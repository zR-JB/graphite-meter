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
        if "@sha256:17da3ac5cadf2b27a3dcf7dea857c4cea558ef757641725fc0eec560030057b3" not in text:
            fail(f"{name} must pin the Skopeo container by digest")
        if not re.search(
            r"(?m)^    env:\n"
            r"      # Skopeo v1\.22\.2; the digest/version runtime contract is verified in ci\.yml\.\n"
            r"      SKOPEO_IMAGE: quay\.io/skopeo/stable@sha256:[0-9a-f]{64}$",
            text,
        ):
            fail(
                f"{name} must document and declare its digest-pinned SKOPEO_IMAGE "
                "in the job env mapping"
            )

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
    """Require every Skopeo consumer to use one digest/version contract without a pin DB."""
    images: set[str] = set()
    versions: set[str] = set()
    image_re = re.compile(r"quay\.io/skopeo/stable@sha256:[0-9a-f]{64}")
    version_re = re.compile(r"(?m)^\s*SKOPEO_VERSION:\s*([^\s#]+)\s*$")
    for path in sorted((root / ".github" / "workflows").glob("*.yml")):
        text = path.read_text(encoding="utf-8")
        images.update(image_re.findall(text))
        versions.update(version_re.findall(text))
    if len(images) != 1:
        fail(f"Skopeo consumers must share exactly one digest-pinned image; got {sorted(images)}")
    if len(versions) != 1:
        fail(f"Skopeo verification must share exactly one declared version; got {sorted(versions)}")
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
    svelte_check = (root / "client" / "scripts" / "check-svelte.ts").read_text(
        encoding="utf-8"
    )
    for required in (
        "process.execPath",
        '"--tsgo"',
    ):
        if required not in svelte_check:
            fail(f"Svelte checker missing deterministic TypeScript 7 invariant: {required}")
    if "--tsgo-experimental-api" in svelte_check or ' : [process.execPath, checker' in svelte_check:
        fail("Svelte checker must use Bun-hosted TS7 CLI mode without a TS6 fallback")


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

    script = root / "scripts" / "ci" / "precommit.py"
    if not script.is_file():
        fail("missing typed staged-tree precommit implementation")
    text = script.read_text(encoding="utf-8")
    for required in (
        '"write-tree"',
        '"worktree", "add"',
        '"checkout-index"',
        '"--name-status"',
        '"--diff-filter=ACMRD"',
        '"protect", "--staged"',
        '"--local-env-vars"',
        'prepare_staged_worktree(root, tree, worktree, env=worktree_env)',
        'run_pipeline_checks(worktree, env=worktree_env)',
        'command(("just", recipe), cwd=worktree, env=worktree_env)',
        'server-check", "server-test',
    ):
        if required not in text:
            fail(f"precommit.py missing staged-tree invariant: {required}")

    just = (root / "justfile").read_text(encoding="utf-8")
    if "_pre-commit:" in just or "_pipeline-check-staged:" in just:
        fail("Just must not duplicate staged-tree hook implementation")
    if "server-unit:" in just:
        fail("Just must not call integration-bearing Go tests a unit suite")
    if "server-test:" not in just or "server-race:" not in just:
        fail("Just must expose distinct normal and race Go test recipes")

    ci = (root / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
    if "run: just server-race" not in ci:
        fail("CI Go job must use the explicit race/coverage recipe")


def check_e2e_lifecycle(root: pathlib.Path = ROOT) -> None:
    client = root / "client"
    forbidden = [
        path
        for path in client.rglob("*")
        if path.is_file()
        and "node_modules" not in path.parts
        and ("playwright" in path.name.lower() or path.name.endswith(".pw.ts"))
    ]
    if forbidden:
        fail(f"Playwright configuration/tests must be removed: {forbidden[0]}")

    webview = (client / "browser" / "webview.ts").read_text(encoding="utf-8")
    chrome = (client / "browser" / "chrome.ts").read_text(encoding="utf-8")
    for required in (
        "Bun.serve",
        "port: 0",
        '"cache-control": "no-store"',
        "Runtime.exceptionThrown",
        "Network.setBlockedURLs",
        "Page.addScriptToEvaluateOnNewDocument",
        "screenshot",
        "document.documentElement.outerHTML",
        "page.close()",
        "Bun.WebView.closeAll()",
    ):
        if required not in webview:
            fail(f"WebView harness missing lifecycle/diagnostic invariant: {required}")
    for required in (
        "new Bun.WebView",
        'type: "chrome"',
        "BUN_CHROME_PATH",
        'dataStore: "ephemeral"',
        'process.env.CI || process.env.GM_WEBVIEW_DEBUG ? "inherit" : "ignore"',
    ):
        if required not in chrome:
            fail(f"Chromium launcher missing CI lifecycle/diagnostic invariant: {required}")

    just = (root / "justfile").read_text(encoding="utf-8")
    for required in (
        'GM_E2E_SERVER_BIN="$certs/graphite-meter-e2e"',
        'go build -trimpath -o "$GM_E2E_SERVER_BIN" ./cmd/graphite-meter',
        "GM_E2E_SERVER_BIN",
    ):
        if required not in just:
            fail(f"client-e2e recipe missing prebuilt-server invariant: {required}")

    fixture = (root / "client" / "e2e" / "fixtures.ts").read_text(encoding="utf-8")
    for required in (
        "Bun.spawn([binary]",
        "Bun.serve",
        "port: 0",
        'resolve(".e2e-dist")',
        '"cache-control": "no-store"',
        "Date.now() + 30_000",
        "backend.kill()",
    ):
        if required not in fixture:
            fail(f"E2E fixture missing ephemeral lifecycle invariant: {required}")

    vite = (root / "client" / "vite.e2e.config.ts").read_text(encoding="utf-8")
    for required in (
        "root: here",
        'outDir: resolve(here, ".e2e-dist")',
        'input: resolve(here, "bench/harness.html")',
    ):
        if required not in vite:
            fail(f"E2E harness build missing invariant: {required}")

    for ignore_name in (".gitignore", ".dockerignore"):
        ignore = (root / ignore_name).read_text(encoding="utf-8")
        if "client/.e2e-dist" not in ignore:
            fail(f"{ignore_name} must exclude generated E2E harness output")

    package = (root / "client" / "package.json").read_text(encoding="utf-8")
    for required in (
        '"test:browser": "bun run build:browser && bun test browser --no-orphans --timeout 30000"',
        '"test:e2e": "bun run build:e2e-harness && bun test e2e --no-orphans --timeout 60000"',
        '"test:bench": "bun run build:e2e-harness && bun test ./bench/throughput.bench.ts --no-orphans --timeout 1800000"',
        '"axe-core"',
        '"check:webview": "bun run scripts/check-webview.ts"',
    ):
        if required not in package:
            fail(f"client browser scripts missing Bun.WebView invariant: {required}")
    preflight = (client / "scripts" / "check-webview.ts").read_text(encoding="utf-8")
    for required in (
        "BUN_CHROME_PATH",
        "GM_EXPECTED_CHROME_VERSION",
        'view.navigate("about:blank")',
        'view.cdp<{ product: string }>("Browser.getVersion")',
        "view.close()",
        "Bun.WebView.closeAll()",
    ):
        if required not in preflight:
            fail(f"WebView launch preflight missing invariant: {required}")
    if 'process.on("exit"' not in webview or "Bun.WebView.closeAll();" not in webview:
        fail("WebView suites must explicitly close the shared browser at process exit")
    for forbidden_dependency in ("@playwright/test", "@axe-core/playwright", "puppeteer"):
        if forbidden_dependency in package:
            fail(f"browser dependency must be removed: {forbidden_dependency}")

    ci = (root / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
    for required in (
        "browser-actions/setup-chrome@48483d551c22a1fe6154c6a195ccc6d5773dc8c4",
        "chrome-version: 152.0.7977.54",
        "BUN_CHROME_PATH: ${{ steps.chrome.outputs.chrome-path }}",
        "BUN_CHROME_ARGS: --no-sandbox",
        "GM_EXPECTED_CHROME_VERSION: 152.0.7977.54",
        "run: cd client && bun run check:webview",
        "webview-browser-failures",
        "webview-e2e-failures",
    ):
        if required not in ci:
            fail(f"CI missing pinned Chromium/WebView invariant: {required}")
    if ci.count("run: cd client && bun run check:webview") != 2:
        fail("each CI WebView job must perform an isolated launch preflight")

    for fixture_name in ("e2e/fixtures.ts", "bench/fixtures.ts"):
        fixture_text = (client / fixture_name).read_text(encoding="utf-8")
        if "process.env.BUN_CHROME_ARGS," not in fixture_text:
            fail(f"{fixture_name} must preserve CI Chromium launch arguments")

def check_oci_verifier_boundary(root: pathlib.Path = ROOT) -> None:
    verifier = (root / "scripts" / "ci" / "verify_oci.py").read_text(encoding="utf-8")
    for required in (
        '"--network",',
        '"none",',
        'f"{archive.resolve()}:/work/image.oci.tar:ro"',
        '"oci:/tmp/graphite-meter-verified:verified"',
        "archive.is_symlink()",
    ):
        if required not in verifier:
            fail(f"OCI verifier missing local/no-network isolation invariant: {required}")
    for forbidden in (
        "TemporaryDirectory",
        "/work/oci-copy",
        ":/work/oci-copy",
    ):
        if forbidden in verifier:
            fail(f"OCI verifier must not create host-writable container output: {forbidden}")


def check_release_verifier_boundary(root: pathlib.Path = ROOT) -> None:
    verifier = (root / "scripts" / "ci" / "verify_release_assets.py").read_text(
        encoding="utf-8"
    )
    for required in (
        '[str(binary.resolve()), "--version"]',
        "subprocess.run(",
        "server --version failed",
    ):
        if required not in verifier:
            fail(f"release verifier missing direct binary-version invariant: {required}")
    for forbidden in (
        "subprocess.Popen(",
        "urllib.",
        '"/preflight"',
        "GM_H1_ADDR",
        "127.0.0.1:7246",
    ):
        if forbidden in verifier:
            fail(f"release verifier must not start a fixed-port server: {forbidden}")

    for required in (
        'graphite-meter_{version}_third-party-source.tar.gz',
        "verify_third_party_source_archive",
        "third_party/go/",
        "third_party/npm/",
        "third_party/manual/",
        "PROVENANCE.json",
        "Source code (tar.gz)",
        "does not duplicate Graphite Meter's own repository source",
    ):
        if required not in verifier:
            fail(f"release verifier missing split source-offer invariant: {required}")
    if "corresponding-source.tar.gz" in verifier:
        fail("release verifier must not require the obsolete duplicated corresponding-source asset")

    just = (root / "justfile").read_text(encoding="utf-8")
    if 'python3 scripts/ci/verify_release_assets.py "{{ version }}"' not in just:
        fail("ordinary release-check must exercise the same native artifact verifier as stable release")
    for required in (
        "_legal-third-party-source-bundle:",
        "third-party-source-bundle",
        "graphite-meter_{{ version }}_third-party-source.tar.gz",
        "LEGAL_THIRD_PARTY_SOURCE_OUT",
    ):
        if required not in just:
            fail(f"release packaging missing split source-offer invariant: {required}")
    if "_legal-source-bundle:" in just or "_corresponding-source.tar.gz" in just:
        fail("release packaging must not recreate the obsolete project+dependency source bundle")

    generator = (root / "go" / "internal" / "legal" / "cmd" / "legalgen" / "main.go").read_text(encoding="utf-8")
    for required in (
        '"third-party-source-bundle"',
        "thirdPartySourceBundle",
        'archiveRoot+"/third_party/go/"',
        'archiveRoot+"/third_party/npm/"',
        'archiveRoot+"/PROVENANCE.json"',
        "manualSourceDestination",
    ):
        if required not in generator:
            fail(f"legal source generator missing split source-offer invariant: {required}")
    if 'archiveRoot+"/project"' in generator or "corresponding-source.tar.gz" in generator:
        fail("third-party source generator must not duplicate Graphite Meter project source")


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
    check_e2e_lifecycle(root)
    check_oci_verifier_boundary(root)
    check_release_verifier_boundary(root)
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
