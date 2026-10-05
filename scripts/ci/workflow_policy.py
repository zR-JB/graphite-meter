#!/usr/bin/env python3
"""Enforce GitHub Actions trust boundaries and keep key material out of Git."""

from __future__ import annotations

import re
import subprocess
import tomllib
from pathlib import Path

from github_api import PEM, TLS_NAME, ControlPlaneError, fail
from toolchains import check as check_toolchain_literals, pin

ROOT = Path(__file__).resolve().parents[2]
USES = re.compile(r"(?m)^\s*(?:-\s*)?uses:\s*(\S+)")
WRITE = re.compile(r"(?<![\w-])(?!permission-)([a-z-]+):\s*write\b")
STEP = re.compile(r"(?m)^(?=\s*- )")
JOB = re.compile(r"(?m)^  (?=[a-z-]+:$)")
RELEASE_SECRETS = {"GHCR_TOKEN", "RELEASE_APP_PRIVATE_KEY"}
# How a build gets unreviewed development notices: the collector's flag or a local Rust build.
DEVELOPMENT_BUILDS = ("--development", "scripts.rust_build")

TRIGGERS = {
    "ci.yml": {"pull_request", "push"},
    "release-request.yml": {"workflow_dispatch"},
    "release.yml": {"workflow_run"},
}
ALLOWED_USES = {
    "release-request.yml": {
        "actions/checkout", "jdx/mise-action", "./.github/actions/setup-project",
        "docker/setup-qemu-action", "docker/setup-buildx-action", "docker/build-push-action",
        "actions/upload-artifact",
    },
    "release.yml": {
        "actions/checkout", "jdx/mise-action", "actions/download-artifact",
        "actions/upload-artifact", "actions/create-github-app-token",
    },
}
ORDERED = {
    "actions/setup-project/action.yml": (
        "rustup toolchain install", "python3 scripts/ci/toolchains.py verify-rust",
    ),
    "workflows/release-request.yml": (
        "if: ${{ github.ref == format('refs/heads/{0}', github.event.repository.default_branch) }}",
        "run: python3 scripts/ci/release.py prepare",
        'python3 scripts/ci/verify_release_assets.py "$VERSION"',
        "SOURCE_SHA: ${{ steps.request.outputs.remote_sha }}\n",
        '[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]', "uses: docker/build-push-action@",
        "no-cache: true", "provenance: mode=max", "github-token: ''",
        "GM_CLIENT_REVISION=${{ steps.request.outputs.sha }}\n",
    ),
    "workflows/release.yml": (
        "github.event.workflow_run.conclusion == 'success'\n",
        "&& github.event.workflow_run.event == 'workflow_dispatch'\n",
        "&& github.event.workflow_run.head_branch == 'main'\n",
        "&& github.event.workflow_run.path == '.github/workflows/release-request.yml'\n",
        "run: python3 scripts/ci/release.py verify",
        "group: release-publish-${{ github.repository }}\n", "cancel-in-progress: false\n",
        "run: python3 scripts/ci/release.py recheck", "run: scripts/ci/publish.sh image",
        "run: python3 scripts/ci/release.py publish", "run: scripts/ci/publish.sh aliases",
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
    "run: python3 scripts/ci/release.py publish": {
        "REPOSITORY": "github.repository", "TARGET_SHA": "github.sha",
    },
}
FORBIDDEN = {
    "workflows/release.yml": ("head_sha", "pull_request.head", "mise run", "secrets["),
    "workflows/release-request.yml": (
        "allow-insecure-entitlement", "cache-from:", "cache-to:", "GIT_AUTH_TOKEN",
    ),
}


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
        installs = re.findall(r"rustup\W+toolchain\W+install\b.*", text)
        if any("--no-self-update" not in line for line in installs):
            fail(f"{name}: rustup must install toolchains with --no-self-update")
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
        if "setup-project" in step and "cache: 'false'" not in step:
            fail("release-request.yml: the untrusted build must disable every cache")
    for step in STEP.split(read(root, ".github/actions/setup-project/action.yml")):
        if ("uses: actions/cache@" in step and "inputs.cache == 'true'" not in step
                or "uses: jdx/mise-action@" in step and "cache: ${{ inputs.cache }}" not in step):
            fail("setup-project: every cache must follow the cache input")
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
    if "run: python3 -m scripts.legal.check_git_sources --verify\n" not in ci:
        fail("CI must verify the Cargo fork pins with check_git_sources --verify")
    jobs = set(re.findall(r"(?m)^  ([a-z-]+):$", ci.split("\njobs:\n", 1)[1])) - {"gate"}
    gate = re.search(r"(?ms)^  gate:\n.*?^    needs: \[([^]]*)\]", ci)
    if missing := sorted(jobs - {name.strip() for name in (gate.group(1) if gate else "").split(",")}):
        fail(f"CI Gate must need every job: {missing}")


def check_development_notices(root: Path) -> None:
    """Nothing CI or a release runs builds with unreviewed development notices: no workflow, image build, mise
    task or scripts/*.sh they reach starts the collector's --development mode or a local Rust build."""
    tasks = tomllib.loads(read(root, "mise.toml"))["tasks"]
    texts = [path.read_text(encoding="utf-8")
             for path in sorted([*(root / ".github").rglob("*.y*ml"), *(root / "container").glob("Dockerfile*")])]
    reached: set[str] = set()
    while texts:
        text = texts.pop()
        if any(marker in text for marker in DEVELOPMENT_BUILDS):
            fail("CI and releases must not build with unreviewed development notices")
        for task, script in re.findall(r"mise run ([\w-]+)|(?<![\w/.-])(scripts/[\w/.-]+\.sh)\b", text):
            if (name := task or script) in reached:
                continue
            reached.add(name)
            if script:
                texts.append(read(root, script))
                continue
            # A task runs its steps and, before and after them, the tasks it depends on.
            for key in ("run", "depends", "depends_post"):
                steps = tasks.get(name, {}).get(key, [])
                for step in steps if isinstance(steps, list) else [steps]:
                    texts.append(step if key == "run" and isinstance(step, str)
                                 else f"mise run {step if isinstance(step, str) else step['task']}")


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


def check_dockerfile(name: str, dockerfile: str) -> None:
    """Digest-pinned base images, BuildKit's own frontend, and exact apt packages from one Debian snapshot."""
    stages = re.findall(r"(?im)^FROM\s.*\sAS\s+(\S+)\s*$", dockerfile)

    def stage(image: str) -> bool:
        # A build argument such as rust-${TARGETARCH} may select an earlier stage; a bare ${BASE} names any image.
        literals = re.split(r"\$\{[^}]*\}", image)
        pattern = ".+".join(map(re.escape, literals))
        return "".join(literals) != "" and any(re.fullmatch(pattern, known) for known in stages)

    if unpinned := [image for image in re.findall(r"(?m)^FROM(?:\s+--\S+)*\s+(\S+)", dockerfile)
                    if image != "scratch" and "@sha256:" not in image and not stage(image)]:
        fail(f"{name} base images must be digest-pinned: {unpinned}")
    if re.search(r"(?im)^\s*#\s*syntax\s*=", dockerfile):
        fail(f"{name} must not select a BuildKit frontend with # syntax=")
    # Debian serves one version per package; exact versions install only from a fixed snapshot.
    snapshot = re.search(r"https://snapshot\.debian\.org/archive/%s/\d{8}T\d{6}Z\\n", dockerfile)
    if "apt-get update" in dockerfile and (not snapshot or dockerfile.index("apt-get update") < snapshot.start()):
        fail(f"{name} must point apt at one snapshot.debian.org timestamp before apt-get update")
    if unpinned := [package for packages in re.findall(r"apt-get install ([^&]*)", dockerfile)
                    for package in packages.split() if package[0] not in "-\\" and "=" not in package]:
        fail(f"{name} must install exact apt package versions: {unpinned}")


def check_repository(root: Path = ROOT) -> None:
    try:
        check_toolchain_literals(root)
    except (ValueError, OSError) as exc:
        fail(str(exc))
    for name in ("container/Dockerfile", "container/Dockerfile.rust"):
        check_dockerfile(name, read(root, name))
    check_actions(root)
    check_workflows(root)
    check_ci(root)
    check_development_notices(root)
    check_certificates(root)


def main() -> None:
    try:
        check_repository()
    except ControlPlaneError as exc:
        raise SystemExit(f"workflow policy: {exc}") from exc
    print("workflow policy: ok")


if __name__ == "__main__":
    main()
