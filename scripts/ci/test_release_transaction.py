#!/usr/bin/env python3
"""Run the publication shell against fake gh, Docker and Skopeo."""
from __future__ import annotations

import copy
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from collections.abc import Callable
from typing import Any

SCRIPT = pathlib.Path(__file__).resolve().parent / "publish.sh"
VERIFIED = "sha256:" + "a" * 64
OTHER = "sha256:" + "b" * 64
# Skopeo keeps registry tags as TAG=DIGEST lines in $TAGS.
SKOPEO = """#!/bin/sh
case "$1 $4" in
  "copy oci-archive:"*) echo "${5##*:}=${PUSHED:-$DIGEST}" >>"$TAGS" ;;
  "copy "*) echo "${5##*:}=${4##*@}" >>"$TAGS" ;;
  "inspect oci-archive:"*) echo "${ARCHIVE:-$DIGEST}" ;;
  inspect*)
    digest=$(grep "^${4##*:}=" "$TAGS" | tail -n1 | cut -d= -f2)
    [ -n "$digest" ] || { echo "${LOOKUP:-manifest unknown}" >&2; exit 1; }
    echo "$digest" ;;
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
TAG, SHA, OTHER_SHA = "v1.2.3", "d" * 40, "e" * 40
SOURCE = "graphite-meter_1.2.3_third-party-source.tar.gz"
ASSETS = {SOURCE: b"third-party source", "checksums.txt": b"checksums"}
# GitHub's tags, annotated tag objects, releases and assets for `gh api` and upload `curl`.
GITHUB = r"""import hashlib, json, os, subprocess, sys
from pathlib import Path
state_path = Path(os.environ["GH_STATE"])
state = json.loads(state_path.read_text())


def fail(message):
    state_path.write_text(json.dumps(state))
    sys.exit(f"gh: {message}")


def release(rid):
    return next((item for item in state["releases"] if item["id"] == rid), None) or fail(
        "Not Found (HTTP 404)")


def public(item):
    return {key: value for key, value in item.items() if key != "assets"}


if Path(sys.argv[0]).name == "curl":
    url, args = sys.argv[-1], sys.argv[1:]
    if os.environ["GH_TOKEN"] in " ".join(args):
        fail("token in curl argv")
    payload = Path(args[args.index("--data-binary") + 1][1:]).read_bytes()
    name = url.split("?name=")[1]
    state["next"] += 1
    release(int(url.split("/releases/")[1].split("/")[0]))["assets"].append(
        {"id": state["next"], "name": name,
         "digest": "sha256:" + hashlib.sha256(payload + state["tamper"].encode()).hexdigest()})
    state["writes"].append(f"upload {name}")
    state_path.write_text(json.dumps(state))
    sys.exit()
flags, words = {}, iter(sys.argv[2:])
for word in words:
    flags[word if word.startswith("--") else "path"] = (
        next(words) if word in ("--method", "--input", "--jq") else word)
method = flags.get("--method", "GET")
path = flags["path"].removeprefix(f"repos/{os.environ['REPOSITORY']}/")
body = json.loads(sys.stdin.read()) if flags.get("--input") == "-" else None
if method != "GET":
    state["writes"].append(f"{method} {path}")
tags, parts = state["tags"], path.split("?")[0].split("/")
if path.startswith("git/ref/tags/"):
    tag = parts[-1]
    out = {"ref": f"refs/tags/{tag}", "object": tags[tag]} if tag in tags else fail(
        "Not Found (HTTP 404)")
elif path.startswith("git/tags/"):
    out = {"object": state["annotated"][parts[-1]]}
elif path == "git/refs":
    tag = body["ref"].removeprefix("refs/tags/")
    if state["race"]:
        tags[tag] = {"type": "commit", "sha": state["race"]}
    if tag in tags:
        fail("Reference already exists (HTTP 422)")
    tags[tag] = {"type": "commit", "sha": body["sha"]}
    out = {"ref": body["ref"], "object": tags[tag]}
elif path == "releases?per_page=100":
    out = [public(item) for item in state["releases"]]
elif path == "releases":
    state["next"] += 1
    upload = f"https://uploads.github.com/{flags['path']}/{state['next']}/assets{{?name}}"
    out = {key: body[key] for key in ("tag_name", "target_commitish", "draft", "prerelease",
                                      "body")} | {"id": state["next"], "assets": [],
                                                  "upload_url": upload}
    state["releases"].append(out)
    out = public(out)
elif parts[:2] == ["releases", "assets"]:
    for item in state["releases"]:
        item["assets"] = [asset for asset in item["assets"] if asset["id"] != int(parts[2])]
    out = None
elif len(parts) == 3:
    out = release(int(parts[1]))["assets"]
else:
    item = release(int(parts[1]))
    if method == "PATCH":
        item |= {key: value for key, value in body.items() if key != "make_latest"}
        if not item["draft"]:
            tags.setdefault(item["tag_name"], {"type": "commit", "sha": item["target_commitish"]})
    out = public(item)
state_path.write_text(json.dumps(state))
text = "" if out is None else json.dumps([out] if "--slurp" in flags else out)
if "--jq" in flags:
    text = subprocess.run(["jq", "-r", flags["--jq"]], input=text, capture_output=True,
                          text=True, check=True).stdout
print(text, end="")
"""
State = dict[str, Any]
EMPTY: State = {"tags": {}, "annotated": {}, "releases": [], "race": "", "tamper": "", "writes": [],
         "next": 100}


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
    def run_script(self, command: str, registry: dict[str, str], releases: str = "v1.2.3",
                   **fakes: str) -> tuple[int, str, dict[str, str]]:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "skopeo").write_text(SKOPEO)
            (root / "skopeo").chmod(0o755)
            tags = root / "tags"
            (root / "docker.log").touch()
            tags.write_text("".join(f"{tag}={digest}\n" for tag, digest in registry.items()))
            env = os.environ | {
                "PATH": f"{directory}{os.pathsep}{os.environ['PATH']}", "TAGS": str(tags),
                "DOCKER_LOG": str(root / "docker.log"), "DIGEST": VERIFIED,
                "REGISTRY_TOKEN": "secret-token", "REPOSITORY": "Owner/Repo",
                "REGISTRY_ACTOR": "owner", "VERSION": "1.2.3", "IMAGE_TAG": "1.2.3",
                "ARCHIVE_DIR": directory, "SKOPEO_IMAGE": "skopeo", "RELEASES": releases,
            } | fakes
            result = subprocess.run(["bash", "-c", f"{SHIM}source {SCRIPT} {command}"], env=env,
                                    capture_output=True, text=True)
            self.assertNotIn("secret-token", (root / "docker.log").read_text())
            state = dict(line.split("=", 1) for line in tags.read_text().splitlines())
            return result.returncode, result.stdout + result.stderr, state

    def test_image_publication_is_exact_and_idempotent(self) -> None:
        for registry, fakes, error in (
            ({}, {}, None), ({"1.2.3": VERIFIED}, {}, None),
            ({"1.2.3": OTHER}, {}, "already exists at " + OTHER),
            ({}, {"ARCHIVE": OTHER}, "archive is not the verified"),
            ({}, {"LOOKUP": "unauthorized: authentication required"}, "lookup failed"),
            ({}, {"PUSHED": OTHER}, "does not match"),
        ):
            with self.subTest(registry=registry, fakes=fakes):
                status, output, tags = self.run_script("image", registry, **fakes)
                if error is None:
                    self.assertEqual((status, tags), (0, {"1.2.3": VERIFIED}), output)
                else:
                    self.assertNotEqual(status, 0)
                    self.assertIn(error, output)
                    self.assertNotEqual(tags.get("1.2.3"), VERIFIED)

    def test_aliases_follow_the_highest_published_releases(self) -> None:
        for releases, series, latest in (
            ("v1.1.9 v1.2.3", VERIFIED, VERIFIED),
            ("v1.2.3 v1.10.0", VERIFIED, OTHER),
            ("v1.2.3 v1.2.4", OTHER, OTHER),
        ):
            registry = {"1.2.3": VERIFIED, "1.2.4": OTHER, "1.10.0": OTHER, "1.1.9": OTHER}
            with self.subTest(releases=releases):
                status, output, tags = self.run_script("aliases", registry, releases)
                self.assertEqual(status, 0, output)
                self.assertEqual((tags["1.2"], tags["latest"]), (series, latest))

    def test_a_moved_or_unreleased_version_stops_promotion(self) -> None:
        for registry, releases, error in (({"1.2.3": OTHER}, "v1.2.3", "not the verified"),
                                          ({"1.2.3": VERIFIED}, "v1.2.2", "not a published")):
            with self.subTest(error=error):
                status, output, tags = self.run_script("aliases", registry, releases)
                self.assertNotEqual(status, 0)
                self.assertIn(error, output)
                self.assertNotIn("latest", tags)


class ReleasePublicationTests(unittest.TestCase):
    def publish(self, state: State) -> tuple[int, str, State]:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "bin").mkdir()
            (root / "bin/gh").write_text(f"#!{sys.executable} -IS\n{GITHUB}")
            (root / "bin/sleep").write_text("#!/bin/sh\n")
            for name in ("gh", "sleep"):
                (root / "bin" / name).chmod(0o755)
            (root / "bin/curl").symlink_to(root / "bin/gh")
            (root / "assets").mkdir()
            for name, payload in ASSETS.items():
                (root / "assets" / name).write_bytes(payload)
            (root / "state.json").write_text(json.dumps(state))
            env = os.environ | {
                "PATH": f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                "GH_STATE": str(root / "state.json"), "GH_TOKEN": "secret-token",
                "REPOSITORY": "owner/repo", "TAG": TAG, "TARGET_SHA": SHA,
                "ASSETS_DIR": str(root / "assets"),
            }
            result = subprocess.run(["bash", str(SCRIPT), "release"], env=env, text=True,
                                    capture_output=True)
            after = json.loads((root / "state.json").read_text())
            return result.returncode, result.stdout + result.stderr, after

    def test_release_publishes_only_exact_assets_at_the_exact_tag(self) -> None:
        status, output, published = self.publish(copy.deepcopy(EMPTY))
        self.assertEqual(status, 0, output)
        release = published["releases"][0]
        self.assertEqual((release["draft"], release["prerelease"]), (False, False))
        self.assertEqual(published["tags"], {TAG: {"type": "commit", "sha": SHA}})
        self.assertEqual(
            sorted((asset["name"], asset["digest"]) for asset in release["assets"]),
            sorted((name, "sha256:" + hashlib.sha256(data).hexdigest())
                   for name, data in ASSETS.items()))
        # The notice names what the third-party source archive's README points to.
        for phrase in ("**Source code (zip)**", "**Source code (tar.gz)**", SOURCE, SHA):
            self.assertIn(phrase, release["body"])

        def edit(**change: object) -> Callable[[State], State]:
            return lambda state: state | {"releases": [state["releases"][0] | change]}

        def stale_asset(state: State) -> State:
            extra = {"id": 1, "name": "old", "digest": ""}
            return edit(draft=True, assets=[*state["releases"][0]["assets"], extra])(state)

        def same(state: State) -> State:
            return state

        other_tag = {"tags": {TAG: {"type": "commit", "sha": OTHER_SHA}}}
        annotated = {"tags": {TAG: {"type": "tag", "sha": OTHER_SHA}},
                     "annotated": {OTHER_SHA: {"type": "commit", "sha": SHA}}}
        tampered = edit(assets=[{"name": name, "digest": "sha256:0"} for name in ASSETS])
        rows: tuple[tuple[State, Callable[[State], State], str | None, bool], ...] = (
            (published, same, "is already published", True),
            (published, stale_asset, None, False),
            (EMPTY, lambda state: state | annotated, None, False),
            (EMPTY, lambda state: state | {"race": SHA}, None, False),
            (published, edit(prerelease=True), "already exists as a prerelease", True),
            (published, tampered, "published but asset names/digests differ", True),
            (published, edit(body="notes"), "notice is missing or stale", True),
            (published, lambda state: state | other_tag, f"resolves to {OTHER_SHA}", True),
            (EMPTY, lambda state: state | other_tag, f"resolves to {OTHER_SHA}", True),
            (EMPTY, lambda state: state | {"race": OTHER_SHA}, f"resolves to {OTHER_SHA}", False),
            (EMPTY, lambda state: state | {"tamper": "x"}, "draft release asset names", False),
            (published, edit(draft=True, body="## Source availability\nold"),
             "stale source-availability notice", True),
        )
        for base, change, expected, read_only in rows:
            before: State = change(copy.deepcopy(base))
            with self.subTest(expected=expected, before=before):
                status, output, after = self.publish(before)
                if read_only:
                    self.assertEqual(after["writes"], before["writes"])
                if expected is None or expected == "is already published":
                    self.assertEqual(status, 0, output)
                    self.assertIn(expected or "published v1.2.3", output)
                    self.assertEqual(after["tags"], before["tags"] or {
                        TAG: {"type": "commit", "sha": SHA}})
                    assets = after["releases"][0]["assets"]
                    self.assertEqual(sorted(asset["name"] for asset in assets), sorted(ASSETS))
                    continue
                self.assertNotEqual(status, 0)
                self.assertIn(expected, output)
                self.assertEqual([item["id"] for item in after["releases"] if not item["draft"]],
                                 [item["id"] for item in before["releases"] if not item["draft"]])


if __name__ == "__main__":
    unittest.main()
