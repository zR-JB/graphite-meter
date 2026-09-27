#!/usr/bin/env python3
"""Run the publication against fake gh, Docker and Skopeo."""
from __future__ import annotations

import contextlib
import copy
import hashlib
import io
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from collections.abc import Callable
from typing import Any
from unittest.mock import patch

from github_api import ControlPlaneError
from release import command_publish

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
# GitHub's tags, annotated tags, releases and uploads behind `gh api`. A write becomes readable
# only after `lag` reads of a new tag or `publish_lag` reads of a published release; a publish
# request can fail after it `commit`s or `drop`s the change.
GITHUB = r"""import hashlib, json, os, sys
from pathlib import Path
from urllib.parse import unquote
state_path = Path(os.environ["GH_STATE"])
state = json.loads(state_path.read_text())
flags, words = {}, iter(sys.argv[2:])
for word in words:
    flags[word if word.startswith("--") else "path"] = (
        next(words) if word in ("--method", "--input", "--header") else word)


def finish(out=None, error=""):
    state_path.write_text(json.dumps(state))
    if error:
        sys.exit(f"gh: {error}")
    print("" if out is None else json.dumps([out] if "--slurp" in flags else out), end="")
    sys.exit()


def release(rid):
    item = next((item for item in state["releases"] if item["id"] == rid), None)
    return item or finish(error="Not Found (HTTP 404)")


def public(item):
    return {key: value for key, value in item.items() if key not in ("assets", "lag")}


if os.environ["GH_TOKEN"] in " ".join(sys.argv):
    finish(error="token in argv")
method = flags.get("--method", "GET")
path = flags["path"].split("github.com/", 1)[-1].removeprefix(f"repos/{os.environ['REPOSITORY']}/")
if method != "GET":
    state["writes"].append(f"{method} {path}")
tags, parts = state["tags"], path.split("?")[0].split("/")
if "assets?name=" in path:
    payload = Path(flags["--input"]).read_bytes() + state["tamper"].encode()
    state["next"] += 1
    release(int(parts[1]))["assets"].append({"id": state["next"], "name": unquote(path.split("=")[1]),
                                             "digest": "sha256:" + hashlib.sha256(payload).hexdigest()})
    finish({})
body = json.loads(sys.stdin.read()) if flags.get("--input") == "-" else None
if path.startswith("git/matching-refs/tags/"):
    visible = []
    for tag, target in tags.items():
        if state["hidden"].get(tag, 0) > 0:
            state["hidden"][tag] -= 1
        elif tag.startswith(parts[-1]):
            visible.append({"ref": f"refs/tags/{tag}", "object": target})
    finish(visible)
if path.startswith("git/tags/"):
    finish({"object": state["annotated"][parts[-1]]})
if path == "git/refs":
    tag = body["ref"].removeprefix("refs/tags/")
    if state["refs_error"]:
        finish(error=state["refs_error"])
    if state["race"]:
        tags[tag] = {"type": "commit", "sha": state["race"]}
    if tag in tags:
        finish(error="Reference already exists (HTTP 422)")
    tags[tag], state["hidden"][tag] = {"type": "commit", "sha": body["sha"]}, state["lag"]
    finish({"ref": body["ref"], "object": tags[tag]})
if path == "releases?per_page=100":
    finish([public(item) for item in state["releases"]])
if path == "releases":
    state["next"] += 1
    upload = f"https://uploads.github.com/{flags['path']}/{state['next']}/assets{{?name}}"
    keys = ("tag_name", "target_commitish", "draft", "prerelease", "body")
    item = {key: body[key] for key in keys} | {"id": state["next"], "assets": [], "upload_url": upload}
    state["releases"].append(item)
    finish(public(item))
if parts[:2] == ["releases", "assets"]:
    for item in state["releases"]:
        item["assets"] = [asset for asset in item["assets"] if asset["id"] != int(parts[2])]
    finish()
if len(parts) == 3:
    finish(release(int(parts[1]))["assets"])
item = release(int(parts[1]))
if method == "PATCH":
    if body.get("draft") is False and state["patch_error"] == "drop":
        finish(error="Bad Gateway (HTTP 502)")
    item |= {key: value for key, value in body.items() if key != "make_latest"}
    if not item["draft"]:
        tags.setdefault(item["tag_name"], {"type": "commit", "sha": item["target_commitish"]})
        item["lag"] = state["publish_lag"]
        if state["patch_error"] == "commit":
            finish(error="Bad Gateway (HTTP 502)")
elif item.get("lag"):
    item["lag"] -= 1
    finish(public(item) | {"draft": True})
finish(public(item))
"""
State = dict[str, Any]
EMPTY: State = {"tags": {}, "annotated": {}, "releases": [], "race": "", "tamper": "", "writes": [],
                "next": 100, "hidden": {}, "lag": 0, "publish_lag": 0, "refs_error": "",
                "patch_error": ""}


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
    def publish(self, state: State, assets: dict[str, bytes] = ASSETS) -> tuple[str | None, str, State]:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "bin").mkdir()
            (root / "bin/gh").write_text(f"#!{sys.executable} -IS\n{GITHUB}")
            (root / "bin/gh").chmod(0o755)
            (root / "assets").mkdir()
            for name, payload in assets.items():
                (root / "assets" / name).write_bytes(payload)
            (root / "state.json").write_text(json.dumps(state))
            env = {
                "PATH": f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                "GH_STATE": str(root / "state.json"), "GH_TOKEN": "secret-token",
                "REPOSITORY": "owner/repo", "TAG": TAG, "TARGET_SHA": SHA,
                "ASSETS_DIR": str(root / "assets"), "RUNNER_TEMP": directory,
            }
            output, error = io.StringIO(), None
            with (patch.dict(os.environ, env), patch("release.time.sleep"),
                  contextlib.redirect_stdout(output), contextlib.redirect_stderr(output)):
                try:
                    command_publish()
                except ControlPlaneError as exc:
                    error = str(exc)
            return error, output.getvalue(), json.loads((root / "state.json").read_text())

    def test_release_without_the_third_party_source_offer_is_refused(self) -> None:
        error, _, after = self.publish(copy.deepcopy(EMPTY), {"checksums.txt": b"checksums"})
        self.assertIn(f"missing the third-party source asset {SOURCE}", error or "")
        self.assertEqual(after["writes"], [])

    def test_release_publishes_only_exact_assets_at_the_exact_tag(self) -> None:
        error, output, published = self.publish(copy.deepcopy(EMPTY))
        self.assertIsNone(error, output)
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
        rejected = {"refs_error": "Reference update failed (HTTP 422)"}
        rows: tuple[tuple[State, Callable[[State], State], str | None, bool], ...] = (
            (published, same, "is already published", True),
            (published, stale_asset, None, False),
            (EMPTY, lambda state: state | annotated, None, False),
            (EMPTY, lambda state: state | {"race": SHA}, None, False),
            (EMPTY, lambda state: state | {"lag": 2, "publish_lag": 2}, None, False),
            (EMPTY, lambda state: state | {"patch_error": "commit"}, None, False),
            (EMPTY, lambda state: state | {"patch_error": "drop"}, "publication did not become", False),
            (published, edit(draft=True, upload_url="https://example.invalid/upload{?name}"),
             "unexpected release upload URL", False),
            (published, edit(prerelease=True), "already exists as a prerelease", True),
            (published, tampered, "published but asset names/digests differ", True),
            (published, edit(body="notes"), "notice is missing or stale", True),
            (published, lambda state: state | other_tag, f"already exists at {OTHER_SHA}", True),
            (EMPTY, lambda state: state | other_tag, f"already exists at {OTHER_SHA}", True),
            (EMPTY, lambda state: state | {"race": OTHER_SHA}, f"resolves to {OTHER_SHA}", False),
            (EMPTY, lambda state: state | rejected, "GitHub rejected creation", False),
            (EMPTY, lambda state: state | {"tamper": "x"}, "draft release asset names", False),
            (published, edit(draft=True, body="## Source availability\nold"),
             "stale source-availability notice", True),
        )
        for base, change, expected, read_only in rows:
            before: State = change(copy.deepcopy(base))
            with self.subTest(expected=expected, before=before):
                error, output, after = self.publish(before)
                if read_only:
                    self.assertEqual(after["writes"], before["writes"])
                if expected is None or expected == "is already published":
                    self.assertIsNone(error, output)
                    self.assertIn(expected or "published v1.2.3", output)
                    self.assertEqual(after["tags"], before["tags"] or {
                        TAG: {"type": "commit", "sha": SHA}})
                    assets = after["releases"][0]["assets"]
                    self.assertEqual(sorted(asset["name"] for asset in assets), sorted(ASSETS))
                    continue
                self.assertIn(expected, error or "")
                self.assertEqual([item["id"] for item in after["releases"] if not item["draft"]],
                                 [item["id"] for item in before["releases"] if not item["draft"]])

if __name__ == "__main__":
    unittest.main()
