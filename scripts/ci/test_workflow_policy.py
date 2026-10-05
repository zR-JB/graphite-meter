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
RUST = "container/Dockerfile.rust"
PUBLISH = "  extra:\n    if: needs.verify.outputs.publish == 'true'\n    environment: other\n"
MUTATIONS: tuple[tuple[str, str | None, str, str], ...] = (
    (SETUP, None, "# ${{ secrets.TOKEN }}\n", r"secrets\."),
    (W + "ci.yml", "runs-on: ubuntu-24.04", "runs-on: ubuntu-latest", "ubuntu-latest"),
    (W + "release.yml", "run: python3 scripts/ci/release.py recheck",
     'run: echo "${{ github.head_ref }}"', "through env"),
    (REQUEST, "          if [[ -z", "          echo ${{ github.ref }}\n          if [[ -z",
     "through env"),
    ("container/Dockerfile", None, "FROM docker.io/library/alpine:3 AS extra\n", "digest-pinned"),
    ("container/Dockerfile", "# Graphite Meter", "#Syntax = example/frontend\n# Graphite Meter",
     "BuildKit frontend"),
    (W + "ci.yml", None, PINNED_STEP.format("actions/setup-go"), "through mise"),
    (REQUEST, "ref: ${{ github.sha }}", "ref: ${{ inputs.sha }}", "triggering github.sha"),
    (W + "release.yml", "cache: false", "cache: true", "cache: false"),
    (SETUP, "install_args:", "args:", "install_args"),
    (REQUEST, "cache: 'false'", "cache: 'true'", "disable every cache"),
    (SETUP, "inputs.client-deps == 'true' && inputs.cache == 'true'", "inputs.client-deps == 'true'",
     "follow the cache input"),
    (SETUP, "cache: ${{ inputs.cache }}", "cache: true", "follow the cache input"),
    (REQUEST, "buildkitd-flags: --log-level=info",
     "buildkitd-flags: --allow-insecure-entitlement network.host", "insecure-entitlement"),
    (W + "release.yml", "TARGET_SHA: ${{ github.sha }}",
     "TARGET_SHA: ${{ needs.verify.outputs.sha }}", "TARGET_SHA"),
    (REQUEST, "provenance: mode=max", "provenance: false", "provenance"),
    (REQUEST, "if: ${{ github.ref == format(", "if: ${{ true || (", "default_branch"),
    (REQUEST, "SOURCE_SHA: ${{ steps.request.outputs.remote_sha }}",
     "SOURCE_SHA: ${{ inputs.sha }}", "remote_sha"),
    (REQUEST, PREPARE, PREPARE + "\n      - run: echo \"$RAW\"\n        env:\n"
     "          RAW: ${{ inputs.sha }}\n", "only the request validator"),
    (W + "release.yml", "environment: ghcr-release", "environment: other", "ghcr-release"),
    (W + "release.yml", "    if: needs.verify.outputs.publish == 'true'\n    needs: verify\n",
     "    needs: verify\n", "release secrets"),
    (W + "release.yml", "secrets.GHCR_TOKEN", "secrets.OTHER_TOKEN", "release secrets"),
    (W + "release.yml", None, "  extra:\n    environment: ghcr-release\n", "release secrets"),
    (W + "ci.yml", None, "# ${{ secrets.GHCR_TOKEN }}\n", r"secrets\."),
    (W + "release.yml", "        run: python3 scripts/ci/release.py recheck\n", "",
     "misorders invariant: run: python3 scripts/ci/release.py recheck"),
    (W + "release.yml", "run: scripts/ci/publish.sh image", "run: scripts/ci/publish.sh aliases",
     "misorders invariant: run: scripts/ci/publish.sh image"),
    (W + "ci.yml", "permissions: {contents: read}", "permissions: {contents: write}",
     "write permission"),
    (W + "release.yml", "on:\n", "on:\n  push:\n    tags: ['v*']\n", "triggered only by"),
    (W + "release.yml", "on:\n", "on:\n  workflow_dispatch:\n", "triggered only by"),
    (W + "release.yml", "head_branch == 'main'", "head_branch != ''", "head_branch == 'main'"),
    (W + "extra.yml", None, "on:\n  push:\n", "unreviewed workflow set"),
    (W + "ci.yml", "permissions:\n  contents: read\n\nenv:", "env:", "top-level permissions"),
    (REQUEST, "  contents: read", "  contents: write", "write permission"),
    (W + "release.yml", "    steps:\n", "    steps:" + PINNED_STEP.format("actions/cache"),
     "repository code"),
    (W + "release.yml", "        run: python3 scripts/ci/release.py verify\n",
     "        run: python3 scripts/ci/release.py verify\n      - run: mise run release-check\n",
     "mise run"),
    (REQUEST, "    steps:\n", "    steps:" + PINNED_STEP.format("actions/cache"),
     "repository code"),
    (W + "ci.yml", "mise run core-check", "mise run client-ci", "local gate step core-check"),
    (W + "ci.yml", "mise run server-race", "mise run server-test", "local gate step server-race"),
    (W + "ci.yml", "mise run legal-check", "mise run legal-generate",
     "local gate step legal-check"),
    (REQUEST, "VERSION= mise run legal-check\n", "", "committed legal outputs"),
    ("certs/dev.txt", None, "local development certificate", "TLS certificate/key paths"),
    ("notes.txt", None, "-----BEGIN " + "PRIVATE KEY-----", "PEM"),
    (W + "ci.yml", "on:\n", "on:\n  pull_request_target:\n", "triggered only by"),
    (W + "ci.yml", "permissions: {contents: read}", "permissions: write-all", "write permission"),
    (W + "ci.yml", None, "  extra:\n    environment: ghcr-release\n", "must not use environment:"),
    (RELEASE, None, PUBLISH + "    env:\n      TOKEN: ${{ secrets.GHCR_TOKEN }}\n",
     "release secrets"),
    (REQUEST, "  contents: read\n", "  contents: read\n  actions: read\n", "only read contents"),
    (REQUEST, "    runs-on:", "    permissions: read-all\n    runs-on:", "only read contents"),
    (REQUEST, "EVENT_SHA: ${{ github.sha }}", "EVENT_SHA: ${{ inputs.sha }}", "EVENT_SHA"),
    (REQUEST, PREPARE, "        run: python3 -c pass\n", "release.py prepare"),
    (REQUEST, 'python3 scripts/ci/verify_release_assets.py "$VERSION"', "true",
     "verify_release_assets"),
    (REQUEST, "uses: docker/build-push-action@", "uses: docker/bake-action@", "build-push-action"),
    (RELEASE, "github.event.workflow_run.conclusion == 'success'\n      && ", "",
     "conclusion == 'success'"),
    (RELEASE, "workflow_run.event == 'workflow_dispatch'", "workflow_run.event != 'push'",
     "workflow_dispatch"),
    (RELEASE, "workflow_run.path == '.github", "workflow_run.path != '.github", "path =="),
    (RELEASE, "run: python3 scripts/ci/release.py verify", "run: echo verified",
     "release.py verify"),
    (RELEASE, "        if: steps.verify.outputs.publish == 'true'\n", "", "hand off"),
    (RELEASE, "group: release-publish-${{ github.repository }}",
     "group: release-publish-${{ github.run_id }}", "group: release-publish"),
    (RELEASE, "cancel-in-progress: false", "cancel-in-progress: true", "cancel-in-progress"),
    (RELEASE, "run: python3 scripts/ci/release.py publish", "run: echo released",
     "release.py publish"),
    (RELEASE, "run: scripts/ci/publish.sh aliases", "run: echo promoted", "publish.sh aliases"),
    (RELEASE, "SOURCE_SHA: ${{ needs.verify.outputs.sha }}",
     "SOURCE_SHA: ${{ github.event.workflow_run.head_sha }}", "head_sha"),
    (RELEASE, "SOURCE_SHA: ${{ needs.verify.outputs.sha }}",
     "SOURCE_SHA: ${{ github.event.pull_request.head.sha }}", "pull_request.head"),
    (RELEASE, "secrets.GHCR_TOKEN", "secrets['GHCR_TOKEN']", r"secrets\["),
    (REQUEST, '[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]', '[[ -n "$SOURCE_SHA" ]]', "SOURCE_SHA"),
    (REQUEST, "no-cache: true", "no-cache: false", "no-cache"),
    (REQUEST, "          no-cache: true\n", "          no-cache: true\n          cache-from: type=gha\n",
     "cache-from"),
    (REQUEST, "          no-cache: true\n", "          no-cache: true\n          cache-to: type=gha\n",
     "cache-to"),
    (REQUEST, "          no-cache: true\n", "          no-cache: true\n          secrets: GIT_AUTH_TOKEN=x\n",
     "GIT_AUTH_TOKEN"),
    (REQUEST, "github-token: ''", "github-token: ${{ github.token }}", "github-token"),
    (REQUEST, "            GM_CLIENT_REVISION=${{ steps.request.outputs.sha }}\n", "",
     "GM_CLIENT_REVISION"),
    (W + "ci.yml", "security, secret-scan, rust", "security, rust", r"Gate must need every job: \['secret-scan'\]"),
    (W + "ci.yml", "mise run rust-check\n", "cargo test\n", "local gate step rust-check"),
    (W + "ci.yml", "mise run rust-check-targets\n", "cargo check\n", "local gate step rust-check-targets"),
    (W + "ci.yml", "check_git_sources --verify\n", "check_git_sources\n", "check_git_sources --verify"),
    # rustup would replace itself from the network before it installs and checks a toolchain.
    (SETUP, "install --no-self-update", "install", "--no-self-update"),
    (SETUP, "        python3 scripts/ci/toolchains.py verify-rust\n", "", "misorders invariant"),
    (SETUP, "inputs.rust == 'true' && inputs.cache == 'true'", "inputs.rust == 'true'",
     "follow the cache input"),
    # Development notices stay out of CI and releases, directly or through a task or script a job runs.
    (W + "ci.yml", None, "# mise run rust-server-run\n", "development notices"),
    ("mise.toml", "[tasks.rust-check]\n", '[tasks.rust-check]\ndepends = ["rust-client-build"]\n',
     "development notices"),
    ("container/Dockerfile", None, "RUN python3 -m scripts.legal.rust --development\n", "development notices"),
    ("scripts/release-artifacts.sh", None, "python3 -m scripts.rust_build\n", "development notices"),
    (RUST, None, "FROM docker.io/library/debian:bookworm AS extra\n", "Dockerfile.rust base images must be"),
    (RUST, None, "FROM --platform=$BUILDPLATFORM ${BASE} AS extra\n", "digest-pinned"),
    (RUST, "--browser-scan /tmp/browser-modules.json \\", "--browser-scan /tmp/browser-modules.json --development \\",
     "development notices"),
    (RUST, "musl-dev:amd64=1.2.3-1", "musl-dev:amd64", "exact apt package versions"),
    (RUST, "RUN printf 'Types: deb", "RUN apt-get update\nRUN printf 'Types: deb", "snapshot.debian.org"),
    (RUST, "/20260927T180000Z", "", "snapshot.debian.org"),
    # A change to any input of the Rust image selects the Rust jobs.
    (".github/ci-paths.yml", "  - 'client/**'\n  - 'api/**'\n", "  - 'api/**'\n",
     r"rust misses \['client/package.json'"),
    (".github/ci-paths.yml", "  - 'container/Dockerfile.rust'\n", "", r"rust misses \['container/Dockerfile.rust'\]"),
    (RUST, "COPY api/ /src/api/\n", "COPY api/ docs/ /src/api/\n", r"rust misses \['docs/x'\]"),
)


class WorkflowPolicyTests(unittest.TestCase):
    def tree(self) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        shutil.copytree(ROOT / ".github", root / ".github")
        shutil.copytree(ROOT / "scripts", root / "scripts", ignore=shutil.ignore_patterns("__pycache__"))
        for name in ("mise.toml", "mise.lock", "go/go.mod", "container/Dockerfile", "container/Dockerfile.rust",
                     "rust/rust-toolchain.toml"):
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
