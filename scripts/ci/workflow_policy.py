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

from precommit import PEM, TLS_NAME
from toolchains import check as check_toolchain_literals, pin

ROOT = Path(__file__).resolve().parents[2]
NAME = r"[A-Za-z0-9_.-]+"
USES = re.compile(r"(?m)^\s*(?:-\s*)?uses:\s*(\S+)")
PINNED = re.compile(rf"{NAME}/{NAME}(?:/{NAME})*@[0-9a-f]{{40}}")
WRITE = re.compile(r"(?m)^\s+([a-z-]+):\s*write\s*$")
STEP = re.compile(r"(?m)^(?=\s*- )")

TRIGGERS = {
    "ci.yml": {"pull_request", "push"},
    "release-request.yml": {"workflow_dispatch"},
    "release.yml": {"workflow_run"},
    "_publish-oci.yml": {"workflow_call"},
    "_publish-release.yml": {"workflow_call"},
    "_promote-oci.yml": {"workflow_call"},
}
WRITERS = {
    "release.yml": {"contents", "packages"},
    "_publish-oci.yml": {"packages"},
    "_publish-release.yml": {"contents"},
    "_promote-oci.yml": {"packages"},
}
ALLOWED_USES = {
    "release-request.yml": {
        "actions/checkout", "jdx/mise-action", "./.github/actions/setup-project",
        "./.github/actions/build-oci", "actions/upload-artifact",
    },
    "release.yml": {
        "actions/checkout", "jdx/mise-action", "actions/download-artifact",
        "actions/upload-artifact", "./.github/workflows/_publish-oci.yml",
        "./.github/workflows/_publish-release.yml", "./.github/workflows/_promote-oci.yml",
    },
    "_publish-oci.yml": {"actions/download-artifact"},
    "_publish-release.yml": {"actions/download-artifact"},
    "_promote-oci.yml": set(),
}
ORDERED = {
    "workflows/release-request.yml": (
        "if: ${{ github.ref == format('refs/heads/{0}', github.event.repository.default_branch) }}",
        "run: python3 scripts/ci/release.py prepare",
        "uses: ./.github/actions/build-oci",
        "source-sha: ${{ steps.request.outputs.remote_sha }}",
    ),
    "workflows/release.yml": (
        "run: python3 scripts/ci/release.py verify", "approval:", "environment: ghcr-release",
        "recheck:", "run: python3 scripts/ci/release.py recheck", "publish-image:",
        "publish-release:", "target_sha: ${{ github.sha }}", "promote:",
    ),
    "workflows/_publish-oci.yml": (
        "group: publish-oci-${{ github.repository }}-${{ inputs.tag }}",
        'gh api "repos/$REPOSITORY/commits/main"',
        'gh api "repos/$REPOSITORY/actions/runs/$EXPECTED_CI_RUN_ID"',
        "code-scanning/analyses?ref=refs/heads/main&tool_name=CodeQL",
        'gh api "repos/$REPOSITORY/pulls/$PR_NUMBER"',
        'gh api "repos/$REPOSITORY/compare/$TRUSTED_MAIN_SHA...$SOURCE_SHA"',
        "commits/$SOURCE_SHA/check-runs?per_page=100",
        "skopeo login",
    ),
    "workflows/_promote-oci.yml": (
        "group: promote-stable-oci-${{ github.repository }}", "sort -V", "skopeo login",
    ),
    "actions/build-oci/action.yml": (
        '[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]', "no-cache: true", "provenance: mode=max",
        "github-token: ''",
    ),
}
FORBIDDEN = {
    "workflows/release.yml": ("head_sha", "pull_request.head", "mise run"),
    "workflows/_publish-oci.yml": ("environment:",),
    "workflows/_publish-release.yml": (
        "environment:", "--location", "gh release upload", "releases/tags/$TAG",
    ),
    "workflows/_promote-oci.yml": ("environment:",),
    "actions/build-oci/action.yml": (
        "allow-insecure-entitlement", "cache-from:", "cache-to:", "GIT_AUTH_TOKEN",
    ),
}
PATH_FILTERS = {
    "go": ("api/**", "client/src/auth/**", "client/src/app.css"),
    "smoke": (".dockerignore",),
    "release": (".dockerignore",),
    "security": ("client/package.json", "client/bun.lock", "client/bunfig.toml"),
}
CI_SUPERSETS = {"server-test": "server-race", "legal-check": "legal-generate"}


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
        for ref in USES.findall(text):
            if not ref.startswith("./") and PINNED.fullmatch(ref) is None:
                fail(f"{name}: external action must use a full 40-character commit SHA: {ref}")
        for needle in ("secrets.", "secrets[", "pull_request_target", "write-all", "ubuntu-latest"):
            if needle in text:
                fail(f"{name} must not use {needle}")
        if re.search(r"uses: (?:actions/setup-(?:go|python)|oven-sh/setup-bun)@", text):
            fail(f"{name} must provision project tools through mise")
        if any("${{" in script for script in run_scripts(text)):
            fail(f"{name}: run scripts must read expressions through env, not interpolate them")
        for step in STEP.split(text):
            if "uses: actions/checkout@" in step:
                if "persist-credentials: false" not in step:
                    fail(f"{name}: checkout must set persist-credentials: false")
                if re.search(r"\bref: (?!\$\{\{ github\.sha \}\}$)", step, re.M):
                    fail(f"{name}: checkout may only select the triggering github.sha")
            if "uses: jdx/mise-action@" in step:
                required = [mise, "install_args: --locked ", "cache:", "MISE_AUTO_INSTALL: '0'"]
                if name.startswith("workflows/"):
                    required += ["install_args: --locked python\n", "cache: false"]
                if missing := [item for item in required if item not in step]:
                    fail(f"{name}: mise setup must declare {missing[0].strip()}")
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
        if expected != {"workflow_call"} and not re.search(r"(?m)^permissions:", text):
            fail(f"{name} must declare top-level permissions")
        if extra := set(WRITE.findall(text)) - WRITERS.get(name, set()):
            fail(f"{name} must not grant write permission: {sorted(extra)}")
        if name in ALLOWED_USES:
            actions = {ref.split("@", 1)[0] for ref in USES.findall(text)}
            if extra := actions - ALLOWED_USES[name]:
                fail(f"{name} must not run repository code or actions: {sorted(extra)}")
    request = (workflows / "release-request.yml").read_text(encoding="utf-8")
    for step in STEP.split(request.split("\njobs:", 1)[1]):
        if "${{ inputs." in step and "run: python3 scripts/ci/release.py prepare" not in step:
            fail("release-request.yml: dispatch inputs may reach only the request validator")
        if "setup-project" in step and step.count("cache: 'false'") != 3:
            fail("release-request.yml: the untrusted build must disable every cache")


def check_ci(root: Path) -> None:
    ci = read(root, ".github/workflows/ci.yml")
    tasks = tomllib.loads(read(root, "mise.toml"))["tasks"]

    def steps(task: str) -> list[str]:
        return [step["task"] for step in tasks[task]["run"] if isinstance(step, dict)]

    gate = [leaf for step in steps("ci") for leaf in (steps(step) if step == "check" else [step])]
    for task in gate:
        if not re.search(rf"mise run {re.escape(CI_SUPERSETS.get(task, task))}(?![\w-])", ci):
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
        set(versions) != {pinned} or ci.count("bun run check:webview") != 2
        or "chrome=$(python3 scripts/ci/toolchains.py get browser.chrome)" not in setup
    ):
        fail("browser jobs must install and launch-check the pinned Chromium")
    scripts = json.loads(read(root, "client/package.json"))["scripts"]
    for name in ("test:browser", "test:e2e", "test:bench"):
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
