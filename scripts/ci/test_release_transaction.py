#!/usr/bin/env python3
"""Run the publication shell against fake gh, Docker and Skopeo."""
from __future__ import annotations

import os
import pathlib
import subprocess
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent / "publish.sh"
VERIFIED = "sha256:" + "a" * 64
MOVED = "sha256:" + "b" * 64
SKOPEO = """#!/bin/sh
echo "$*" >>"$SKOPEO_LOG"
case "$1 $4" in
  "copy "*) echo "VERSION_DIGEST=$DIGEST" >"$SKOPEO_LOG.state" ;;
  "inspect docker://"*:1.2.3)
    [ ! -f "$SKOPEO_LOG.state" ] || . "$SKOPEO_LOG.state"
    [ -n "$VERSION_DIGEST" ] || { echo "manifest unknown" >&2; exit 1; }
    echo "$VERSION_DIGEST" ;;
  inspect*) echo "$DIGEST" ;;
esac
"""
# Docker runs the in-container script on the host and records its argv.
SHIM = """docker() {
  echo "$*" >>"$DOCKER_LOG"
  while [ "$1" != -ec ]; do [ "$1" = -e ] && export "$2"; shift; done
  sh -ec "$2"
}
gh() { printf '%s\\n' $RELEASES; }
"""


class ReleaseTransactionTests(unittest.TestCase):
    def _run_helpers(self, body: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(["bash"], input=f"source {SCRIPT}\n{body}", text=True,
                              capture_output=True, check=False)

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


class RegistryTests(unittest.TestCase):
    def run_script(self, command: str, version_digest: str = "",
                   releases: str = "v1.2.3") -> tuple[int, str, str]:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "skopeo").write_text(SKOPEO)
            (root / "skopeo").chmod(0o755)
            env = os.environ | {
                "PATH": f"{directory}{os.pathsep}{os.environ['PATH']}",
                "SKOPEO_LOG": str(root / "skopeo.log"), "DOCKER_LOG": str(root / "docker.log"),
                "DIGEST": VERIFIED, "VERSION_DIGEST": version_digest,
                "REGISTRY_TOKEN": "secret-token", "REPOSITORY": "Owner/Repo",
                "REGISTRY_ACTOR": "owner", "VERSION": "1.2.3", "IMAGE_TAG": "1.2.3",
                "ARCHIVE_DIR": directory, "SKOPEO_IMAGE": "skopeo", "RELEASES": releases,
            }
            result = subprocess.run(["bash", "-c", f"{SHIM}source {SCRIPT} {command}"], env=env,
                                    capture_output=True, text=True)
            self.assertNotIn("secret-token", (root / "docker.log").read_text())
            log = (root / "skopeo.log").read_text()
            return result.returncode, result.stdout + result.stderr, log

    def test_image_publication_is_exact_and_idempotent(self) -> None:
        status, output, log = self.run_script("image")
        self.assertEqual(status, 0, output)
        self.assertIn("copy --all --preserve-digests oci-archive:/work/graphite-meter.oci.tar "
                      "docker://ghcr.io/owner/repo:1.2.3", log)
        status, output, log = self.run_script("image", VERIFIED)
        self.assertEqual((status, "copy" in log), (0, False), output)
        status, output, _ = self.run_script("image", MOVED)
        self.assertNotEqual(status, 0)
        self.assertIn("already exists", output)

    def test_aliases_copy_only_the_verified_digest(self) -> None:
        status, output, log = self.run_script("aliases", VERIFIED, "v1.1.9 v1.2.3")
        self.assertEqual(status, 0, output)
        for alias in ("1.2", "latest"):
            self.assertIn(f"copy --all --preserve-digests docker://ghcr.io/owner/repo@{VERIFIED} "
                          f"docker://ghcr.io/owner/repo:{alias}", log)

    def test_a_moved_version_tag_stops_promotion(self) -> None:
        status, output, log = self.run_script("aliases", MOVED)
        self.assertNotEqual(status, 0)
        self.assertIn("not the verified", output)
        self.assertNotIn("copy", log)

    def test_an_older_series_is_promoted_without_latest(self) -> None:
        status, output, log = self.run_script("aliases", VERIFIED, "v1.2.3 v1.10.0")
        self.assertEqual(status, 0, output)
        self.assertIn("promoted ghcr.io/owner/repo:1.2", output)
        self.assertNotIn(":latest", log)


if __name__ == "__main__":
    unittest.main()
