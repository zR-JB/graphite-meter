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
     "misorders invariant: run: scripts/ci/publish.sh image|VERSION must be exactly"),
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
    (REQUEST, "name: Build untrusted release candidate", "name: Build the release candidate", "as release.py expects"),
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
    (REQUEST, "uses: docker/build-push-action@", "uses: docker/bake-action@", "misorders invariant"),
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
     "SOURCE_SHA: ${{ github.event.workflow_run.head_sha }}", "SOURCE_SHA must be exactly"),
    (RELEASE, "IMAGE_TAG: ${{ needs.verify.outputs.version }}\n",
     "IMAGE_TAG: ${{ github.event.workflow_run.head_sha }}\n", "head_sha"),
    (RELEASE, "IMAGE_TAG: ${{ needs.verify.outputs.version }}\n",
     "IMAGE_TAG: ${{ github.event.pull_request.head.sha }}\n", "pull_request.head"),
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
    # A change to any input of the Rust image selects the jobs that build it.
    (".github/ci-paths.yml", "  - 'client/**'\n  - 'api/**'\n", "  - 'api/**'\n",
     r"rust-image misses \['client/package.json'"),
    (".github/ci-paths.yml", "  - 'container/Dockerfile.rust'\n", "",
     r"rust-image misses \['container/Dockerfile.rust'\]"),
    (RUST, "COPY api/ /src/api/\n", "COPY api/ docs/ /src/api/\n", r"rust-image misses \['docs/x'\]"),
    # A source-only Rust change selects the TUI exports.
    (".github/ci-paths.yml", "rust-release:\n  - *workflow\n  - 'rust/**'\n", "rust-release:\n  - *workflow\n",
     r"rust-release misses \['rust/rust-toolchain.toml'"),
    (RUST, "COPY LICENSE COPYRIGHT mise.toml ./\n", "COPY LICENSE COPYRIGHT mise.toml client/ ./\n",
     r"rust-release misses \['client/x'\]"),
    # A client lockfile change selects the staging check of the server's source offers.
    (".github/ci-paths.yml", "  - 'client/bun.lock'\n  - 'client/bunfig.toml'\n  - 'client/patches/**'\n",
     "  - 'client/bunfig.toml'\n  - 'client/patches/**'\n", r"rust-release misses \['client/bun.lock'\]"),
    # The release request builds Rust only for a validated request, its TUI archives only for a stable one, from
    # the source the build job resolved, empty, with provenance, without a token, and stages exactly its files; the
    # approved job publishes exactly what the verify job verified.
    (REQUEST, "name: Build untrusted Rust image", "name: Build Rust image", "as release.py expects"),
    (REQUEST, "    needs: build\n    if: needs.build.outputs.rust == 'true'\n", "    needs: build\n",
     "only when the validated request selects them"),
    (REQUEST, "    if: needs.build.outputs.rust == 'true' && needs.build.outputs.stable == 'true'\n",
     "    if: needs.build.outputs.rust == 'true'\n", "TUI archives only for a stable release"),
    # The TUI job runs after the image job, so release.py can bind each artifact to the one job that ran.
    (REQUEST, "    needs: [build, rust-image]\n", "    needs: build\n", "must run one after another"),
    (REQUEST, "context: ${{ needs.build.outputs.context }}\n          file: container/Dockerfile.rust\n"
     "          target: server\n", "context: .\n          file: container/Dockerfile.rust\n          target: server\n",
     "build the source the build job resolved"),
    (REQUEST, "context: ${{ steps.source.outputs.context }}\n          file: container/Dockerfile\n",
     "context: https://github.com/${{ github.repository }}.git\n          file: container/Dockerfile\n",
     "build the source the build job resolved"),
    (REQUEST, "      context: ${{ steps.source.outputs.context }}\n    steps:",
     "      context: ${{ github.event.inputs.sha }}\n    steps:", r"misorders invariant: \s+context"),
    (REQUEST, "dest=${{ runner.temp }}/rust-tui-export\n          no-cache: true\n",
     "dest=${{ runner.temp }}/rust-tui-export\n", "every image build must declare no-cache"),
    # Only the source offers' export reuses its image build, from the job's own builder and with the same inputs.
    (REQUEST, "dest=${{ runner.temp }}/rust-image.oci.tar\n          no-cache: true\n",
     "dest=${{ runner.temp }}/rust-image.oci.tar\n", "every image build must declare no-cache"),
    (REQUEST, "          outputs: type=local,dest=${{ runner.temp }}/rust-server-export\n          provenance: mode=max\n"
     "          github-token: ''\n          build-args: |\n            VERSION=${{ needs.build.outputs.version }}\n",
     "          outputs: type=local,dest=${{ runner.temp }}/rust-server-export\n          provenance: mode=max\n"
     "          github-token: ''\n          build-args: |\n            VERSION=${{ needs.build.outputs.version }}\n"
     "            PROFILE=ci\n", "every image build must declare no-cache"),
    (REQUEST, "          target: server-artifacts\n          platforms: linux/amd64,linux/arm64\n",
     "          target: server-artifacts\n          platforms: linux/amd64\n", "every image build must declare no-cache"),
    (REQUEST, "      # This job's fresh builder", "      - uses: docker/setup-buildx-action@" + "a" * 40 +
     "\n      # This job's fresh builder", "every image build must declare no-cache"),
    (REQUEST, "dest=${{ runner.temp }}/rust-tui-export\n          no-cache: true\n          provenance: mode=max\n",
     "dest=${{ runner.temp }}/rust-tui-export\n          no-cache: true\n          provenance: false\n",
     "every image build must declare provenance"),
    (REQUEST, "dest=${{ runner.temp }}/rust-image.oci.tar\n          no-cache: true\n          provenance: mode=max\n"
     "          github-token: ''\n", "dest=${{ runner.temp }}/rust-image.oci.tar\n          no-cache: true\n"
     "          provenance: mode=max\n          github-token: ${{ github.token }}\n", "every image build must declare"),
    (REQUEST, "dest=${{ runner.temp }}/rust-tui-export\n", "dest=${{ runner.temp }}/rust-tui-export\n"
     "          cache-from: type=gha\n", "cache-from"),
    (REQUEST, "VERSION=${{ needs.build.outputs.version }}\n\n      # The check",
     "VERSION=${{ inputs.tag }}\n\n      # The check", "only the request validator"),
    (REQUEST, "run: python3 scripts/ci/rust_release.py stage-image\n",
     'run: cp -R "$SERVER_EXPORT" "$OUT_DIR"\n', "rust_release.py stage-image"),
    (REQUEST, 'python3 -m scripts.package_rust "$VERSION" --output "$OUT_DIR" --check\n', "", "package_rust"),
    (RELEASE, "IMAGE_TAG: ${{ needs.verify.outputs.version }}-rust\n", "IMAGE_TAG: latest-rust\n",
     "misorders invariant: IMAGE_TAG"),
    (RELEASE, "DIGEST: ${{ needs.verify.outputs.rust_digest }}\n          ARCHIVE_DIR",
     "DIGEST: ${{ needs.verify.outputs.digest }}\n          ARCHIVE_DIR", "DIGEST must be exactly"),
    (RELEASE, "RUST_DIGEST: ${{ needs.verify.outputs.rust_digest }}", "RUST_DIGEST: ${{ github.sha }}",
     "RUST_DIGEST must be exactly"),
    (RELEASE, "RUST_OCI_SHA256: ${{ needs.verify.outputs.rust_oci_sha256 }}",
     "RUST_OCI_SHA256: ${{ needs.verify.outputs.oci_sha256 }}", "RUST_OCI_SHA256 must be exactly"),
    (RELEASE, "        if: steps.verify.outputs.publish == 'true' && steps.verify.outputs.rust == 'true'\n",
     "        if: steps.verify.outputs.rust == 'true'\n", "hand off"),
    # CI builds the TUI archives with the pinned BuildKit, which `docker build` uses only when named.
    (W + "ci.yml", "          BUILDX_BUILDER: ${{ steps.buildx.outputs.name }}\n", "",
     "misorders invariant: BUILDX_BUILDER"),
    # CI stages and verifies its Rust builds as a release does.
    (W + "ci.yml", "--target server-artifacts", "--target server", "--target server-artifacts"),
    (W + "ci.yml", "OUT_DIR=$RUNNER_TEMP/staged/tui python3 scripts/ci/rust_release.py stage-tui\n", "",
     "rust_release.py stage-tui"),
    (W + "ci.yml", "python3 scripts/ci/rust_release.py check\n", "python3 scripts/ci/verify_oci.py\n",
     "rust_release.py check"),
    (W + "ci.yml", "rust-tui, rust-release, rust-windows", "rust-tui, rust-windows",
     r"Gate must need every job: \['rust-release'\]"),
    (W + "ci.yml", "      rust-e2e, rust-tui", "      rust-tui", r"Gate must need every job: \['rust-e2e'\]"),
    # Each job runs on its reviewed filter, which the plan outputs, and never without the jobs it needs.
    (W + "ci.yml", "    needs: [plan, rust-image]\n    if: needs.plan.outputs.rust-image ==",
     "    needs: [plan, rust-image]\n    if: needs.plan.outputs.rust ==", r"reviewed filters: \['rust-e2e'\]"),
    (W + "ci.yml", "      rust-interop: ${{ github.event_name == 'push' || "
     "steps.filter.outputs.rust-interop == 'true' }}\n", "", r"the plan does not output: \['rust-interop', 'rust-perf'\]"),
    (W + "ci.yml", "steps.filter.outputs.rust-image == 'true'", "steps.filter.outputs.rust == 'true'",
     "filters by their names"),
    (".github/ci-paths.yml", "rust-release:\n  - *workflow\n", "rust-release:\n  - *workflow\n  - 'docs/**'\n",
     "CI job rust-release may run without rust-image"),
    ("rust/server/src/extra.rs", None, 'const NOTE: &str = include_str!("../../../docs/DEPLOYMENT.md");\n',
     "rust misses docs/DEPLOYMENT.md, which rust/server/src/extra.rs includes"),
    ("rust/client/tests/extra.rs", None, 'concat!(env!("CARGO_MANIFEST_DIR"), "/../../client/app.css")\n',
     "rust misses client/app.css"),
    # The daily advisory check runs cargo-deny's advisories without caches.
    (W + "advisories.yml", "on:\n", "on:\n  pull_request:\n", "triggered only by"),
    (W + "advisories.yml", "run: cargo deny --locked check advisories\n", "run: cargo deny --locked check bans\n",
     "misorders invariant: run: cargo deny --locked check advisories"),
    (W + "advisories.yml", "cache: 'false'", "cache: 'true'", "advisories.yml must disable every cache"),
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
        shutil.copytree(ROOT / "scripts", root / "scripts", ignore=shutil.ignore_patterns("__pycache__"))
        for name in ("mise.toml", "mise.lock", "go/go.mod", "go/go.sum", "client/bun.lock", "container/Dockerfile",
                     "container/Dockerfile.rust", "rust/rust-toolchain.toml", "rust/Cargo.lock"):
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
