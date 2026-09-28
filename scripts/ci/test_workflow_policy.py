from __future__ import annotations

import re
import shutil
import tempfile
import unittest
from pathlib import Path

from .github_api import ControlPlaneError
from .workflow_policy import check_repository

ROOT = Path(__file__).resolve().parents[2]
W = ".github/workflows/"
SETUP = ".github/actions/setup-project/action.yml"
PINNED_STEP = "\n      - uses: {}@" + "a" * 40 + "\n        with: {{persist-credentials: false}}\n"
REQUEST = W + "release-request.yml"
PREPARE = "        run: python3 -m scripts.ci.release prepare\n"
RELEASE = W + "release.yml"
PUBLISH = "  extra:\n    if: needs.verify.outputs.publish == 'true'\n    environment: other\n"
MUTATIONS: tuple[tuple[str, str | None, str, str], ...] = (
    (W + "fork-upkeep.yml", "secrets.FORK_UPKEEP_APP_PRIVATE_KEY", "secrets.OTHER_KEY", "upkeep App"),
    (W + "fork-upkeep.yml", "permission-pull-requests: write\n",
     "permission-pull-requests: write\n          permission-workflows: write\n", "never its workflows"),
    (W + "fork-upkeep.yml", "github.ref == format('refs/heads/{0}', github.event.repository.default_branch)",
     "true", "only from the default branch"),
    (W + "fork-upkeep.yml", "repositories: ${{ steps.inventory.outputs.repositories }}",
     "repositories: all", "fork inventory"),
    (SETUP, None, "# ${{ secrets.TOKEN }}\n", r"secrets\."),
    (W + "ci.yml", "runs-on: ubuntu-24.04", "runs-on: ubuntu-latest", "ubuntu-latest"),
    (W + "release.yml", "run: python3 -m scripts.ci.release recheck",
     'run: echo "${{ github.head_ref }}"', "through env"),
    (REQUEST, "          if [[ -z", "          echo ${{ github.ref }}\n          if [[ -z",
     "through env"),
    ("container/Dockerfile", None, "FROM docker.io/library/alpine:3 AS extra\n", "digest-pinned"),
    ("container/Dockerfile", "# Graphite Meter", "#Syntax = example/frontend\n# Graphite Meter",
     "BuildKit frontend"),
    ("container/Dockerfile.rust", None, "FROM --platform=$BUILDPLATFORM docker.io/library/alpine:3 AS extra\n",
     "Dockerfile.rust base images must be digest-pinned"),
    ("container/Dockerfile.rust", "FROM rust-amd64 AS rust-arm64", "FROM rust:1.98.1 AS rust-arm64",
     "Dockerfile.rust base images must be digest-pinned"),
    ("container/Dockerfile.rust", "FROM rust-${TARGETARCH} AS server-build", "FROM ${BASE} AS server-build",
     "Dockerfile.rust base images must be digest-pinned"),
    ("container/Dockerfile.rust", "# Experimental only", "# syntax=example/frontend\n# Experimental only",
     "BuildKit frontend"),
    ("container/Dockerfile.rust", "mingw-w64-x86-64-dev=10.0.0-3", "mingw-w64-x86-64-dev", "exact apt package"),
    ("container/Dockerfile.rust", "/20260927T180000Z", "", "one snapshot.debian.org timestamp"),
    ("container/Dockerfile.rust", "RUN printf", "RUN apt-get update && printf", "one snapshot.debian.org timestamp"),
    (W + "ci.yml", None, PINNED_STEP.format("actions/setup-go"), "through mise"),
    (REQUEST, "ref: ${{ github.sha }}", "ref: ${{ inputs.sha }}", "triggering github.sha"),
    (W + "ci.yml", "        with: {persist-credentials: false}\n",
     "        with:\n          ref: ${{ needs.build.outputs.sha }}\n          persist-credentials: false\n",
     "triggering github.sha"),
    (W + "release.yml", "cache: false", "cache: true", "cache: false"),
    (SETUP, "install_args:", "args:", "install_args"),
    (REQUEST, "cache: 'false'", "cache: 'true'", "disable every cache"),
    (REQUEST, "cache-mode: none\n", "", "no cache token"),
    (REQUEST, "cache-mode: none\n", "cache-mode: read\n", "no cache token"),
    (REQUEST, "    runs-on: macos-15\n", "    runs-on: macos-15\n    cache-mode: read\n", "no cache token"),
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
    (W + "release.yml", "        run: python3 -m scripts.ci.release recheck\n", "",
     "misorders invariant: run: python3 -m scripts.ci.release recheck"),
    (W + "release.yml", "run: scripts/ci/publish.sh image", "run: scripts/ci/publish.sh aliases",
     "misorders invariant"),
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
    (W + "release.yml", "        run: python3 -m scripts.ci.release verify\n",
     "        run: python3 -m scripts.ci.release verify\n      - run: mise run release-check\n",
     "mise run"),
    (REQUEST, "    steps:\n", "    steps:" + PINNED_STEP.format("actions/cache"),
     "repository code"),
    (W + "ci.yml", "mise run core-check", "mise run client-ci", "local gate step core-check"),
    (W + "ci.yml", "mise run server-race", "mise run server-test", "local gate step server-race"),
    (W + "ci.yml", "mise run legal-check", "mise run legal-generate",
     "local gate step legal-check"),
    (REQUEST, "VERSION= mise run legal-check\n", "", "legal-check|committed legal outputs"),
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
    (REQUEST, PREPARE, "        run: python3 -c pass\n", "release prepare"),
    (REQUEST, 'python3 -m scripts.ci.verify_release_assets "$VERSION"', "true",
     "verify_release_assets"),
    (REQUEST, "uses: docker/build-push-action@", "uses: docker/bake-action@", "misorders invariant"),
    (RELEASE, "github.event.workflow_run.conclusion == 'success'\n      && ", "",
     "conclusion == 'success'"),
    (RELEASE, "workflow_run.event == 'workflow_dispatch'", "workflow_run.event != 'push'",
     "workflow_dispatch"),
    (RELEASE, "workflow_run.path == '.github", "workflow_run.path != '.github", "path =="),
    (RELEASE, "run: python3 -m scripts.ci.release verify", "run: echo verified",
     "release verify"),
    (RELEASE, "        if: steps.verify.outputs.publish == 'true'\n", "", "hand off"),
    (RELEASE, "group: release-publish-${{ github.repository }}",
     "group: release-publish-${{ github.run_id }}", "group: release-publish"),
    (RELEASE, "cancel-in-progress: false", "cancel-in-progress: true", "cancel-in-progress"),
    (RELEASE, "run: python3 -m scripts.ci.release publish", "run: echo released",
     "release publish"),
    (RELEASE, "run: scripts/ci/publish.sh aliases", "run: echo promoted", "publish.sh aliases"),
    (RELEASE, "SOURCE_SHA: ${{ needs.verify.outputs.sha }}",
     "SOURCE_SHA: ${{ github.event.workflow_run.head_sha }}", "head_sha|SOURCE_SHA"),
    (RELEASE, "SOURCE_SHA: ${{ needs.verify.outputs.sha }}",
     "SOURCE_SHA: ${{ github.event.pull_request.head.sha }}", "pull_request.head|SOURCE_SHA"),
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
    (REQUEST, "/rust-export/tui\n          no-cache: true\n", "/rust-export/tui\n", "every image build must declare no-cache"),
    (REQUEST, "run: python3 -m scripts.ci.release stage-rust\n",
     'run: find "$RUST_EXPORT" -type f -exec cp {} "$RUST_ASSETS/" \\;\n', "release stage-rust"),
    (REQUEST, "python3 -m scripts.ci.release checksums\n", "shasum -a 256 ./* >checksums.txt\n",
     "release checksums"),
    (W + "ci.yml", "          python3 -m scripts.ci.release check-rust\n", "", "release check-rust"),
    (W + "ci.yml", "run: mise run rust-check-targets\n", "run: mise run rust-check\n", "rust-check-targets"),
    (W + "ci.yml", "run: mise run rust-delayed-downloads\n",
     "run: cargo test --workspace --test connection_faults quic_downloads -- --ignored\n", "rust-delayed-downloads"),
    (W + "ci.yml", "--target server-artifacts", "--target server", "target server-artifacts"),
    (W + "ci.yml", "rust-release,\n            ", "", r"Gate must need every job: \['rust-release'\]"),
)


class WorkflowPolicyTests(unittest.TestCase):
    def tree(self) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        shutil.copytree(ROOT / ".github", root / ".github")
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
        for name, marker in ((REQUEST, "release prepare"), (RELEASE, "release verify"),
                             (RELEASE, "release publish"), (RELEASE, "release recheck")):
            text = (ROOT / name).read_text()
            step = next(step for step in re.split(r"(?m)^(?=      - )", text) if marker in step)
            for variable in re.findall(r"(?m)^ +(?!GH_TOKEN)([A-Z][A-Z0-9_]*): \$\{\{ (?:github|needs)\.", step):
                with self.subTest(marker=marker, variable=variable):
                    root = self.tree()
                    rebound = re.sub(rf"(?m)^( +{variable}): .*$", r"\1: ${{ github.job }}", step)
                    (root / name).write_text(text.replace(step, rebound))
                    with self.assertRaisesRegex(ControlPlaneError, f" {variable} must be exactly"):
                        check_repository(root)


if __name__ == "__main__":
    unittest.main()
