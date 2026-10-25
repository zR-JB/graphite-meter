"""Run the publication against a fake `gh api`, Docker and Skopeo."""

import contextlib
import copy
import hashlib
import io
import json
import os
import pathlib
import subprocess
import tempfile
import unittest
from collections.abc import Callable
from typing import Any
from unittest.mock import patch
from urllib.parse import unquote

from assets import release_assets
from github_api import ControlPlaneError, api
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
# The first $LIST_FAIL reads fail after one page and the first $LIST_LAG show nothing, as GitHub's can after a publish.
gh() {
  reads=$(( $(cat "$TAGS.reads" 2>/dev/null || echo 0) + 1 )); echo "$reads" >"$TAGS.reads"
  [ "$reads" -le "${LIST_FAIL:-0}" ] && { echo "${RELEASES%% *}"; echo "gh: Bad Gateway (HTTP 502)" >&2; return 1; }
  [ "$reads" -gt "${LIST_LAG:-0}" ] && printf '%s\\n' $RELEASES
  return 0
}
sleep() { :; }
"""
TAG, SHA, OTHER_SHA = "v1.2.3", "d" * 40, "e" * 40
SOURCE = "graphite-meter_1.2.3_third-party-source.tar.gz"
ASSETS = {name: name.encode() for name in (*release_assets("1.2.3"), "checksums.txt")}
State = dict[str, Any]
EMPTY: State = {"tags": {}, "annotated": {}, "releases": [], "race": "", "tamper": "", "writes": [],
                "next": 100, "hidden": {}, "lag": 0, "publish_lag": 0, "refs_error": "",
                "patch_error": "", "flaky": []}


class Refused(Exception):
    pass


def github(state: State, argv: list[str], stdin: str | None) -> object:
    """GitHub's tags, annotated tags, releases and uploads behind one `gh api` call. A write becomes readable only
    after `lag` reads of a new tag or `publish_lag` reads of a published release; a publish request can fail after
    it `commit`s or `drop`s the change; each `flaky` call fails once first."""
    flags: dict[str, str] = {}
    words = iter(argv[2:])
    for word in words:
        flags[word if word.startswith("--") else "path"] = (
            next(words) if word in ("--method", "--input", "--header") else word)

    def release(rid: int) -> State:
        if (item := next((item for item in state["releases"] if item["id"] == rid), None)) is None:
            raise Refused("Not Found (HTTP 404)")
        return item

    def public(item: State) -> State:
        return {key: value for key, value in item.items() if key not in ("assets", "lag")}

    if "secret-token" in " ".join(argv):
        raise Refused("token in argv")
    method = flags.get("--method", "GET")
    path = flags["path"].split("github.com/", 1)[-1]
    path = path[len("repos/owner/repo/"):] if path.lower().startswith("repos/owner/repo/") else path
    if method != "GET":
        state["writes"].append(f"{method} {path}")
    if f"{method} {path}" in state["flaky"]:
        state["flaky"].remove(f"{method} {path}")
        raise Refused("Bad Gateway (HTTP 502)")
    tags, parts = state["tags"], path.split("?")[0].split("/")
    if "assets?name=" in path:
        payload = pathlib.Path(flags["--input"]).read_bytes() + state["tamper"].encode()
        state["next"] += 1
        release(int(parts[1]))["assets"].append({"id": state["next"], "name": unquote(path.split("=")[1]),
                                                 "digest": "sha256:" + hashlib.sha256(payload).hexdigest()})
        return {}
    body: State = json.loads(stdin) if stdin and flags.get("--input") == "-" else {}
    if path.startswith("git/matching-refs/tags/"):
        visible = []
        for tag, target in tags.items():
            if state["hidden"].get(tag, 0) > 0:
                state["hidden"][tag] -= 1
            elif tag.startswith(parts[-1]):
                visible.append({"ref": f"refs/tags/{tag}", "object": target})
        return visible
    if path.startswith("git/tags/"):
        return {"object": state["annotated"][parts[-1]]}
    if path == "git/refs":
        tag = body["ref"].removeprefix("refs/tags/")
        if state["refs_error"]:
            raise Refused(state["refs_error"])
        if state["race"]:
            tags[tag] = {"type": "commit", "sha": state["race"]}
        if tag in tags:
            raise Refused("Reference already exists (HTTP 422)")
        tags[tag], state["hidden"][tag] = {"type": "commit", "sha": body["sha"]}, state["lag"]
        return {"ref": body["ref"], "object": tags[tag]}
    if path == "releases?per_page=100":
        return [public(item) for item in state["releases"]]
    if path == "releases":
        state["next"] += 1
        upload = f"https://uploads.github.com/{flags['path']}/{state['next']}/assets{{?name}}"
        keys = ("tag_name", "target_commitish", "draft", "prerelease", "body")
        item = {key: body[key] for key in keys} | {"id": state["next"], "assets": [], "upload_url": upload}
        state["releases"].append(item)
        return public(item)
    if parts[:2] == ["releases", "assets"]:
        for item in state["releases"]:
            item["assets"] = [asset for asset in item["assets"] if asset["id"] != int(parts[2])]
        return None
    if len(parts) == 3:
        return release(int(parts[1]))["assets"]
    item = release(int(parts[1]))
    if method == "PATCH":
        if body.get("draft") is False and state["patch_error"] == "drop":
            raise Refused("Bad Gateway (HTTP 502)")
        item |= {key: value for key, value in body.items() if key != "make_latest"}
        if not item["draft"]:
            tags.setdefault(item["tag_name"], {"type": "commit", "sha": item["target_commitish"]})
            item["lag"] = state["publish_lag"]
            if state["patch_error"] == "commit":
                raise Refused("Bad Gateway (HTTP 502)")
    elif item.get("lag"):
        item["lag"] -= 1
        return public(item) | {"draft": True}
    return public(item)


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

    def test_the_image_is_tagged_with_its_stable_or_prerelease_version_only(self) -> None:
        for tag, error in (("1.2.3-rc.1", None), ("1.2.3-alpha.0", None), ("1.2.3-rust", "invalid image tag"),
                           ("1.2.3-rc.1-rust", "invalid image tag"), ("1.2.3-preview.1", "invalid image tag"),
                           ("01.2.3", "invalid image tag"), ("latest", "invalid image tag")):
            with self.subTest(tag=tag):
                status, output, tags = self.run_script("image", {}, IMAGE_TAG=tag)
                if error is None:
                    self.assertEqual((status, tags), (0, {tag: VERIFIED}), output)
                else:
                    self.assertNotEqual(status, 0)
                    self.assertIn(error, output)
                    self.assertEqual(tags, {})

    def test_promotion_waits_out_a_lagging_or_failing_release_list(self) -> None:
        for releases, fake, latest in (("v1.2.3", {"LIST_LAG": "2"}, VERIFIED),
                                       ("v1.2.3 v1.2.4", {"LIST_FAIL": "2"}, OTHER)):
            with self.subTest(fake=fake):
                status, output, tags = self.run_script("aliases", {"1.2.3": VERIFIED, "1.2.4": OTHER}, releases,
                                                       **fake)
                self.assertEqual((status, tags.get("latest")), (0, latest), output)

    def test_a_moved_or_unreleased_version_stops_promotion(self) -> None:
        for registry, releases, error in (({"1.2.3": OTHER}, "v1.2.3", "not the verified"),
                                          ({"1.2.3": VERIFIED}, "v1.2.2", "not a published")):
            with self.subTest(error=error):
                status, output, tags = self.run_script("aliases", registry, releases)
                self.assertNotEqual(status, 0)
                self.assertIn(error, output)
                self.assertNotIn("latest", tags)


class ReleasePublicationTests(unittest.TestCase):
    def publish(self, before: State, assets: dict[str, bytes] = ASSETS,
                digest: str = VERIFIED) -> tuple[str | None, str, State]:
        state = copy.deepcopy(before)

        def gh(argv: list[str], *, input: str | None = None, env: dict[str, str] | None = None,
               **_: object) -> subprocess.CompletedProcess[str]:
            self.assertEqual((os.environ if env is None else env).get("GH_TOKEN"), "secret-token")
            try:
                out = github(state, argv, input)
            except Refused as exc:
                return subprocess.CompletedProcess(argv, 1, "", f"gh: {exc}")
            text = "" if out is None else json.dumps([out] if "--slurp" in argv else out)
            return subprocess.CompletedProcess(argv, 0, text, "")

        with tempfile.TemporaryDirectory() as directory:
            files = pathlib.Path(directory) / "assets"
            files.mkdir()
            for name, payload in assets.items():
                (files / name).write_bytes(payload)
            env = {"GH_TOKEN": "secret-token", "REPOSITORY": "Owner/Repo", "TAG": TAG, "TARGET_SHA": SHA,
                   "DIGEST": digest, "ASSETS_DIR": str(files), "RUNNER_TEMP": directory}
            output, error = io.StringIO(), None
            with (patch.dict(os.environ, env), patch("release.time.sleep"), patch("github_api.subprocess.run", gh),
                  contextlib.redirect_stdout(output), contextlib.redirect_stderr(output)):
                try:
                    command_publish()
                except ControlPlaneError as exc:
                    error = str(exc)
            return error, output.getvalue(), state

    def test_a_release_publishes_exactly_the_client_archives_the_source_offer_and_checksums(self) -> None:
        for assets in ({name: data for name, data in ASSETS.items() if name != SOURCE},
                       ASSETS | {"graphite-meter-client_1.2.3_darwin_arm64.tar.gz": b"x"}):
            with self.subTest(assets=sorted(assets)):
                error, _, after = self.publish(EMPTY, assets)
                self.assertIn("release handoff holds", error or "")
                self.assertEqual(after["writes"], [])

    def test_a_release_names_only_a_verified_image_digest(self) -> None:
        for digest in ("sha256:" + "A" * 64, "latest", VERIFIED + "\n"):
            with self.subTest(digest=digest):
                error, _, after = self.publish(EMPTY, digest=digest)
                self.assertIn("invalid verified image digest", error or "")
                self.assertEqual(after["writes"], [])

    def test_release_publishes_only_exact_assets_at_the_exact_tag(self) -> None:
        error, output, published = self.publish(EMPTY)
        self.assertIsNone(error, output)
        release = published["releases"][0]
        self.assertEqual((release["draft"], release["prerelease"]), (False, False))
        self.assertEqual(published["tags"], {TAG: {"type": "commit", "sha": SHA}})
        self.assertEqual(
            sorted((asset["name"], asset["digest"]) for asset in release["assets"]),
            sorted((name, "sha256:" + hashlib.sha256(data).hexdigest())
                   for name, data in ASSETS.items()))
        self.assertTrue(release["body"].startswith(
            f"## Source\n\nThe source for this release is the repository at tag **{TAG}** (commit **{SHA}**), "
            "attached by GitHub as Source code (zip/tar.gz). The source of the third-party components in the image "
            f"and the server and client archives is attached as **{SOURCE}**.\n\n## Image\n\n"
            f"`ghcr.io/owner/repo:1.2.3@{VERIFIED}` for linux/amd64 and linux/arm64; pin the digest to deploy "
            "exactly this release."))
        source_only = release["body"].split("\n\n## Image")[0]

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
            (published, edit(body=f"{source_only}\n\nnotes"), "is already published", True),
            (published, edit(body=release["body"].replace(VERIFIED, "sha256:" + "b" * 64)),
             "notice is missing or stale", True),
            (published, edit(draft=True, body=f"{source_only}\n\nnotes"), "stale source or image notice", True),
            (published, lambda state: state | other_tag, f"already exists at {OTHER_SHA}", True),
            (EMPTY, lambda state: state | other_tag, f"already exists at {OTHER_SHA}", True),
            (EMPTY, lambda state: state | {"race": OTHER_SHA}, f"resolves to {OTHER_SHA}", False),
            (EMPTY, lambda state: state | rejected, "GitHub rejected creation", False),
            (EMPTY, lambda state: state | {"tamper": "x"}, "draft release asset names", False),
            (published, edit(draft=True, body="## Source availability\nold"), "stale source or image notice", True),
        )
        for base, change, expected, read_only in rows:
            before: State = change(base)
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

    def test_a_transient_failure_repeats_only_calls_that_cannot_create_twice(self) -> None:
        upload = "POST releases/101/assets?name=checksums.txt"
        error, output, _ = self.publish(EMPTY | {"flaky": ["GET releases?per_page=100", "PATCH releases/101"]})
        self.assertIsNone(error, output)
        error, _, after = self.publish(EMPTY | {"flaky": [upload]})
        self.assertIn("(HTTP 502)", error or "")
        self.assertEqual(after["writes"].count(upload), 1)

    def test_a_hung_call_is_cut_by_its_timeout(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "gh").write_text('#!/bin/sh\necho "$*" >>"$CALLS"\nexec sleep 30\n')
            (root / "gh").chmod(0o755)
            env = {"PATH": f"{directory}{os.pathsep}{os.environ['PATH']}", "CALLS": str(root / "calls"),
                   "GH_TOKEN": "secret-token"}
            with (patch.dict(os.environ, env), patch("github_api.time.sleep"),
                  patch.multiple("github_api", TIMEOUT_SECONDS=0.05, UPLOAD_TIMEOUT_SECONDS=0.05),
                  contextlib.redirect_stderr(io.StringIO())):
                for call, attempts in ((lambda: api("repos/owner/repo/releases/1"), 3),
                                       (lambda: api("upload", method="POST", upload=root / "gh"), 1)):
                    (root / "calls").write_text("")
                    with self.assertRaisesRegex(ControlPlaneError, "timeout after 0.05 s"):
                        call()
                    self.assertEqual(len((root / "calls").read_text().splitlines()), attempts)


if __name__ == "__main__":
    unittest.main()
