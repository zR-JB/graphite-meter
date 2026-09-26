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
SHA_ENV = "EVENT_SHA: ${{ github.sha }}"
MUTATIONS: tuple[tuple[str, str | None, str, str], ...] = (
    (W + "ci.yml", CHECKOUT, "uses: actions/checkout@v7", "40-character commit SHA"),
    (OCI, None, "# ${{ secrets.TOKEN }}\n", r"secrets\."),
    (W + "ci.yml", "runs-on: ubuntu-24.04", "runs-on: ubuntu-latest", "ubuntu-latest"),
    (W + "release.yml", 'run: echo "::notice::', 'run: echo "${{ github.head_ref }}', "through env"),
    (OCI, "        bun=$(", "        echo ${{ inputs.version }}\n        bun=$(", "through env"),
    (W + "ci.yml", None, PINNED_STEP.format("actions/setup-go"), "through mise"),
    (W + "ci.yml", "persist-credentials: false", "fetch-depth: 1", "persist-credentials"),
    (W + "prerelease-request.yml", "ref: ${{ github.sha }}", "ref: ${{ inputs.sha }}",
     "triggering github.sha"),
    (W + "prerelease-publish.yml", "cache: false", "cache: true", "cache: false"),
    (".github/actions/setup-project/action.yml", "install_args:", "args:", "install_args"),
    (OCI, "buildkitd-flags: --log-level=info",
     "buildkitd-flags: --allow-insecure-entitlement network.host", "insecure-entitlement"),
    (W + "release.yml", "target_sha: ${{ github.sha }}",
     "target_sha: ${{ needs.guard.outputs.sha }}", "needs.guard.outputs.sha"),
    (W + "_publish-release.yml", "/releases?per_page=100", "/releases/tags/$TAG", "releases/tags"),
    (W + "_publish-oci.yml", 'gh api "repos/$REPOSITORY/commits/main"', "echo skip",
     "commits/main"),
    (W + "_publish-oci.yml", "-${{ inputs.tag }}", "", "group: publish-oci"),
    (OCI, "provenance: mode=max", "provenance: false", "provenance"),
    (W + "release.yml", "      - name: Generate", "      - uses: ./.github/actions/build-oci\n"
     "      - name: Generate", "build-oci"),
    (W + "prerelease-request.yml", "if: ${{ github.ref == format(", "if: ${{ true || (",
     "default_branch"),
    (W + "prerelease-publish.yml", "environment: ghcr-release", "", "environment"),
    (W + "release.yml", "on:\n", "on:\n  push:\n    tags: ['v*']\n", "triggered only by"),
    (W + "release.yml", "on:\n", "on:\n  workflow_dispatch:\n", "triggered only by"),
    (W + "extra.yml", None, "on:\n  push:\n", "unreviewed workflow set"),
    (W + "ci.yml", "permissions:\n  contents: read\n\nenv:", "env:", "top-level permissions"),
    (W + "prerelease-request.yml", "  contents: read", "  contents: write", "write permission"),
    (W + "release-request.yml", "    steps:\n",
     "    steps:" + PINNED_STEP.format("actions/checkout"), "repository code"),
    (W + "prerelease-request.yml", "./.github/actions/build-oci", "./source/build-oci",
     "repository code"),
    (W + "prerelease-request.yml", SHA_ENV, SHA_ENV + "\n          RAW: ${{ inputs.sha }}",
     "raw SHA"),
    (W + "prerelease-request.yml", "      - id: request\n",
     "      - run: mise run release-build\n      - id: request\n", "request helpers"),
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
