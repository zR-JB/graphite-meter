#!/usr/bin/env python3
"""Enforce GitHub Actions trust boundaries and keep key material out of Git."""

import re
import subprocess
import tomllib
from pathlib import Path, PurePosixPath

from github_api import PEM, TLS_NAME, ControlPlaneError, fail
from release import request_jobs
from toolchains import check as check_toolchain_literals, pin

ROOT = Path(__file__).resolve().parents[2]
USES = re.compile(r"(?m)^\s*(?:-\s*)?uses:\s*(\S+)")
WRITE = re.compile(r"(?<![\w-])(?!permission-)([a-z-]+):\s*write\b")
STEP = re.compile(r"(?m)^(?=\s*- )")
JOB = re.compile(r"(?m)^  (?=[a-z0-9-]+:$)")
RELEASE_SECRETS = {"GHCR_TOKEN", "RELEASE_APP_PRIVATE_KEY"}
# How a build gets unreviewed development notices: the collector's flag or a local build.
DEVELOPMENT_BUILDS = ("--development", "scripts.build")
# The collector, which defines --development, and this policy name the markers without building.
NAMING = ("scripts/legal/rust.py", "scripts/ci/workflow_policy.py")

TRIGGERS = {
    "advisories.yml": {"schedule", "workflow_dispatch"},
    "ci.yml": {"merge_group", "pull_request", "push", "schedule"},
    "release-request.yml": {"workflow_dispatch"},
    "release.yml": {"workflow_run"},
}
ALLOWED_USES = {
    "release-request.yml": {
        "actions/checkout", "./.github/actions/setup-project", "./.github/actions/setup-buildx",
        "actions/upload-artifact", "actions/download-artifact",
    },
    "release.yml": {
        "actions/checkout", "jdx/mise-action", "actions/download-artifact",
        "actions/upload-artifact", "actions/create-github-app-token",
    },
}
# Steps each file must contain, in this order: deleting or reordering one weakens a trust boundary.
ORDERED = {
    "workflows/release-request.yml": (
        "if: github.ref != format('refs/heads/{0}', github.event.repository.default_branch)\n"
        "        run: |\n          echo \"::error::Release requests run from main; pick main in 'Use workflow from'\"\n"
        "          exit 1\n",
        "run: python3 scripts/ci/release.py prepare", '[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]',
        "run: scripts/ci/build-image.sh\n", "run: python3 scripts/ci/assets.py stage\n",
    ),
    # The trusted consumer acts only on a successful main dispatch of the request workflow, one publish at a time.
    "workflows/release.yml": (
        "github.event.workflow_run.conclusion == 'success'\n",
        "&& github.event.workflow_run.event == 'workflow_dispatch'\n",
        "&& github.event.workflow_run.head_branch == 'main'\n",
        "&& github.event.workflow_run.path == '.github/workflows/release-request.yml'\n",
        "run: python3 scripts/ci/release.py verify", "group: release-publish-${{ github.repository }}\n",
        "cancel-in-progress: false\n", "run: python3 scripts/ci/release.py recheck",
        "run: scripts/ci/publish.sh image", "run: python3 scripts/ci/release.py publish",
        "run: scripts/ci/publish.sh aliases",
    ),
    "actions/setup-project/action.yml": ("rustup toolchain install", "python3 scripts/ci/toolchains.py verify-rust"),
    "workflows/advisories.yml": (
        "run: mise run rust-deny-fetch\n", "run: cargo deny --locked --offline check advisories\n",
    ),
}
# Identity that release.py trusts comes from the run context, never from dispatch inputs.
CONTEXT = {
    "run: python3 scripts/ci/release.py prepare": {
        "GH_TOKEN": "github.token", "REPOSITORY": "github.repository", "REPOSITORY_OWNER": "github.repository_owner",
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
        "REPOSITORY": "github.repository", "TARGET_SHA": "github.sha", "DIGEST": "needs.verify.outputs.digest",
    },
    # What the approved job rechecks and publishes comes only from the verify job.
    "run: python3 scripts/ci/release.py recheck": {
        "TAG": "needs.verify.outputs.tag", "SOURCE_SHA": "needs.verify.outputs.sha",
        "MAIN_SHA": "needs.verify.outputs.main_sha", "PR": "needs.verify.outputs.pr",
        "OCI_SHA256": "needs.verify.outputs.oci_sha256", "ASSETS_SHA256": "needs.verify.outputs.assets_sha256",
    },
    "run: scripts/ci/publish.sh image": {
        "IMAGE_TAG": "needs.verify.outputs.version", "DIGEST": "needs.verify.outputs.digest",
    },
    "run: scripts/ci/publish.sh aliases": {
        "VERSION": "needs.verify.outputs.version", "DIGEST": "needs.verify.outputs.digest",
    },
}
IMAGE_SCRIPT = "scripts/ci/build-image.sh"
PREPARE = "run: python3 scripts/ci/release.py prepare"
EXPRESSION = re.compile(r"\$\{\{(.*?)\}\}", re.S)
# Dispatch inputs, directly or through the event or context that carries them.
INPUTS = re.compile(r"\binputs\b|\bgithub\.event\b(?!\.repository\.)|\bgithub\b(?![.\w])")
STATUS = re.compile(r"\b(?:always|cancelled|failure|success)\s*\(")
DETACHED = re.compile(r"(?<![&>|<])&(?![&>])|\bln\s|\b(?:nohup|setsid|disown)\b")
UPLOAD = re.compile(r"python3 scripts/ci/assets\.py upload (?:request|image|client|bundle)")
# Dependency manifests and lockfiles, toolchain pins, Cargo configuration and build scripts.
SUPPLY_CHAIN = re.compile(r"(?:^|/)(?:Cargo\.(?:toml|lock)|deny\.toml|build\.rs|rust-toolchain(?:\.toml)?"
                          r"|package\.json|bun\.lock|bunfig\.toml|mise\.(?:toml|lock))$"
                          r"|(?:^|/)\.cargo/")
# The Dependabot ecosystem that updates each lockfile's directory.
ECOSYSTEMS = {"Cargo.lock": "cargo", "bun.lock": "bun"}
FORBIDDEN = {
    "workflows/release.yml": ("head_sha", "pull_request.head", "mise run", "secrets["),
    "workflows/release-request.yml": (
        "allow-insecure-entitlement", "cache-from", "cache-to", "--build-context", "build-contexts", "GIT_AUTH_TOKEN",
        "GITHUB_TOKEN", "continue-on-error",
    ),
}


def read(root: Path, name: str) -> str:
    return (root / name).read_text()


def files(root: Path) -> list[str]:
    """The tracked files, or every file outside a Git checkout."""
    listed = subprocess.run(["git", "ls-files", "-z"], cwd=root, capture_output=True, check=False)
    if listed.returncode == 0:
        return [entry.decode() for entry in listed.stdout.split(b"\0") if entry]
    return [path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()]


def run_scripts(text: str, key: str = "run") -> list[str]:
    """Each `key:` value, with the more-indented lines that continue it."""
    lines = text.splitlines()
    scripts: list[str] = []
    for number, line in enumerate(lines):
        if (match := re.match(rf"( *)(- )?{key}:(.*)", line)) is None:
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
    paths = sorted([*github.glob("workflows/*.y*ml"), *github.glob("actions/**/action.y*ml")])
    mise = f"version: {pin('tools.mise', root)}"
    for path in paths:
        name = str(path.relative_to(github))
        text = path.read_text()
        if floating := re.findall(r"runs-on: (\S+-latest)", text):
            fail(f"{name} must pin its runner image, not {floating[0]}")
        if name != "workflows/release.yml":
            for needle in ("secrets.", "secrets[", "environment:"):
                if needle in text:
                    fail(f"{name} must not use {needle}")
        if re.search(r"uses: (?:actions/setup-[\w-]+|oven-sh/setup-bun)@", text):
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
        if "--build-context" in text or "build-contexts:" in text:
            fail(f"{name} must not substitute a build stage")
        # Comments do not count, so a guard cannot survive as text alone.
        code = re.sub(r"(?m)^\s*#.*\n", "", text)
        last = -1
        for needle in ORDERED.get(name, ()):
            if (position := code.find(needle, last + 1)) < 0:
                fail(f"{name} is missing or misorders invariant: {needle.strip()}")
            last = position


def check_workflows(root: Path) -> None:
    workflows = root / ".github" / "workflows"
    names = {path.name for path in workflows.glob("*.y*ml")}
    if names != TRIGGERS.keys():
        fail(f"unreviewed workflow set: {sorted(names ^ TRIGGERS.keys())}")
    for name, expected in TRIGGERS.items():
        text = (workflows / name).read_text()
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
    release = (workflows / "release.yml").read_text()
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
    request = (workflows / "release-request.yml").read_text()
    jobs = [job for job in JOB.split(request.split("\njobs:\n", 1)[1]) if job.strip()]
    # release.py binds the request's artifact to its jobs by these names, each once.
    named: list[str] = []
    for job in jobs:
        platforms = re.search(r"(?m)^        platform: \[(.*)\]$", job)
        named += [name.replace("${{ matrix.platform }}", platform.strip())
                  for name in re.findall(r"(?m)^    name: (.*)$", job)
                  for platform in (platforms.group(1).split(",") if platforms and "matrix" in name else [""])]
    if sorted(named) != sorted(request_jobs()):
        fail(f"release-request.yml must run the jobs {sorted(request_jobs())} once each, as release.py expects")
    first = jobs[0].split(":", 1)[0]
    for job in jobs:
        needs = re.search(r"(?m)^    needs: \[?([^]\n]*)\]?$", job)
        if job != jobs[0] and first not in {name.strip() for name in (needs.group(1) if needs else "").split(",")}:
            fail(f"release-request.yml: every build must need {first}, which refuses other refs")
        # An upload follows symlinks, so each sends only the regular files assets.py upload moved into its directory.
        steps = [step for step in STEP.split(job) if step.strip()]
        for before, step in zip(steps, steps[1:]):
            if "uses: actions/upload-artifact@" in step and (
                    [UPLOAD.fullmatch(run.strip()) is not None for run in run_scripts(before)] != [True]
                    or "path: ${{ runner.temp }}/upload/\n" not in step):
                fail("release-request.yml: every upload must directly follow assets.py upload and send its directory")
    if STATUS.search(request):
        fail("release-request.yml: a job or step must not run past a failure, skip or cancellation")
    scopes = re.findall(r"(?m)^ *permissions:.*(?:\n +\S.*)*", request)
    if scopes != ["permissions:\n  contents: read"]:
        fail("release-request.yml: the untrusted build may only read contents")
    if any(DETACHED.search(run) for run in run_scripts(request)):
        fail("release-request.yml: a run step must not background a command or create a link")
    steps = STEP.split(request.split("\njobs:", 1)[1])
    validators = [step for step in steps
                  if [run.strip() for run in run_scripts(step)] == [PREPARE.removeprefix("run: ")]]
    if len(validators) != 1:
        fail("release-request.yml must run the request validator in exactly one step")
    for step in steps:
        if step is validators[0]:
            continue
        if any(INPUTS.search(expression) for expression in [*EXPRESSION.findall(step), *run_scripts(step, "if")]):
            fail("release-request.yml: dispatch inputs may reach only the request validator")
        if "github.token" in step:
            fail("release-request.yml: only the request validator may read the token")
        if f"run: {IMAGE_SCRIPT}" in step and "CONTEXT: ${{ needs.request.outputs.context }}\n" not in step:
            fail("release-request.yml: the image must build the resolved CONTEXT")
    check_image_script(read(root, IMAGE_SCRIPT))
    # Caches are keyed by job; only CI writes them.
    for name in sorted(names - {"ci.yml"}):
        for step in STEP.split((workflows / name).read_text()):
            if "setup-project" in step and "cache: 'false'" not in step:
                fail(f"{name} must disable every cache")
    for step in STEP.split(read(root, ".github/actions/setup-project/action.yml")):
        if ("uses: actions/cache@" in step and "inputs.cache == 'true'" not in step
                or "uses: jdx/mise-action@" in step and "cache: ${{ inputs.cache }}" not in step):
            fail("setup-project: every cache must follow the cache input")


def check_image_script(text: str) -> None:
    """Image archives alone record max provenance; no build imports a cache, substitutes a stage or gets a
    secret, an entitlement or other attestations."""
    for needle in (*FORBIDDEN["workflows/release-request.yml"], "--secret", "--allow", "--attest"):
        if needle in text:
            fail(f"{IMAGE_SCRIPT} must not contain {needle}")
    code = "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("#"))
    if DETACHED.search(code):
        fail(f"{IMAGE_SCRIPT} must not background a command or create a link")
    commands = [command.strip() for command in code.replace("\\\n", " ").splitlines()]
    archives = [command for command in commands if "type=oci" in command]
    # Flags inside build() apply to every call, so only a call line may set provenance.
    provenance = {command: re.findall(r"--provenance[= ](\S+)", command) for command in commands}
    if len(archives) != 1 or not archives[0].startswith("build ") or provenance[archives[0]] != ["mode=max"] or any(
            values and (values != ["false"] or not command.startswith("build "))
            for command, values in provenance.items() if command != archives[0]):
        fail(f"{IMAGE_SCRIPT}: the one OCI archive export must record --provenance mode=max, every other build none")


def check_ci(root: Path) -> None:
    ci = read(root, ".github/workflows/ci.yml")
    tasks = tomllib.loads(read(root, "mise.toml"))["tasks"]

    def runs(task: str) -> bool:
        """Whether CI runs `task`, itself or every task it consists of."""
        steps = [step["task"] for step in tasks[task]["run"] if isinstance(step, dict)]
        ran = re.search(rf"mise run {re.escape(task)}(?![\w-])", ci) is not None
        return ran or bool(steps) and len(steps) == len(tasks[task]["run"]) and all(map(runs, steps))

    # A local build is the one step CI replaces, with the image's reviewed server.
    local = {name for name, task in tasks.items() if any(marker in str(task.get("run", "")) for marker in DEVELOPMENT_BUILDS)}
    for task in (step["task"] for step in tasks["ci"]["run"] if step["task"] not in local):
        if not runs(task):
            fail(f"CI must run the local gate step {task}")
    if "run: python3 -m scripts.legal.check_git_sources --verify\n" not in ci:
        fail("CI must verify the Cargo fork pins with check_git_sources --verify")
    e2e = next((job for job in JOB.split(ci) if job.startswith("e2e:\n")), "")
    setups = [step for step in STEP.split(e2e) if "setup-project" in step]
    if not setups or any("cache: ${{ github.event_name != 'schedule' }}\n" not in step for step in setups):
        fail("CI: the E2E job must not cache on schedule, where it tests unpinned browsers")
    jobs = set(re.findall(r"(?m)^  ([a-z0-9-]+):$", ci.split("\njobs:\n", 1)[1])) - {"gate"}
    gate = re.search(r"(?ms)^  gate:\n.*?^    needs: \[([^]]*)\]", ci)
    if missing := sorted(jobs - {name.strip() for name in (gate.group(1) if gate else "").split(",")}):
        fail(f"CI Gate must need every job: {missing}")


def check_development_notices(root: Path) -> None:
    """Nothing CI or a release runs builds with unreviewed development notices: no workflow, image build, mise
    task, scripts/*.sh or Python driver they reach starts the collector's --development mode or a local build."""
    tasks = tomllib.loads(read(root, "mise.toml"))["tasks"]
    texts = [path.read_text()
             for path in sorted([*(root / ".github").rglob("*.y*ml"), root / "container/Dockerfile"])]
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
    check_dockerfile("container/Dockerfile", read(root, "container/Dockerfile"))
    check_actions(root)
    check_workflows(root)
    check_ci(root)
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
