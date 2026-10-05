"""Fake boundaries for control-plane tests: GitHub API, Git, a Skopeo engine and assets."""

from __future__ import annotations

import hashlib
import io
import json
import os
import tarfile
import unittest
import zipfile
from collections.abc import Callable, Mapping
from contextlib import AbstractContextManager
from pathlib import Path
from typing import cast
from unittest.mock import patch

from github_api import ControlPlaneError, JsonObject, JsonValue
from verify_release_assets import TARGETS, TUI_FILES, tui_archives

AMD, ARM = "sha256:" + "a" * 64, "sha256:" + "b" * 64
INDEX_TYPE = "application/vnd.oci.image.index.v1+json"
MANIFEST_TYPE = "application/vnd.oci.image.manifest.v1+json"
SLSA = "https://slsa.dev/provenance/v1"
ENGINE = """#!/bin/sh
printf '%s\\n' "$*" >>"$FAKE_ENGINE_LOG"
case "$*" in
  *" inspect --raw "*) printf '%s\\n' "$FAKE_INDEX" ;;
  *"{{json .Labels}}"*) printf '%s\\n' "$FAKE_LABELS" ;;
  *"{{.Digest}}"*) echo "$FAKE_DIGEST" ;;
  *" copy --all "*) ;;
  *) echo "unexpected engine call: $*" >&2; exit 1 ;;
esac
"""


class Paged(list[object]):
    """A response GitHub must serve through `--paginate --slurp`."""


class Answers(list[object]):
    """Successive responses to one call; the last one repeats."""


def pages(*items: object) -> Paged:
    return Paged(items)


def github(responses: Mapping[str, object]) -> AbstractContextManager[object]:
    """Answer exactly the given API paths, with pagination only where GitHub needs it."""
    served: dict[str, int] = {}

    def api(path: str, *, paginate: bool = False) -> JsonValue:
        if path not in responses:
            raise AssertionError(f"unexpected API call {path}")
        body = responses[path]
        items = list(body) if isinstance(body, Answers) else [body]
        if any(isinstance(item, Paged) != paginate for item in items):
            raise AssertionError(f"{path} must {'not ' if paginate else ''}be paginated")
        served[path] = served.get(path, -1) + 1
        return cast(JsonValue, items[min(served[path], len(items) - 1)])

    return patch("github_api.api", api)


def outcome[T](test: unittest.TestCase, error: str | None, call: Callable[[], T]) -> T | None:
    """Return `call()` when `error` is None; otherwise require it to refuse with `error`."""
    if error is None:
        return call()
    with test.assertRaisesRegex(ControlPlaneError, error):
        call()
    return None


def git_head(directory: Path, sha: str) -> dict[str, str]:
    """Point `git rev-parse HEAD` at `sha` through a minimal GIT_DIR."""
    git = directory / "git"
    (git / "objects").mkdir(parents=True)
    (git / "refs").mkdir()
    (git / "HEAD").write_text(sha + "\n")
    return {"GIT_DIR": str(git)}


def index(*manifests: JsonObject) -> JsonObject:
    return {"schemaVersion": 2, "mediaType": INDEX_TYPE, "manifests": list(manifests)}


def descriptor(os_name: str, arch: str, digest: str, attests: str | None = None) -> JsonObject:
    item: JsonObject = {"mediaType": MANIFEST_TYPE, "digest": digest,
                               "platform": {"os": os_name, "architecture": arch}}
    if attests is not None:
        item["annotations"] = {"vnd.docker.reference.type": "attestation-manifest",
                               "vnd.docker.reference.digest": attests}
    return item


RUNNABLE = (descriptor("linux", "amd64", AMD), descriptor("linux", "arm64", ARM))
IMAGE_FILES = {"./graphite-meter": b"server", "usr/share/licenses/graphite-meter/THIRD_PARTY_NOTICES.txt": b"notices"}
ATTESTED = (descriptor("unknown", "unknown", "sha256:" + "c" * 64, AMD),
            descriptor("unknown", "unknown", "sha256:" + "d" * 64, ARM))


def write_oci(path: Path, repository: str, revision: str, *, remote: bool, tamper: bool = False,
              predicate: str = SLSA, files: Mapping[str, bytes] = IMAGE_FILES) -> JsonObject:
    """Write two images of `files` with BuildKit-shaped provenance into an OCI archive; return its index."""
    blobs: dict[str, bytes] = {}

    def add(value: object) -> str:
        data = json.dumps(value).encode()
        digest = "sha256:" + hashlib.sha256(data).hexdigest()
        blobs[digest] = data + (b" " if tamper else b"")
        return digest

    layer = io.BytesIO()
    with tarfile.open(fileobj=layer, mode="w:gz") as archive:
        for name, payload in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))
    layer_digest = "sha256:" + hashlib.sha256(layer.getvalue()).hexdigest()
    blobs[layer_digest] = layer.getvalue()
    images = {arch: add({"schemaVersion": 2, "mediaType": MANIFEST_TYPE, "architecture": arch,
                         "layers": [{"digest": layer_digest}]}) for arch in ("amd64", "arm64")}

    source: JsonObject = {"path": "Dockerfile"}
    if remote:
        source = {"uri": f"https://github.com/{repository}.git#{revision}",
                  "digest": {"sha1": revision}, "path": "container/Dockerfile"}
    vcs = {"source": f"https://github.com/{repository}", "revision": revision}
    statement = add({"predicateType": SLSA, "subject": [], "predicate": {
        "buildDefinition": {"externalParameters": {"configSource": source}},
        "runDetails": {"metadata": {"buildkit_metadata": {} if remote else {"vcs": vcs}}}}})
    statement_layer = {"mediaType": "application/vnd.in-toto+json", "digest": statement,
             "annotations": {"in-toto.io/predicate-type": predicate}}
    attested = [descriptor("unknown", "unknown", add({"layers": [statement_layer]}), image)
                for image in images.values()]
    with tarfile.open(path, "w") as archive:
        for digest, data in blobs.items():
            info = tarfile.TarInfo("blobs/sha256/" + digest.removeprefix("sha256:"))
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    return index(*(descriptor("linux", arch, image) for arch, image in images.items()), *attested)


def engine(directory: Path, repository: str, version: str, revision: str,
           oci: JsonObject | None = None) -> dict[str, str]:
    """Serve a two-platform image, `oci` or one with placeholder provenance, from a fake `docker`."""
    script = directory / "bin" / "docker"
    script.parent.mkdir(exist_ok=True)
    script.write_text(ENGINE)
    script.chmod(0o755)
    labels = {"org.opencontainers.image.source": f"https://github.com/{repository}",
              "org.opencontainers.image.revision": revision,
              "org.opencontainers.image.version": version,
              "org.opencontainers.image.licenses": "AGPL-3.0-or-later"}
    return {
        "CONTAINER_ENGINE": "docker", "FAKE_ENGINE_LOG": str(directory / "engine.log"),
        "PATH": f"{script.parent}{os.pathsep}{os.environ['PATH']}",
        "SKOPEO_IMAGE": "quay.io/containers/skopeo:v1.22.3@sha256:" + "e" * 64,
        "FAKE_INDEX": json.dumps(oci or index(*RUNNABLE, *ATTESTED)),
        "FAKE_LABELS": json.dumps(labels),
        "FAKE_DIGEST": AMD, "REPOSITORY": repository,
    }


def write_tar(path: Path, members: dict[str, bytes]) -> None:
    with tarfile.open(path, "w:gz") as archive:
        for name, payload in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))


def source_members(version: str) -> dict[str, bytes]:
    root = f"graphite-meter_{version}_third-party-source"
    readme = (b"Use Source code (tar.gz) or Source code (zip). This archive does not "
              b"duplicate Graphite Meter's own repository source.\n")
    return {
        f"{root}/README.txt": readme,
        f"{root}/LEGAL_INVENTORY.json": b'{"server":[],"tui":[],"container":[]}',
        f"{root}/PROVENANCE.json": b"[]",
        f"{root}/third_party/go/quic-go/internal/testdata/priv.key": b"upstream fixture",
        f"{root}/third_party/manual/sample/source.txt": b"manual source",
    }


def write_checksums(dist: Path) -> None:
    lines = [f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
             for path in sorted(dist.iterdir()) if path.name != "checksums.txt"]
    (dist / "checksums.txt").write_text("".join(lines))


def write_release_assets(dist: Path, version: str, reported: str = "") -> None:
    """Write the native release a stable request uploads; each TUI prints `reported`."""
    dist.mkdir(parents=True, exist_ok=True)
    write_tar(dist / f"graphite-meter_{version}_third-party-source.tar.gz",
              source_members(version))
    script = f"#!/bin/sh\necho graphite-meter-client {reported or version}\n".encode()
    for name, (base, binary) in tui_archives(version, TARGETS).items():
        members = {f"{base}/{file}": b"x" for file in TUI_FILES} | {f"{base}/{binary}": script}
        if name.endswith(".zip"):
            with zipfile.ZipFile(dist / name, "w") as archive:
                for member, payload in members.items():
                    archive.writestr(member, payload)
        else:
            write_tar(dist / name, members)
    write_checksums(dist)
