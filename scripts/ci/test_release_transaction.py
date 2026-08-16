#!/usr/bin/env python3
"""Regression tests for the privileged stable GitHub Release transaction.

The publication job intentionally stays checkout-free and shell-only. These tests
exercise its embedded shell helpers from the unprivileged CI layer with a fake
`gh` implementation so GitHub API convergence and permission failures remain
covered without adding repository code execution to the privileged runner.
"""
from __future__ import annotations

import pathlib
import re
import subprocess
import textwrap
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "_publish-release.yml"


class ReleaseTransactionTests(unittest.TestCase):
    def _helpers(self) -> str:
        text = WORKFLOW.read_text(encoding="utf-8")
        start = text.index("          resolve_tag_target() {")
        end = text.index(
            "          # A pre-existing tag is acceptable only when it already names the exact source SHA."
        )
        return textwrap.dedent(text[start:end])

    def _run_helpers(self, body: str) -> subprocess.CompletedProcess[str]:
        script = "set -euo pipefail\n" + self._helpers() + "\n" + body
        return subprocess.run(
            ["bash"],
            input=script,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def test_transaction_structure_preserves_last_mile_boundary(self) -> None:
        text = WORKFLOW.read_text(encoding="utf-8")
        for forbidden in ("actions/checkout@", "uses: ./", "scripts/", "just "):
            self.assertNotIn(forbidden, text)
        for required in (
            "wait_for_tag_target()",
            "wait_for_release_published()",
            'created_ref=$(gh api --method POST "repos/$REPOSITORY/git/refs"',
            'require_tag_target "$created_ref"',
            'wait_for_tag_target "new tag creation"',
            "Reference already exists",
            'published=$(wait_for_release_published "post-publish reconciliation")',
            'wait_for_tag_target "post-publish verification"',
        ):
            self.assertIn(required, text)
        self.assertLess(
            text.index('wait_for_tag_target "new tag creation"'),
            text.index('{"draft":false,"prerelease":false,"make_latest":"legacy"}'),
        )

    def test_contents_write_is_confined_to_stable_release_publication(self) -> None:
        writers = {
            path.name
            for path in (ROOT / ".github" / "workflows").glob("*.yml")
            if re.search(
                r"(?m)^\s+contents:\s*write\s*$",
                path.read_text(encoding="utf-8"),
            )
        }
        self.assertEqual(writers, {"release.yml", "_publish-release.yml"})

    def test_tag_creation_waits_for_read_after_write_visibility(self) -> None:
        sha = "a" * 40
        result = self._run_helpers(
            f'''
TAG=v9.8.7
TARGET_SHA={sha}
REPOSITORY=example/repo
td=$(mktemp -d)
trap 'rm -rf "$td"' EXIT
err="$td/err"
created="$td/created"
reads="$td/reads"
printf '0\n' >"$reads"
sleep() {{ :; }}
gh() {{
  if [[ "$*" == *"--method POST"* && "$*" == *"repos/$REPOSITORY/git/refs"* ]]; then
    touch "$created"
    printf '%s\n' '{{"ref":"refs/tags/v9.8.7","object":{{"type":"commit","sha":"{sha}"}}}}'
    return 0
  fi
  if [[ "$*" == "api repos/$REPOSITORY/git/ref/tags/$TAG" ]]; then
    if [[ ! -f "$created" ]]; then
      echo 'gh: Not Found (HTTP 404)' >&2
      return 1
    fi
    n=$(cat "$reads")
    n=$((n + 1))
    printf '%s\n' "$n" >"$reads"
    if (( n < 3 )); then
      echo 'gh: Not Found (HTTP 404)' >&2
      return 1
    fi
    printf '%s\n' '{{"ref":"refs/tags/v9.8.7","object":{{"type":"commit","sha":"{sha}"}}}}'
    return 0
  fi
  echo "unexpected gh invocation: $*" >&2
  return 97
}}
ensure_tag_target
[[ $(cat "$reads") == 3 ]]
'''
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("tag creation accepted; waiting for read visibility", result.stderr)
        self.assertIn("is visible at the exact target", result.stderr)

    def test_non_race_422_is_not_hidden_by_retry_logic(self) -> None:
        sha = "b" * 40
        result = self._run_helpers(
            f'''
TAG=v9.8.8
TARGET_SHA={sha}
REPOSITORY=example/repo
td=$(mktemp -d)
trap 'rm -rf "$td"' EXIT
err="$td/err"
reads="$td/reads"
printf '0\n' >"$reads"
sleep() {{ :; }}
gh() {{
  if [[ "$*" == "api repos/$REPOSITORY/git/ref/tags/$TAG" ]]; then
    n=$(cat "$reads")
    printf '%s\n' "$((n + 1))" >"$reads"
    echo 'gh: Not Found (HTTP 404)' >&2
    return 1
  fi
  if [[ "$*" == *"--method POST"* && "$*" == *"repos/$REPOSITORY/git/refs"* ]]; then
    echo 'gh: Reference update failed (HTTP 422)' >&2
    return 1
  fi
  echo "unexpected gh invocation: $*" >&2
  return 97
}}
if ensure_tag_target; then
  echo 'unexpected success' >&2
  exit 98
fi
[[ $(cat "$reads") == 1 ]]
'''
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("GitHub rejected creation", result.stderr)
        self.assertIn("Reference update failed", result.stderr)

    def test_create_conflict_accepts_only_exact_converged_winner(self) -> None:
        sha = "c" * 40
        result = self._run_helpers(
            f'''
TAG=v9.8.9
TARGET_SHA={sha}
REPOSITORY=example/repo
td=$(mktemp -d)
trap 'rm -rf "$td"' EXIT
err="$td/err"
created="$td/created"
reads="$td/reads"
printf '0\n' >"$reads"
sleep() {{ :; }}
gh() {{
  if [[ "$*" == *"--method POST"* && "$*" == *"repos/$REPOSITORY/git/refs"* ]]; then
    touch "$created"
    echo 'gh: Reference already exists (HTTP 422)' >&2
    return 1
  fi
  if [[ "$*" == "api repos/$REPOSITORY/git/ref/tags/$TAG" ]]; then
    if [[ ! -f "$created" ]]; then
      echo 'gh: Not Found (HTTP 404)' >&2
      return 1
    fi
    n=$(cat "$reads")
    n=$((n + 1))
    printf '%s\n' "$n" >"$reads"
    if (( n < 2 )); then
      echo 'gh: Not Found (HTTP 404)' >&2
      return 1
    fi
    printf '%s\n' '{{"ref":"refs/tags/v9.8.9","object":{{"type":"commit","sha":"{sha}"}}}}'
    return 0
  fi
  echo "unexpected gh invocation: $*" >&2
  return 97
}}
ensure_tag_target
'''
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("creation raced with another writer", result.stderr)

    def test_publication_waits_for_state_convergence(self) -> None:
        result = self._run_helpers(
            '''
TAG=v9.9.0
TARGET_SHA=dddddddddddddddddddddddddddddddddddddddd
REPOSITORY=example/repo
release_id=12345
td=$(mktemp -d)
trap 'rm -rf "$td"' EXIT
err="$td/err"
reads="$td/reads"
printf '0\n' >"$reads"
sleep() { :; }
gh() {
  if [[ "$*" == "api repos/$REPOSITORY/releases/$release_id" ]]; then
    n=$(cat "$reads")
    n=$((n + 1))
    printf '%s\n' "$n" >"$reads"
    if (( n < 3 )); then
      printf '%s\n' '{"tag_name":"v9.9.0","draft":true,"prerelease":false}'
    else
      printf '%s\n' '{"tag_name":"v9.9.0","draft":false,"prerelease":false}'
    fi
    return 0
  fi
  echo "unexpected gh invocation: $*" >&2
  return 97
}
published=$(wait_for_release_published "test convergence")
[[ $(jq -r '.draft' <<<"$published") == false ]]
[[ $(cat "$reads") == 3 ]]
'''
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("release publication visibility", result.stderr)


if __name__ == "__main__":
    unittest.main()
