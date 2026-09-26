#!/usr/bin/env python3
"""Enforce GitHub Actions trust boundaries and keep key material out of Git."""

from __future__ import annotations

import json
import re
import shlex
import subprocess
import tomllib
from pathlib import Path
from typing import NoReturn

from github_api import PEM, TLS_NAME
from toolchains import check as check_toolchain_literals, pin

ROOT = Path(__file__).resolve().parents[2]
USES = re.compile(r"(?m)^\s*(?:-\s*)?uses:\s*(\S+)")
WRITE = re.compile(r"(?<![\w-])(?!permission-)([a-z-]+):\s*write\b")
STEP = re.compile(r"(?m)^(?=\s*- )")
JOB = re.compile(r"(?m)^  (?=[a-z-]+:$)")
RELEASE_SECRETS = {"GHCR_TOKEN", "RELEASE_APP_PRIVATE_KEY"}

TRIGGERS = {
    "ci.yml": {"pull_request", "push"},
    "release-request.yml": {"workflow_dispatch"},
    "release.yml": {"workflow_run"},
}
ALLOWED_USES = {
    "release-request.yml": {
        "actions/checkout", "jdx/mise-action", "./.github/actions/setup-project",
        "./.github/actions/build-oci", "actions/upload-artifact",
    },
    "release.yml": {
        "actions/checkout", "jdx/mise-action", "actions/download-artifact",
        "actions/upload-artifact", "actions/create-github-app-token",
    },
}
ORDERED = {
    "workflows/release-request.yml": (
        "if: ${{ github.ref == format('refs/heads/{0}', github.event.repository.default_branch) }}",
        "run: python3 scripts/ci/release.py prepare",
        'python3 scripts/ci/verify_release_assets.py "$VERSION"',
        "uses: ./.github/actions/build-oci",
        "source-sha: ${{ steps.request.outputs.remote_sha }}",
    ),
    "workflows/release.yml": (
        "github.event.workflow_run.conclusion == 'success'\n",
        "&& github.event.workflow_run.event == 'workflow_dispatch'\n",
        "&& github.event.workflow_run.head_branch == 'main'\n",
        "&& github.event.workflow_run.path == '.github/workflows/release-request.yml'\n",
        "run: python3 scripts/ci/release.py verify",
        "group: release-publish-${{ github.repository }}\n", "cancel-in-progress: false\n",
        "run: python3 scripts/ci/release.py recheck", "run: scripts/ci/publish.sh image",
        "run: scripts/ci/publish.sh release", "run: scripts/ci/publish.sh aliases",
    ),
    "actions/build-oci/action.yml": (
        '[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]', "no-cache: true", "provenance: mode=max",
        "github-token: ''", "GM_CLIENT_REVISION=${{ inputs.revision }}\n",
    ),
}
# Identity that release.py trusts comes from the run context, never from dispatch inputs.
CONTEXT = {
    "run: python3 scripts/ci/release.py prepare": {
        "REPOSITORY": "github.repository", "REPOSITORY_OWNER": "github.repository_owner",
        "ACTOR": "github.actor", "TRIGGERING_ACTOR": "github.triggering_actor",
        "EVENT_NAME": "github.event_name", "EVENT_SHA": "github.sha", "REF": "github.ref",
        "WORKFLOW_REF": "github.workflow_ref", "REQUEST_RUN_ID": "github.run_id",
        "REQUEST_RUN_ATTEMPT": "github.run_attempt",
    },
    "run: python3 scripts/ci/release.py verify": {
        "REPOSITORY": "github.repository", "REPOSITORY_OWNER": "github.repository_owner",
        "PUBLISHER_SHA": "github.sha", "WORKFLOW_REF": "github.workflow_ref",
        "REQUEST_RUN_ID": "github.event.workflow_run.id",
    },
    "run: scripts/ci/publish.sh release": {
        "REPOSITORY": "github.repository", "TARGET_SHA": "github.sha",
    },
}
FORBIDDEN = {
    "workflows/release.yml": ("head_sha", "pull_request.head", "mise run", "secrets["),
    "actions/build-oci/action.yml": (
        "allow-insecure-entitlement", "cache-from:", "cache-to:", "GIT_AUTH_TOKEN",
    ),
}
PATH_FILTERS = {
    "go": ("api/**", "client/src/auth/**", "client/src/app.css"),
    "code": (".dockerignore",),
    "deps": ("client/package.json", "client/bun.lock", "client/bunfig.toml"),
}


class PolicyError(RuntimeError):
    pass


def fail(message: str) -> NoReturn:
    raise PolicyError(message)


def read(root: Path, name: str) -> str:
    return (root / name).read_text(encoding="utf-8")


def run_scripts(text: str) -> list[str]:
    lines = text.splitlines()
    scripts: list[str] = []
    for number, line in enumerate(lines):
        if (match := re.match(r"( *)(- )?run:(.*)", line)) is None:
            continue
        indent = len(match.group(1)) + len(match.group(2) or "")
        body = [match.group(3)]
        for following in lines[number + 1:]:
            if following.strip() and len(following) - len(following.lstrip()) <= indent:
                break
            body.append(following)
        scripts.append("\n".join(body))
    return scripts


def check_actions(root: Path) -> None:
    github = root / ".github"
    files = sorted([*github.glob("workflows/*.y*ml"), *github.glob("actions/**/action.y*ml")])
    mise = f"version: {pin('tools.mise', root)}"
    for path in files:
        name = str(path.relative_to(github))
        text = path.read_text(encoding="utf-8")
        needles = ["ubuntu-latest"]
        if name != "workflows/release.yml":
            needles += ["secrets.", "secrets[", "environment:"]
        for needle in needles:
            if needle in text:
                fail(f"{name} must not use {needle}")
        if re.search(r"uses: (?:actions/setup-(?:go|python)|oven-sh/setup-bun)@", text):
            fail(f"{name} must provision project tools through mise")
        if any("${{" in script for script in run_scripts(text)):
            fail(f"{name}: run scripts must read expressions through env, not interpolate them")
        for step in STEP.split(text):
            if "uses: actions/checkout@" in step:
                if re.search(r"\bref: (?!\$\{\{ github\.sha \}\}$)", step, re.M):
                    fail(f"{name}: checkout may only select the triggering github.sha")
            if "uses: jdx/mise-action@" in step:
                required = [mise, "install_args: --locked ", "cache:", "MISE_AUTO_INSTALL: '0'"]
                if name.startswith("workflows/"):
                    required += ["install_args: --locked python\n", "cache: false"]
                if missing := [item for item in required if item not in step]:
                    fail(f"{name}: mise setup must declare {missing[0].strip()}")
            for marker, bindings in CONTEXT.items():
                env = re.findall(r"(?m)^ +([A-Z_]+): (.*)$", step) if marker in step else []
                for variable, value in bindings.items() if env else ():
                    if [found for key, found in env if key == variable] != [f"${{{{ {value} }}}}"]:
                        fail(f"{name}: {variable} must be exactly ${{{{ {value} }}}}")
        for needle in FORBIDDEN.get(name, ()):
            if needle in text:
                fail(f"{name} must not contain {needle}")
        last = -1
        for needle in ORDERED.get(name, ()):
            if (position := text.find(needle)) <= last:
                fail(f"{name} is missing or misorders invariant: {needle}")
            last = position


def check_workflows(root: Path) -> None:
    workflows = root / ".github" / "workflows"
    names = {path.name for path in workflows.glob("*.y*ml")}
    if names != TRIGGERS.keys():
        fail(f"unreviewed workflow set: {sorted(names ^ TRIGGERS.keys())}")
    for name, expected in TRIGGERS.items():
        text = (workflows / name).read_text(encoding="utf-8")
        block = re.search(r"(?ms)^on:\n(.*?)(?=^\S)", text)
        triggers = set(re.findall(r"(?m)^  ([a-z_]+):", block.group(1) if block else ""))
        if triggers != expected:
            fail(f"{name} must be triggered only by {sorted(expected)}")
        if not re.search(r"(?m)^permissions:", text):
            fail(f"{name} must declare top-level permissions")
        if writes := WRITE.findall(text):
            fail(f"{name} must not grant write permission: {sorted(set(writes))}")
        if name in ALLOWED_USES:
            actions = {ref.split("@", 1)[0] for ref in USES.findall(text)}
            if extra := actions - ALLOWED_USES[name]:
                fail(f"{name} must not run repository code or actions: {sorted(extra)}")
    release = (workflows / "release.yml").read_text(encoding="utf-8")
    publish = "needs.verify.outputs.publish == 'true'"
    for job in JOB.split(release.split("\njobs:\n", 1)[1]):
        secrets = set(re.findall(r"secrets\.(\w+)", job))
        if (secrets or "environment:" in job) and (
            publish not in job or "environment: ghcr-release\n" not in job
            or secrets - RELEASE_SECRETS
        ):
            fail("release.yml: only the publish-mode ghcr-release job may read release secrets")
    for step in STEP.split(release):
        if ("uses: actions/upload-artifact@" in step
                and "if: steps.verify.outputs.publish == 'true'" not in step):
            fail("release.yml: only publish mode may hand off verified artifacts")
    request = (workflows / "release-request.yml").read_text(encoding="utf-8")
    scopes = re.findall(r"(?m)^ *permissions:.*(?:\n +\S.*)*", request)
    if scopes != ["permissions:\n  contents: read"]:
        fail("release-request.yml: the untrusted build may only read contents")
    for step in STEP.split(request.split("\njobs:", 1)[1]):
        if "${{ inputs." in step and "run: python3 scripts/ci/release.py prepare" not in step:
            fail("release-request.yml: dispatch inputs may reach only the request validator")
        if "setup-project" in step and step.count("cache: 'false'") != 3:
            fail("release-request.yml: the untrusted build must disable every cache")
    if "VERSION= mise run legal-check\n" not in request:
        fail("release-request.yml: stable builds must check committed legal outputs first")


def check_ci(root: Path) -> None:
    ci = read(root, ".github/workflows/ci.yml")
    tasks = tomllib.loads(read(root, "mise.toml"))["tasks"]

    def steps(task: str) -> list[str]:
        return [step["task"] for step in tasks[task]["run"] if isinstance(step, dict)]

    for task in steps("ci"):
        if not re.search(rf"mise run {re.escape(task)}(?![\w-])", ci):
            fail(f"CI must run the local gate step {task}")
    sections = re.findall(r"(?ms)^([a-z]+):\n(.*?)(?=^\S|\Z)", read(root, ".github/ci-paths.yml"))
    filters = {section: set(re.findall(r"- '([^']+)'", body)) for section, body in sections}
    for section, paths in PATH_FILTERS.items():
        if missing := set(paths) - filters.get(section, set()):
            fail(f"CI {section} checks must run when {sorted(missing)} change")
    pinned = "${{ steps.toolchain.outputs.chrome-version }}"
    versions = re.findall(r"(?m)^\s+(?:chrome-version|GM_EXPECTED_CHROME_VERSION): (.+)$", ci)
    setup = read(root, ".github/actions/setup-project/action.yml")
    if (
        set(versions) != {pinned} or len(versions) != 2
        or "chrome=$(python3 scripts/ci/toolchains.py get browser.chrome)" not in setup
    ):
        fail("the E2E job must install and version-check the pinned Chromium")
    scripts = json.loads(read(root, "client/package.json"))["scripts"]
    for name in ("test:e2e", "test:bench"):
        if "--no-orphans" not in shlex.split(scripts[name]):
            fail(f"{name} must clean up child processes with --no-orphans")


def check_certificates(root: Path) -> None:
    listed = subprocess.run(["git", "ls-files", "-z"], cwd=root, capture_output=True, check=False)
    if listed.returncode == 0:
        names = [entry.decode() for entry in listed.stdout.split(b"\0") if entry]
    else:
        names = [str(path.relative_to(root)) for path in root.rglob("*") if path.is_file()]
    if bad := [name for name in names if TLS_NAME.search(name)]:
        fail("tracked TLS certificate/key paths found:\n  " + "\n  ".join(bad))
    if bad := [name for name in names if PEM.search((root / name).read_bytes())]:
        fail("tracked PEM certificate/private-key material found:\n  " + "\n  ".join(bad))


def check_repository(root: Path = ROOT) -> None:
    try:
        check_toolchain_literals(root)
    except (ValueError, OSError) as exc:
        fail(str(exc))
    dockerfile = read(root, "container/Dockerfile")
    if unpinned := [image for image in re.findall(r"(?m)^FROM (\S+)", dockerfile)
                    if image != "scratch" and "@sha256:" not in image]:
        fail(f"container/Dockerfile base images must be digest-pinned: {unpinned}")
    if re.search(r"(?im)^\s*#\s*syntax\s*=", dockerfile):
        fail("container/Dockerfile must not select a BuildKit frontend with # syntax=")
    check_actions(root)
    check_workflows(root)
    check_ci(root)
    check_certificates(root)


def main() -> None:
    try:
        check_repository()
    except PolicyError as exc:
        raise SystemExit(f"workflow policy: {exc}") from exc
    print("workflow policy: ok")


if __name__ == "__main__":
    main()
