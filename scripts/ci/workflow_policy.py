#!/usr/bin/env python3
"""Enforce GitHub Actions trust boundaries and keep key material out of Git."""

from __future__ import annotations

import ast
import re
import subprocess
import tomllib
from pathlib import Path, PurePosixPath

from .github_api import PEM, TLS_NAME, ControlPlaneError, fail
from .toolchains import check as check_toolchain_literals, pin

ROOT = Path(__file__).resolve().parents[2]
USES = re.compile(r"(?m)^\s*(?:-\s*)?uses:\s*(\S+)")
WRITE = re.compile(r"(?<![\w-])(?!permission-)([a-z-]+):\s*write\b")
STEP = re.compile(r"(?m)^(?=\s*- )")
JOB = re.compile(r"(?m)^  (?=[a-z-]+:$)")
RELEASE_SECRETS = {"GHCR_TOKEN", "RELEASE_APP_PRIVATE_KEY"}
# The macOS TUIs are packaged by these scripts and every module they import or run with -m.
DARWIN_SCRIPTS = ("scripts/package_rust.py",)

TRIGGERS = {
    "advisories.yml": {"schedule", "workflow_dispatch"},
    "ci.yml": {"pull_request", "push"},
    "fork-upkeep.yml": {"schedule", "workflow_dispatch"},
    "release-request.yml": {"workflow_dispatch"},
    "release.yml": {"workflow_run"},
}
ALLOWED_USES = {
    "fork-upkeep.yml": {"actions/checkout", "jdx/mise-action", "actions/create-github-app-token"},
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
    "workflows/release-request.yml": (
        "if: ${{ github.ref == format('refs/heads/{0}', github.event.repository.default_branch) }}",
        "run: python3 -m scripts.ci.release prepare",
        "VERSION= mise run legal-check\n",
        "SOURCE_SHA: ${{ steps.request.outputs.remote_sha }}\n",
        '[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]', "uses: docker/build-push-action@",
        'python3 -m scripts.ci.verify_release_assets "$VERSION"',
        # Only the expected Rust artifacts leave the exports, with a listing that cannot list itself;
        # the macOS job packages with the task CI runs.
        "run: python3 -m scripts.ci.release stage-rust\n", 'run: mise run rust-darwin-package "$VERSION"\n',
    ),
    # CI builds, stages and verifies the Rust exports as a release request and the release do, and
    # packages the macOS TUIs with the release request's task.
    "workflows/ci.yml": (
        "run: mise run rust-check\n", "run: python3 -m scripts.legal.check_git_sources --verify\n",
        "run: mise run rust-check-targets\n", "run: mise run rust-delayed-downloads\n",
        "--target tui-artifacts", "--target server-artifacts",
        "python3 -m scripts.ci.release stage-rust\n", "python3 -m scripts.ci.release check-rust\n",
        "run: mise run rust-darwin-package 0.0.0-dev\n",
        "cargo test --locked -p graphite-meter-client -p graphite-meter-core -p graphite-meter-net\n",
    ),
    "workflows/release.yml": (
        "github.event.workflow_run.conclusion == 'success'\n",
        "&& github.event.workflow_run.event == 'workflow_dispatch'\n",
        "&& github.event.workflow_run.head_branch == 'main'\n",
        "&& github.event.workflow_run.path == '.github/workflows/release-request.yml'\n",
        "run: python3 -m scripts.ci.release verify",
        "group: release-publish-${{ github.repository }}\n", "cancel-in-progress: false\n",
        "run: python3 -m scripts.ci.release recheck", "run: scripts/ci/publish.sh image",
        "run: python3 -m scripts.ci.release publish", "run: scripts/ci/publish.sh aliases",
    ),
}
# Identity that release.py trusts comes from the run context, never from dispatch inputs.
CONTEXT = {
    "run: python3 -m scripts.ci.release prepare": {
        "REPOSITORY": "github.repository", "REPOSITORY_OWNER": "github.repository_owner",
        "ACTOR": "github.actor", "TRIGGERING_ACTOR": "github.triggering_actor",
        "EVENT_NAME": "github.event_name", "EVENT_SHA": "github.sha", "REF": "github.ref",
        "WORKFLOW_REF": "github.workflow_ref", "REQUEST_RUN_ID": "github.run_id",
        "REQUEST_RUN_ATTEMPT": "github.run_attempt",
    },
    "run: python3 -m scripts.ci.release verify": {
        "REPOSITORY": "github.repository", "REPOSITORY_OWNER": "github.repository_owner",
        "PUBLISHER_SHA": "github.sha", "WORKFLOW_REF": "github.workflow_ref",
        "REQUEST_RUN_ID": "github.event.workflow_run.id",
    },
    "run: python3 -m scripts.ci.release recheck": {
        "REPOSITORY": "github.repository", "TAG": "needs.verify.outputs.tag",
        "SOURCE_SHA": "needs.verify.outputs.sha", "MAIN_SHA": "needs.verify.outputs.main_sha",
        "PR": "needs.verify.outputs.pr", "RUST": "needs.verify.outputs.rust",
        "OCI_SHA256": "needs.verify.outputs.oci_sha256",
        "RUST_OCI_SHA256": "needs.verify.outputs.rust_oci_sha256",
        "ASSETS_SHA256": "needs.verify.outputs.assets_sha256",
    },
    "run: python3 -m scripts.ci.release publish": {
        "REPOSITORY": "github.repository", "TARGET_SHA": "github.sha",
        "TAG": "needs.verify.outputs.tag",
        "SOURCE_SHA": "needs.verify.outputs.sha", "PR": "needs.verify.outputs.pr",
        "RUST": "needs.verify.outputs.rust",
    },
}
CHECKOUT_REFS = {"workflows/release-request.yml": ("github.sha", "needs.build.outputs.sha")}
IMAGE_BUILD = (
    "no-cache: true\n", "provenance: mode=max\n", "github-token: ''\n",
    "GM_CLIENT_REVISION=${{ steps.request.outputs.sha }}\n",
)
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
        needles = ["ubuntu-latest", "secrets["]
        if name not in ("workflows/release.yml", "workflows/fork-upkeep.yml"):
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
                refs = {f"${{{{ {ref} }}}}" for ref in CHECKOUT_REFS.get(name, ("github.sha",))}
                if set(re.findall(r"(?m)\bref: (.*)$", step)) - refs:
                    fail(f"{name}: checkout may only select the triggering github.sha or the commit release.py validated")
            if "uses: jdx/mise-action@" in step:
                required = [mise, "install_args: --locked ", "cache:", "MISE_AUTO_INSTALL: '0'"]
                if name.startswith("workflows/"):
                    required += ["install_args: --locked python\n", "cache: false"]
                if missing := [item for item in required if item not in step]:
                    fail(f"{name}: mise setup must declare {missing[0].strip()}")
            if "uses: docker/build-push-action@" in step:
                if missing := [item for item in IMAGE_BUILD if item not in step]:
                    fail(f"{name}: every image build must declare {missing[0].strip()}")
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
    upkeep = (workflows / "fork-upkeep.yml").read_text(encoding="utf-8")
    if set(re.findall(r"secrets\.(\w+)", upkeep)) != {"FORK_UPKEEP_APP_PRIVATE_KEY", "FORK_PIN_APP_PRIVATE_KEY"}:
        fail("fork-upkeep.yml: only the fork upkeep App and pin App private keys are allowed")
    pin = next(step for step in STEP.split(upkeep) if "secrets.FORK_PIN_APP_PRIVATE_KEY" in step)
    if "repositories: ${{ github.event.repository.name }}\n" not in pin or "permission-workflows" in pin:
        fail("fork-upkeep.yml: the pin App token may write only this repository, never its workflows")
    if "github.ref == format('refs/heads/{0}', github.event.repository.default_branch)" not in upkeep:
        fail("fork-upkeep.yml: upkeep may run only from the default branch")
    if "repositories: ${{ steps.inventory.outputs.repositories }}" not in upkeep:
        fail("fork-upkeep.yml: App repositories must come from the fork inventory")
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
    # Dispatch runs in the default branch's cache scope, where built PR code could plant caches.
    if re.findall(r"(?m)^ *cache-mode:.*", request) != ["cache-mode: none"]:
        fail("release-request.yml: the untrusted build must get no cache token")
    for step in STEP.split(request.split("\njobs:", 1)[1]):
        if "${{ inputs." in step and "run: python3 -m scripts.ci.release prepare" not in step:
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
    jobs = set(re.findall(r"(?m)^  ([a-z-]+):$", ci.split("\njobs:\n", 1)[1])) - {"gate"}
    gate = re.search(r"(?ms)^  gate:\n.*?^    needs: \[([^]]*)\]", ci)
    if missing := sorted(jobs - {name.strip() for name in (gate.group(1) if gate else "").split(",")}):
        fail(f"CI Gate must need every job: {missing}")


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


def script_modules(root: Path, scripts: tuple[str, ...]) -> set[str]:
    """The repository files `scripts` load: modules they import or run with -m, and their packages."""
    found: set[str] = set()
    pending = [root / script for script in scripts]
    while pending:
        path = pending.pop()
        if (relative := path.relative_to(root).as_posix()) in found:
            continue
        found.add(relative)
        package = list(path.relative_to(root).parent.parts)
        modules: list[list[str]] = []
        for node in ast.walk(ast.parse(path.read_text(encoding="utf-8"))):
            if isinstance(node, ast.ImportFrom):
                base = package[:len(package) + 1 - node.level] if node.level else []
                base = base + (node.module.split(".") if node.module else [])
                modules += [base, *(base + [alias.name] for alias in node.names)]
            elif isinstance(node, ast.Import):
                modules += [alias.name.split(".") for alias in node.names]
            elif isinstance(node, ast.Constant) and isinstance(node.value, str) and re.fullmatch(
                    r"scripts(?:\.\w+)+", node.value):
                modules.append(node.value.split("."))
        for parts in modules:
            for depth in range(1, len(parts) + 1):
                prefix = root.joinpath(*parts[:depth])
                pending += [file for file in (prefix / "__init__.py", prefix.with_suffix(".py"))
                            if file.is_file() and (depth == len(parts) or file.name == "__init__.py")]
    return found


def check_paths(root: Path) -> None:
    """PRs that change an input of the Rust image or of the macOS TUIs select the jobs that build them."""
    filters = path_filters(read(root, ".github/ci-paths.yml"))
    image = [source + "x" if source.endswith("/") else source for line in re.findall(
        r"(?m)^COPY (?!--)(.+)$", read(root, "container/Dockerfile.rust")) for source in line.split()[:-1]]
    for name, inputs in (("rust", [".dockerignore", *image]), ("darwin", sorted(script_modules(root, DARWIN_SCRIPTS)))):
        if missing := [path for path in inputs
                       if not any(PurePosixPath(path).full_match(glob) for glob in filters.get(name, []))]:
            fail(f".github/ci-paths.yml {name} misses {missing}")


def check_build_context(root: Path) -> None:
    """Local Rust build output, which the .gitignore files under rust/ name, stays out of the image build context."""
    excluded, missing = set(read(root, ".dockerignore").splitlines()), []
    for directory, subdirectories, files in (root / "rust").walk():
        ignored = [line.strip("/") for line in (directory / ".gitignore").read_text().splitlines()
                   if line.strip() and not line.startswith("#")] if ".gitignore" in files else []
        subdirectories[:] = [name for name in subdirectories if name not in ignored]
        missing += [path for name in ignored if (path := (directory / name).relative_to(root).as_posix()) not in excluded]
    if missing:
        fail(f".dockerignore misses Rust build output {missing}")


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
    for name in ("container/Dockerfile", "container/Dockerfile.rust"):
        dockerfile = read(root, name)
        stages = re.findall(r"(?im)^FROM\s.*\sAS\s+(\S+)\s*$", dockerfile)

        def stage(image: str) -> bool:
            # A build argument such as rust-${TARGETARCH} may select an earlier stage; a bare
            # ${BASE} could name any image.
            literals = re.split(r"\$\{[^}]*\}", image)
            pattern = ".+".join(map(re.escape, literals))
            return "".join(literals) != "" and any(re.fullmatch(pattern, name) for name in stages)

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
    check_actions(root)
    check_workflows(root)
    check_ci(root)
    check_paths(root)
    check_build_context(root)
    check_certificates(root)


def main() -> None:
    try:
        check_repository()
    except ControlPlaneError as exc:
        raise SystemExit(f"workflow policy: {exc}") from exc
    print("workflow policy: ok")


if __name__ == "__main__":
    main()
