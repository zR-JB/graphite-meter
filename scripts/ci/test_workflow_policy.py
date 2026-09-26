from __future__ import annotations

import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from workflow_policy import PolicyError, check_repository

ROOT = Path(__file__).resolve().parents[2]
W = ".github/workflows/"
OCI = ".github/actions/build-oci/action.yml"
PINNED_STEP = "\n      - uses: {}@" + "a" * 40 + "\n        with: {{persist-credentials: false}}\n"
REQUEST = W + "release-request.yml"
PREPARE = "        run: python3 scripts/ci/release.py prepare\n"
RELEASE = W + "release.yml"
PUBLISH = "  extra:\n    if: needs.verify.outputs.publish == 'true'\n    environment: other\n"
MUTATIONS: tuple[tuple[str, str | None, str, str], ...] = (
    (OCI, None, "# ${{ secrets.TOKEN }}\n", r"secrets\."),
    (W + "ci.yml", "runs-on: ubuntu-24.04", "runs-on: ubuntu-latest", "ubuntu-latest"),
    (W + "release.yml", "run: python3 scripts/ci/release.py recheck",
     'run: echo "${{ github.head_ref }}"', "through env"),
    (OCI, "        if [[ -z", "        echo ${{ inputs.version }}\n        if [[ -z",
     "through env"),
    ("container/Dockerfile", None, "FROM docker.io/library/alpine:3 AS extra\n", "digest-pinned"),
    ("container/Dockerfile", "# Graphite Meter", "#Syntax = example/frontend\n# Graphite Meter",
     "BuildKit frontend"),
    (W + "ci.yml", None, PINNED_STEP.format("actions/setup-go"), "through mise"),
    (REQUEST, "ref: ${{ github.sha }}", "ref: ${{ inputs.sha }}", "triggering github.sha"),
    (W + "release.yml", "cache: false", "cache: true", "cache: false"),
    (".github/actions/setup-project/action.yml", "install_args:", "args:", "install_args"),
    (REQUEST, "bun-cache: 'false'", "bun-cache: 'true'", "disable every cache"),
    (OCI, "buildkitd-flags: --log-level=info",
     "buildkitd-flags: --allow-insecure-entitlement network.host", "insecure-entitlement"),
    (W + "release.yml", "TARGET_SHA: ${{ github.sha }}",
     "TARGET_SHA: ${{ needs.verify.outputs.sha }}", "TARGET_SHA"),
    (OCI, "provenance: mode=max", "provenance: false", "provenance"),
    (REQUEST, "if: ${{ github.ref == format(", "if: ${{ true || (", "default_branch"),
    (REQUEST, "source-sha: ${{ steps.request.outputs.remote_sha }}",
     "source-sha: ${{ inputs.sha }}", "remote_sha"),
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
     "        run: python3 scripts/ci/release.py verify\n      - run: mise run release-build\n",
     "mise run"),
    (REQUEST, "    steps:\n", "    steps:" + PINNED_STEP.format("actions/cache"),
     "repository code"),
    (W + "ci.yml", "mise run core-check", "mise run client-ci", "local gate step core-check"),
    (W + "ci.yml", "mise run server-race", "mise run server-test", "local gate step server-race"),
    (W + "ci.yml", "mise run legal-check", "mise run legal-generate",
     "local gate step legal-check"),
    (REQUEST, "VERSION= mise run legal-check\n", "", "committed legal outputs"),
    (".github/ci-paths.yml", "  - 'client/src/app.css'\n", "", "client/src/app.css"),
    (W + "ci.yml", "chrome-version: ${{ steps", "chrome-version: latest #", "pinned Chromium"),
    ("client/package.json", "--parallel=3 --no-orphans", "--parallel=3", "no-orphans"),
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
    (REQUEST, "uses: ./.github/actions/build-oci", "uses: ./.github/actions/setup-project",
     "build-oci"),
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
    (RELEASE, "run: scripts/ci/publish.sh release", "run: echo released", "publish.sh release"),
    (RELEASE, "run: scripts/ci/publish.sh aliases", "run: echo promoted", "publish.sh aliases"),
    (RELEASE, "SOURCE_SHA: ${{ needs.verify.outputs.sha }}",
     "SOURCE_SHA: ${{ github.event.workflow_run.head_sha }}", "head_sha"),
    (RELEASE, "SOURCE_SHA: ${{ needs.verify.outputs.sha }}",
     "SOURCE_SHA: ${{ github.event.pull_request.head.sha }}", "pull_request.head"),
    (RELEASE, "secrets.GHCR_TOKEN", "secrets['GHCR_TOKEN']", r"secrets\["),
    (OCI, '[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]', '[[ -n "$SOURCE_SHA" ]]', "SOURCE_SHA"),
    (OCI, "no-cache: true", "no-cache: false", "no-cache"),
    (OCI, "        no-cache: true\n", "        no-cache: true\n        cache-from: type=gha\n",
     "cache-from"),
    (OCI, "        no-cache: true\n", "        no-cache: true\n        cache-to: type=gha\n",
     "cache-to"),
    (OCI, "        no-cache: true\n", "        no-cache: true\n        secrets: GIT_AUTH_TOKEN=x\n",
     "GIT_AUTH_TOKEN"),
    (OCI, "github-token: ''", "github-token: ${{ github.token }}", "github-token"),
    (OCI, "          GM_CLIENT_REVISION=${{ inputs.revision }}\n", "", "GM_CLIENT_REVISION"),
)


class WorkflowPolicyTests(unittest.TestCase):
    def tree(self) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        shutil.copytree(ROOT / ".github", root / ".github")
        for name in ("mise.toml", "mise.lock", "go/go.mod", "container/Dockerfile",
                     "client/package.json"):
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
                with self.assertRaisesRegex(PolicyError, error):
                    check_repository(root)

    def test_release_identity_comes_only_from_the_run_context(self) -> None:
        for name, marker in ((REQUEST, "release.py prepare"), (RELEASE, "release.py verify"),
                             (RELEASE, "publish.sh release")):
            text = (ROOT / name).read_text()
            step = next(step for step in re.split(r"(?m)^(?=      - )", text) if marker in step)
            for variable in re.findall(r"(?m)^ +(?!GH_TOKEN)([A-Z_]+): \$\{\{ github\.", step):
                with self.subTest(marker=marker, variable=variable):
                    root = self.tree()
                    rebound = re.sub(rf"(?m)^( +{variable}): .*$", r"\1: ${{ github.job }}", step)
                    (root / name).write_text(text.replace(step, rebound))
                    with self.assertRaisesRegex(PolicyError, f" {variable} must be exactly"):
                        check_repository(root)

    def test_pinned_linters_reject_unpinned_actions_and_unknown_outputs(self) -> None:
        zizmor = ("zizmor", "--offline", "--config", ".github/zizmor.yml", ".github")
        for command, old, new, error in (
            (zizmor, "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
             "actions/checkout@v7", "unpinned-uses"),
            (zizmor, "persist-credentials: false", "fetch-depth: 1", "artipacked"),
            (zizmor, "on:\n", "on:\n  pull_request_target:\n", "dangerous-triggers"),
            (("actionlint", "-shellcheck=", "-pyflakes=", W + "ci.yml"),
             "needs.plan.outputs.core ==", "needs.plan.outputs.missing ==", "missing"),
        ):
            if shutil.which(command[0]) is None:
                self.fail(f"{command[0]} must be on PATH; run through mise run pipeline-test")
            with self.subTest(error=error):
                root = self.tree()
                path = root / W / "ci.yml"
                self.assertIn(old, path.read_text())
                path.write_text(path.read_text().replace(old, new, 1))
                result = subprocess.run(command, cwd=root, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(error, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
