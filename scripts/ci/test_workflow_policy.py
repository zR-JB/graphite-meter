from __future__ import annotations

import shutil
import tempfile
import unittest
from pathlib import Path

from workflow_policy import PolicyError, check_repository

ROOT = Path(__file__).resolve().parents[2]
W = ".github/workflows/"
OCI = ".github/actions/build-oci/action.yml"
CHECKOUT = "uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1"
PINNED_STEP = "\n      - uses: {}@" + "a" * 40 + "\n        with: {{persist-credentials: false}}\n"
REQUEST = W + "release-request.yml"
PREPARE = "        run: python3 scripts/ci/release.py prepare\n"
MUTATIONS: tuple[tuple[str, str | None, str, str], ...] = (
    (W + "ci.yml", CHECKOUT, "uses: actions/checkout@v7", "40-character commit SHA"),
    (OCI, None, "# ${{ secrets.TOKEN }}\n", r"secrets\."),
    (W + "ci.yml", "runs-on: ubuntu-24.04", "runs-on: ubuntu-latest", "ubuntu-latest"),
    (W + "release.yml", 'run: echo "::notice::', 'run: echo "${{ github.head_ref }}',
     "through env"),
    (OCI, "        if [[ -z", "        echo ${{ inputs.version }}\n        if [[ -z",
     "through env"),
    ("container/Dockerfile", None, "FROM docker.io/library/alpine:3 AS extra\n", "digest-pinned"),
    (W + "ci.yml", None, PINNED_STEP.format("actions/setup-go"), "through mise"),
    (W + "ci.yml", "persist-credentials: false", "fetch-depth: 1", "persist-credentials"),
    (REQUEST, "ref: ${{ github.sha }}", "ref: ${{ inputs.sha }}", "triggering github.sha"),
    (W + "release.yml", "cache: false", "cache: true", "cache: false"),
    (".github/actions/setup-project/action.yml", "install_args:", "args:", "install_args"),
    (REQUEST, "bun-cache: 'false'", "bun-cache: 'true'", "disable every cache"),
    (OCI, "buildkitd-flags: --log-level=info",
     "buildkitd-flags: --allow-insecure-entitlement network.host", "insecure-entitlement"),
    (W + "release.yml", "target_sha: ${{ github.sha }}",
     "target_sha: ${{ needs.verify.outputs.sha }}", "target_sha"),
    (W + "_publish-release.yml", "/releases?per_page=100", "/releases/tags/$TAG", "releases/tags"),
    (W + "_publish-oci.yml", 'gh api "repos/$REPOSITORY/commits/main"', "echo skip",
     "commits/main"),
    (W + "_publish-oci.yml", "-${{ inputs.tag }}", "", "group: publish-oci"),
    (OCI, "provenance: mode=max", "provenance: false", "provenance"),
    (REQUEST, "if: ${{ github.ref == format(", "if: ${{ true || (", "default_branch"),
    (REQUEST, "source-sha: ${{ steps.request.outputs.remote_sha }}",
     "source-sha: ${{ inputs.sha }}", "remote_sha"),
    (REQUEST, PREPARE, PREPARE + "\n      - run: echo \"$RAW\"\n        env:\n"
     "          RAW: ${{ inputs.sha }}\n", "only the request validator"),
    (W + "release.yml", "environment: ghcr-release", "", "environment"),
    (W + "release.yml", "    if: needs.verify.outputs.publish == 'true'\n    needs: [verify, r",
     "    needs: [verify, r", "require publish mode"),
    (W + "release.yml", "  approval:\n", "  publish-image:\n    uses: ./x.yml\n  approval:\n",
     "misorders invariant: publish-image"),
    (W + "release.yml", "on:\n", "on:\n  push:\n    tags: ['v*']\n", "triggered only by"),
    (W + "release.yml", "on:\n", "on:\n  workflow_dispatch:\n", "triggered only by"),
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
    (W + "ci.yml", "mise run server-race", "mise run server-test", "local gate step server-test"),
    (".github/ci-paths.yml", "  - 'client/src/app.css'\n", "", "client/src/app.css"),
    (W + "ci.yml", "chrome-version: ${{ steps", "chrome-version: latest #", "pinned Chromium"),
    ("client/package.json", "e2e.sh --no-orphans", "e2e.sh", "no-orphans"),
    ("certs/dev.txt", None, "local development certificate", "TLS certificate/key paths"),
    ("notes.txt", None, "-----BEGIN " + "PRIVATE KEY-----", "PEM"),
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


if __name__ == "__main__":
    unittest.main()
