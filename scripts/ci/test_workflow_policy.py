from __future__ import annotations

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
DOCKERFILE = "container/Dockerfile"
PUBLISH = "  extra:\n    if: needs.verify.outputs.publish == 'true'\n    environment: other\n"
MUTATIONS: tuple[tuple[str, str | None, str, str], ...] = (
    (SETUP, None, "# ${{ secrets.TOKEN }}\n", r"secrets\."),
    (W + "ci.yml", "runs-on: ubuntu-24.04", "runs-on: ubuntu-latest", "pin its runner image"),
    (W + "ci.yml", "runs-on: windows-2025", "runs-on: windows-latest", "pin its runner image"),
    (W + "release.yml", "run: python3 scripts/ci/release.py recheck",
     'run: echo "${{ github.head_ref }}"', "through env"),
    (REQUEST, "          if [[ -z", "          echo ${{ github.ref }}\n          if [[ -z",
     "through env"),
    (DOCKERFILE, None, "FROM docker.io/library/alpine:3 AS extra\n", "digest-pinned"),
    (DOCKERFILE, None, "FROM --platform=$BUILDPLATFORM ${BASE} AS extra\n", "digest-pinned"),
    (DOCKERFILE, "# Graphite Meter", "#Syntax = example/frontend\n# Graphite Meter", "BuildKit frontend"),
    (DOCKERFILE, "musl-dev:amd64=1.2.3-1", "musl-dev:amd64", "exact apt package versions"),
    (DOCKERFILE, "RUN printf 'Types: deb", "RUN apt-get update\nRUN printf 'Types: deb", "snapshot.debian.org"),
    (DOCKERFILE, "/20260927T180000Z", "", "snapshot.debian.org"),
    (W + "ci.yml", None, PINNED_STEP.format("actions/setup-go"), "through mise"),
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
    (REQUEST, "          no-cache: true\n", "          no-cache: true\n          allow: network.host\n"
     "          # allow-insecure-entitlement\n", "insecure-entitlement"),
    (W + "release.yml", "TARGET_SHA: ${{ github.sha }}",
     "TARGET_SHA: ${{ needs.verify.outputs.sha }}", "TARGET_SHA"),
    (REQUEST, "provenance: mode=max", "provenance: false", "provenance"),
    (REQUEST, "github-token: ''", "github-token: ${{ github.token }}", "github-token"),
    (REQUEST, "context: ${{ steps.source.outputs.context }}\n", "context: .\n", "context"),
    (REQUEST, "no-cache: true", "no-cache: false", "first image build must declare no-cache"),
    (REQUEST, "          no-cache: true\n", "          no-cache: true\n          cache-from: type=gha\n",
     "cache-from"),
    (REQUEST, "          no-cache: true\n", "          no-cache: true\n          cache-to: type=gha\n",
     "cache-to"),
    (REQUEST, "          no-cache: true\n", "          no-cache: true\n          secrets: GIT_AUTH_TOKEN=x\n",
     "GIT_AUTH_TOKEN"),
    (REQUEST, "if: ${{ github.ref == format(", "if: ${{ true || (", "misorders invariant: if:"),
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
    (REQUEST, "name: Build untrusted release candidate", "name: Build the release candidate",
     "as release.py expects"),
    (REQUEST, "\njobs:\n", "\njobs:\n  other:\n    name: Another job\n    runs-on: ubuntu-24.04\n"
     "    steps:\n      - run: true\n", "one job named"),
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
    def tree(self) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        shutil.copytree(ROOT / ".github", root / ".github")
        for name in ("scripts", "rust/interop"):
            shutil.copytree(ROOT / name, root / name, ignore=shutil.ignore_patterns("__pycache__"))
        for name in ("mise.toml", "mise.lock", "client/bun.lock", "container/Dockerfile", "rust/rust-toolchain.toml",
                     "rust/Cargo.lock"):
            (root / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, root / name)
        return root

    def test_repository_satisfies_policy(self) -> None:
        check_repository(self.tree())

    def test_each_violation_is_rejected(self) -> None:
        for name, old, new, error in MUTATIONS:
            with self.subTest(name=name, error=error):
                root = self.tree()
                path = root / name
                text = path.read_text() if path.exists() else ""
                if old is None:
                    text += new
                else:
                    self.assertIn(old, text)
                    text = text.replace(old, new, 1)
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
                with self.assertRaisesRegex(ControlPlaneError, error):
                    check_repository(root)

    def test_release_identity_comes_only_from_the_run_context(self) -> None:
        for name, marker in ((REQUEST, "release.py prepare"), (RELEASE, "release.py verify"),
                             (RELEASE, "release.py publish")):
            text = (ROOT / name).read_text()
            step = next(step for step in re.split(r"(?m)^(?=      - )", text) if marker in step)
            for variable in re.findall(r"(?m)^ +(?!GH_TOKEN)([A-Z_]+): \$\{\{ github\.", step):
                with self.subTest(marker=marker, variable=variable):
                    root = self.tree()
                    rebound = re.sub(rf"(?m)^( +{variable}): .*$", r"\1: ${{ github.job }}", step)
                    (root / name).write_text(text.replace(step, rebound))
                    with self.assertRaisesRegex(ControlPlaneError, f" {variable} must be exactly"):
                        check_repository(root)


if __name__ == "__main__":
    unittest.main()
