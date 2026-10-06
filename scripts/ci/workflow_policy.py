#!/usr/bin/env python3
"""Enforce GitHub Actions trust boundaries and keep key material out of Git."""

from __future__ import annotations

import os
import re
import subprocess
import tomllib
from pathlib import Path, PurePosixPath

from github_api import PEM, TLS_NAME, ControlPlaneError, fail
from release import REQUEST_JOBS, RUST_TUI_JOB
from toolchains import check as check_toolchain_literals, pin

ROOT = Path(__file__).resolve().parents[2]
USES = re.compile(r"(?m)^\s*(?:-\s*)?uses:\s*(\S+)")
WRITE = re.compile(r"(?<![\w-])(?!permission-)([a-z-]+):\s*write\b")
STEP = re.compile(r"(?m)^(?=\s*- )")
JOB = re.compile(r"(?m)^  (?=[a-z0-9-]+:$)")
RELEASE_SECRETS = {"GHCR_TOKEN", "RELEASE_APP_PRIVATE_KEY"}
# How a build gets unreviewed development notices: the collector's flag or a local Rust build.
DEVELOPMENT_BUILDS = ("--development", "scripts.rust_build")
# The collector, which defines --development, and this policy name the markers without building.
NAMING = ("scripts/legal/rust.py", "scripts/ci/workflow_policy.py")

TRIGGERS = {
    "advisories.yml": {"schedule", "workflow_dispatch"},
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
    "workflows/advisories.yml": ("run: cargo deny --locked check advisories\n",),
    "actions/setup-project/action.yml": (
        "rustup toolchain install", "python3 scripts/ci/toolchains.py verify-rust",
    ),
    "workflows/release-request.yml": (
        "if: ${{ github.ref == format('refs/heads/{0}', github.event.repository.default_branch) }}",
        "\n      context: ${{ steps.source.outputs.context }}\n", "run: python3 scripts/ci/release.py prepare",
        'python3 scripts/ci/verify_release_assets.py "$VERSION"',
        "SOURCE_SHA: ${{ steps.request.outputs.remote_sha }}\n",
        '[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]', "uses: docker/build-push-action@",
        "GM_CLIENT_REVISION=${{ steps.request.outputs.sha }}\n",
        "run: python3 scripts/ci/rust_release.py stage-image\n",
        "python3 scripts/ci/rust_release.py stage-tui\n",
        'python3 -m scripts.package_rust "$VERSION" --output "$OUT_DIR" --check\n',
    ),
    # CI exports the image's source offers, builds the TUI archives with the release request's BuildKit, then stages
    # and verifies its Rust builds as releases do.
    "workflows/ci.yml": (
        "--target server-artifacts", "BUILDX_BUILDER: ${{ steps.buildx.outputs.name }}\n",
        'run: mise run rust-client-package "$VERSION"\n', "python3 scripts/ci/rust_release.py stage-image\n",
        "python3 scripts/ci/rust_release.py stage-tui\n", "python3 scripts/ci/rust_release.py check\n",
    ),
    "workflows/release.yml": (
        "github.event.workflow_run.conclusion == 'success'\n",
        "&& github.event.workflow_run.event == 'workflow_dispatch'\n",
        "&& github.event.workflow_run.head_branch == 'main'\n",
        "&& github.event.workflow_run.path == '.github/workflows/release-request.yml'\n",
        "run: python3 scripts/ci/release.py verify",
        "group: release-publish-${{ github.repository }}\n", "cancel-in-progress: false\n",
        "run: python3 scripts/ci/release.py recheck", "run: scripts/ci/publish.sh image",
        "IMAGE_TAG: ${{ needs.verify.outputs.version }}-rust\n",
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
    # What the approved job rechecks and publishes comes only from the verify job.
    "run: python3 scripts/ci/release.py recheck": {
        "TAG": "needs.verify.outputs.tag", "SOURCE_SHA": "needs.verify.outputs.sha",
        "MAIN_SHA": "needs.verify.outputs.main_sha", "PR": "needs.verify.outputs.pr",
        "OCI_SHA256": "needs.verify.outputs.oci_sha256", "ASSETS_SHA256": "needs.verify.outputs.assets_sha256",
        "RUST": "needs.verify.outputs.rust", "RUST_OCI_SHA256": "needs.verify.outputs.rust_oci_sha256",
    },
    "ARCHIVE_DIR: ${{ runner.temp }}/handoff/rust-image\n": {"DIGEST": "needs.verify.outputs.rust_digest"},
    "run: scripts/ci/publish.sh aliases": {
        "VERSION": "needs.verify.outputs.version", "DIGEST": "needs.verify.outputs.digest",
        "RUST_DIGEST": "needs.verify.outputs.rust_digest",
    },
}
# Every image build of the untrusted request records max provenance and gets no token.
IMAGE_BUILD = ("provenance: mode=max\n", "github-token: ''\n")
# Each starts empty but an export of the server's source offers, which reuses the image build before it.
OFFERS_EXPORT = "target: server-artifacts\n"
# Every image build of the untrusted request builds the source its build job validated and resolved.
IMAGE_CONTEXTS = ("${{ steps.source.outputs.context }}", "${{ needs.build.outputs.context }}")
# The plan output that selects each CI job; the jobs ALWAYS names run on every change.
SELECTED_BY = {
    "core": "code", "go": "go", "e2e": "code", "smoke": "code", "release": "code", "security": "deps",
    "rust": "rust", "rust-windows": "rust", "rust-interop": "rust-interop",
    "rust-image": "rust-image", "rust-e2e": "rust-image", "rust-tui": "rust-release", "rust-release": "rust-release",
}
ALWAYS = ("plan", "tooling", "secret-scan", "gate")
# Repository paths a Rust source includes: relative to the file, or to its crate's manifest directory.
RUST_INCLUDE = re.compile(r'include_(?:str|bytes)!\("([^"]+)"\)|concat!\(env!\("CARGO_MANIFEST_DIR"\), "([^"]+)"\)')
# Dependency manifests and lockfiles, toolchain pins, Cargo configuration and build scripts.
SUPPLY_CHAIN = re.compile(r"(?:^|/)(?:Cargo\.(?:toml|lock)|deny\.toml|build\.rs|rust-toolchain(?:\.toml)?"
                          r"|go\.(?:mod|sum)|package\.json|bun\.lock|bunfig\.toml|mise\.(?:toml|lock))$"
                          r"|(?:^|/)\.cargo/")
# The Dependabot ecosystem that updates each lockfile's directory.
ECOSYSTEMS = {"Cargo.lock": "cargo", "go.sum": "gomod", "bun.lock": "bun"}
FORBIDDEN = {
    "workflows/release.yml": ("head_sha", "pull_request.head", "mise run", "secrets["),
    "workflows/release-request.yml": (
        "allow-insecure-entitlement", "cache-from:", "cache-to:", "GIT_AUTH_TOKEN",
    ),
}


def read(root: Path, name: str) -> str:
    return (root / name).read_text(encoding="utf-8")


def files(root: Path) -> list[str]:
    """The tracked files, or every file outside a Git checkout."""
    listed = subprocess.run(["git", "ls-files", "-z"], cwd=root, capture_output=True, check=False)
    if listed.returncode == 0:
        return [entry.decode() for entry in listed.stdout.split(b"\0") if entry]
    return [path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()]


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
                env = re.findall(r"(?m)^ +([A-Z][A-Z0-9_]*): (.*)$", step) if marker in step else []
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
    # release.py takes each artifact only from the request job that wrote it.
    if re.findall(r"(?m)^    name: (.*)$", request) != list(REQUEST_JOBS):
        fail(f"release-request.yml: its jobs must be named {list(REQUEST_JOBS)}, as release.py expects")
    # The Rust jobs run only for a validated request that selected them, the TUI archives only for a stable one and
    # after the image, so no two jobs run at once and each artifact binds to the one job that ran when it was written.
    for job in JOB.split(request.split("\njobs:\n", 1)[1]):
        tui = f"    name: {RUST_TUI_JOB}\n" in job
        needs = "[build, rust-image]" if tui else "build"
        stable = " && needs.build.outputs.stable == 'true'" if tui else ""
        selected = f"    needs: {needs}\n    if: needs.build.outputs.rust == 'true'{stable}\n"
        if job and not job.startswith("build:\n") and selected not in job:
            fail("release-request.yml: Rust jobs must run one after another, only when the validated request "
                 "selects them, and the TUI archives only for a stable release")
    scopes = re.findall(r"(?m)^ *permissions:.*(?:\n +\S.*)*", request)
    if scopes != ["permissions:\n  contents: read"]:
        fail("release-request.yml: the untrusted build may only read contents")
    for step in STEP.split(request.split("\njobs:", 1)[1]):
        if "${{ inputs." in step and "run: python3 scripts/ci/release.py prepare" not in step:
            fail("release-request.yml: dispatch inputs may reach only the request validator")
        if "uses: docker/build-push-action@" not in step:
            continue
        if missing := [item for item in IMAGE_BUILD if item not in step]:
            fail(f"release-request.yml: every image build must declare {missing[0].strip()}")
        contexts = re.findall(r"(?m)^ +context: (.*)$", step)
        if len(contexts) != 1 or contexts[0] not in IMAGE_CONTEXTS:
            fail("release-request.yml: every image build must build the source the build job resolved")
    for job in JOB.split(request.split("\njobs:\n", 1)[1]):
        builds = [step for step in STEP.split(job) if "uses: docker/build-push-action@" in step]
        for previous, step in zip(["", *builds], builds):
            # The job's own builder then holds only the image build's layers, so both outputs come from one build.
            reuses = (OFFERS_EXPORT in step and "target: server\n" in previous and "no-cache: true\n" in previous
                      and job.count("uses: docker/setup-buildx-action@") == 1
                      and build_inputs(step) == build_inputs(previous))
            if "no-cache: true\n" not in step and not reuses:
                fail("release-request.yml: every image build must declare no-cache, but the source offers' export "
                     "right after its image build with the same inputs")
    # Caches are keyed by job; only CI writes them.
    for name in sorted(names - {"ci.yml"}):
        for step in STEP.split((workflows / name).read_text(encoding="utf-8")):
            if "setup-project" in step and "cache: 'false'" not in step:
                fail(f"{name} must disable every cache")
    for step in STEP.split(read(root, ".github/actions/setup-project/action.yml")):
        if ("uses: actions/cache@" in step and "inputs.cache == 'true'" not in step
                or "uses: jdx/mise-action@" in step and "cache: ${{ inputs.cache }}" not in step):
            fail("setup-project: every cache must follow the cache input")
    if "VERSION= mise run legal-check\n" not in request:
        fail("release-request.yml: stable builds must check committed legal outputs first")


def build_inputs(step: str) -> tuple[list[str], str]:
    """The Dockerfile, context, platforms and build arguments of a build-push-action step."""
    keys = re.findall(r"(?m)^ +(?:file|context|platforms): .*$", step)
    arguments = re.search(r"(?m)^( +)build-args: \|\n((?:\1 +\S.*\n)*)", step)
    return keys, arguments[2] if arguments else ""


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
    jobs = set(re.findall(r"(?m)^  ([a-z0-9-]+):$", ci.split("\njobs:\n", 1)[1])) - {"gate"}
    gate = re.search(r"(?ms)^  gate:\n.*?^    needs: \[([^]]*)\]", ci)
    if missing := sorted(jobs - {name.strip() for name in (gate.group(1) if gate else "").split(",")}):
        fail(f"CI Gate must need every job: {missing}")


def check_development_notices(root: Path) -> None:
    """Nothing CI or a release runs builds with unreviewed development notices: no workflow, image build, mise
    task, scripts/*.sh or Python driver they reach starts the collector's --development mode or a local Rust
    build."""
    tasks = tomllib.loads(read(root, "mise.toml"))["tasks"]
    texts = [path.read_text(encoding="utf-8")
             for path in sorted([*(root / ".github").rglob("*.y*ml"), *(root / "container").glob("Dockerfile*")])]
    reached = set(NAMING)
    while texts:
        text = texts.pop()
        if any(marker in text for marker in DEVELOPMENT_BUILDS):
            fail("CI and releases must not build with unreviewed development notices")
        for task, script, module, driver in re.findall(r"mise run ([\w-]+)|(?<![\w/.-])(scripts/[\w/.-]+\.sh)\b"
                                                        r"|python3 (?:-m ([\w.]+)|(?:\.\./)*([\w/.-]+\.py))", text):
            if module:
                path = module.replace(".", "/")
                script = next((name for name in (f"{path}.py", f"{path}/__main__.py") if (root / name).is_file()), "")
            if (name := task or script or driver) in reached or not name:
                continue
            reached.add(name)
            if script or driver:
                if not (root / name).is_file():
                    fail(f"{name}, which CI or a release runs, is missing")
                texts.append(read(root, name))
                continue
            # A task runs its steps and, before and after them, the tasks it depends on.
            for key in ("run", "depends", "depends_post"):
                steps = tasks.get(name, {}).get(key, [])
                for step in steps if isinstance(steps, list) else [steps]:
                    texts.append(step if key == "run" and isinstance(step, str)
                                 else f"mise run {step if isinstance(step, str) else step['task']}")


def path_filters(text: str) -> dict[str, list[str]]:
    """The globs of each .github/ci-paths.yml filter, with its aliases expanded."""
    filters: dict[str, list[str]] = {}
    anchors: dict[str, list[str]] = {}
    current: list[str] = []
    for line in text.splitlines():
        if match := re.fullmatch(r"([\w-]+):(?: &([\w-]+))?", line):
            current = filters[match[1]] = []
            if match[2]:
                anchors[match[2]] = current
        elif match := re.fullmatch(r"  - '([^']+)'|  - \*([\w-]+)", line):
            current.extend([match[1]] if match[1] else anchors[match[2]])
    return filters


def check_paths(root: Path) -> None:
    """A change to any input of the Rust image selects the jobs that build it; one to any input of the TUI archives,
    which every stage but the browser app's provides, or of the browser's locked dependencies, whose sources the
    server's source offers carry, selects their export and staging; and one to a file a Rust source includes selects
    the Rust checks."""
    filters = path_filters(read(root, ".github/ci-paths.yml"))
    stages = [(match[1] if (match := re.match(r"FROM .* AS (\S+)", text)) else "", text)
              for text in re.split(r"(?m)^(?=FROM )", read(root, "container/Dockerfile.rust"))]
    # Each filter covers a stage's inputs up to the instruction named for it.
    for name, until in (("rust-image", {}), ("rust-release", {"browser": "RUN bun --bun install"})):
        # A copied directory stands for every file below it.
        inputs = [".dockerignore", "container/Dockerfile.rust", *(
            source + "x" if source.endswith("/") else source
            for stage, text in stages
            for line in re.findall(r"(?m)^COPY (?!--)(.+)$", text.split(until[stage])[0] if stage in until else text)
            for source in line.split()[:-1])]
        if missing := [path for path in inputs
                       if not any(PurePosixPath(path).full_match(glob) for glob in filters.get(name, []))]:
            fail(f".github/ci-paths.yml {name} misses {missing}")
    for name in files(root):
        if not name.startswith("rust/") or not name.endswith(".rs"):
            continue
        for relative, manifest in RUST_INCLUDE.findall(read(root, name)):
            base = PurePosixPath(name).parent if relative else PurePosixPath(*PurePosixPath(name).parts[:2])
            included = PurePosixPath(os.path.normpath(base / (relative or manifest.lstrip("/"))))
            if not any(included.full_match(glob) for glob in filters["rust"]):
                fail(f".github/ci-paths.yml rust misses {included}, which {name} includes")


def check_selection(root: Path) -> None:
    """Every push runs every job, and a pull request each job whose reviewed filter matches; a job's needs run
    whenever it does, since the Gate passes a job skipped for a skipped prerequisite."""
    ci = read(root, ".github/workflows/ci.yml")
    filters = path_filters(read(root, ".github/ci-paths.yml"))
    outputs = re.findall(r"(?m)^      ([\w-]+): \$\{\{ github\.event_name == 'push' \|\| "
                         r"steps\.filter\.outputs\.([\w-]+) == 'true' \}\}$", ci)
    if any(name != source or name not in filters for name, source in outputs):
        fail("CI's plan must output .github/ci-paths.yml filters by their names, and every one on push")
    selected: dict[str, str | None] = {}
    needs: dict[str, list[str]] = {}
    for job in filter(None, JOB.split(ci.split("\njobs:\n", 1)[1])):
        name = job.split(":", 1)[0]
        output = re.search(r"(?m)^    if: needs\.plan\.outputs\.([\w-]+) == 'true'$", job)
        selected[name] = output[1] if output else None
        listed = re.search(r"(?m)^    needs: \[?([^]\n]*)", job)
        needs[name] = [item.strip() for item in listed[1].split(",")] if listed else []
    expected = {**SELECTED_BY, **dict.fromkeys(ALWAYS)}
    if wrong := sorted(name for name in selected.keys() | expected.keys()
                       if selected.get(name, "") != expected.get(name, "")):
        fail(f"CI jobs must run on their reviewed filters: {wrong}")
    if unknown := sorted(name for name, output in selected.items() if output and output not in dict(outputs)):
        fail(f"CI jobs run on filters the plan does not output: {unknown}")
    for name, output in selected.items():
        for needed in needs[name]:
            # A glob lies within another that matches it as a path.
            if output and (other := selected.get(needed)) and not all(
                    any(PurePosixPath(glob).full_match(cover) for cover in filters[other]) for glob in filters[output]):
                fail(f"CI job {name} may run without {needed}: filter {output} is not within {other}")


def owned(path: str, patterns: list[str]) -> bool:
    """Whether a CODEOWNERS pattern, in gitignore syntax without negation or **, covers `path`."""
    parts = path.split("/")
    for pattern in patterns:
        starts = range(1) if "/" in pattern.rstrip("/") else range(len(parts))
        for start in starts:
            for end in range(start + 1, len(parts) + 1):
                if ((end < len(parts) or not pattern.endswith("/"))
                        and PurePosixPath(*parts[start:end]).full_match(pattern.strip("/"))):
                    return True
    return False


def check_dependencies(root: Path) -> None:
    """CODEOWNERS covers every supply-chain file, and Dependabot updates every lockfile's ecosystem."""
    patterns = [line.split()[0] for line in read(root, ".github/CODEOWNERS").splitlines()
                if line.strip() and not line.startswith("#")]
    names = files(root)
    if unowned := sorted(name for name in names if SUPPLY_CHAIN.search(name) and not owned(name, patterns)):
        fail(f".github/CODEOWNERS leaves supply-chain files without an owner: {unowned}")
    updates = set(re.findall(r"(?m)^  - package-ecosystem: (\S+)\n    directory: (\S+)$",
                             read(root, ".github/dependabot.yml")))
    for name in names:
        path = PurePosixPath(name)
        if (ecosystem := ECOSYSTEMS.get(path.name)) and (ecosystem, f"/{path.parent}") not in updates:
            fail(f".github/dependabot.yml must update {ecosystem} in /{path.parent}")


def check_certificates(root: Path) -> None:
    names = files(root)
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
    check_paths(root)
    check_selection(root)
    check_dependencies(root)
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
