"""Fake boundaries for control-plane tests: GitHub API, Git, a Skopeo engine and release files."""

from __future__ import annotations

import hashlib
import io
import json
import os
import tarfile
import tempfile
import unittest
import zipfile
from collections.abc import Callable, Mapping
from contextlib import AbstractContextManager
from pathlib import Path, PurePosixPath
from typing import cast
from unittest.mock import patch

from assets import CLIENT, OCI, SERVER, prerelease, stage_assets
from github_api import ControlPlaneError, JsonObject, JsonValue
from verify_oci import STAGE
from workspace import ROOT, build_name, build_offer, load, offer_name, source_txt, tui_archive

AMD, ARM = "sha256:" + "a" * 64, "sha256:" + "b" * 64
INDEX_TYPE = "application/vnd.oci.image.index.v1+json"
MANIFEST_TYPE = "application/vnd.oci.image.manifest.v1+json"
SLSA = "https://slsa.dev/provenance/v1"
REPOSITORY_URL = "https://github.com/zR-JB/graphite-meter"
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
              predicate: str = SLSA, files: Mapping[str, bytes] = IMAGE_FILES,
              arch_files: Mapping[str, Mapping[str, bytes]] | None = None,
              stage: tuple[str, str | None] = STAGE) -> JsonObject:
    """Write two images of `files`, each with its `arch_files`, and BuildKit-shaped provenance of building `stage`, a
    Dockerfile and target, into an OCI archive; return its index."""
    blobs: dict[str, bytes] = {}

    def add(value: object) -> str:
        data = json.dumps(value).encode()
        digest = "sha256:" + hashlib.sha256(data).hexdigest()
        blobs[digest] = data + (b" " if tamper else b"")
        return digest

    def layer(contents: Mapping[str, bytes]) -> str:
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w:gz") as archive:
            for name, payload in contents.items():
                info = tarfile.TarInfo(name)
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))
        digest = "sha256:" + hashlib.sha256(data.getvalue()).hexdigest()
        blobs[digest] = data.getvalue()
        return digest

    images = {arch: add({"schemaVersion": 2, "mediaType": MANIFEST_TYPE, "architecture": arch, "layers": [
        {"digest": layer({**files, **(arch_files or {}).get(arch, {})})}]}) for arch in ("amd64", "arm64")}

    # A local context's Dockerfile is named without its directory.
    source: JsonObject = {"path": PurePosixPath(stage[0]).name}
    if remote:
        source = {"uri": f"https://github.com/{repository}.git#{revision}",
                  "digest": {"sha1": revision}, "path": stage[0]}
    vcs = {"source": f"https://github.com/{repository}", "revision": revision}
    args: JsonObject = {"target": stage[1]} if stage[1] else {}
    statement = add({"predicateType": SLSA, "subject": [], "predicate": {
        "buildDefinition": {"externalParameters": {"configSource": source, "request": {
            "frontend": "dockerfile.v0", "args": args}}},
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
    """Serve a two-platform image of `version`, `oci` or one with placeholder provenance, from a fake `docker`."""
    script = directory / "bin" / "docker"
    script.parent.mkdir(exist_ok=True)
    script.write_text(ENGINE)
    script.chmod(0o755)
    return {
        "CONTAINER_ENGINE": "docker", "FAKE_ENGINE_LOG": str(directory / "engine.log"),
        "PATH": f"{script.parent}{os.pathsep}{os.environ['PATH']}",
        "SKOPEO_IMAGE": "quay.io/containers/skopeo:v1.22.3@sha256:" + "e" * 64,
        "FAKE_INDEX": json.dumps(oci or index(*RUNNABLE, *ATTESTED)),
        "FAKE_LABELS": json.dumps({"org.opencontainers.image.source": f"https://github.com/{repository}",
                                   "org.opencontainers.image.revision": revision,
                                   "org.opencontainers.image.version": version,
                                   "org.opencontainers.image.licenses": "AGPL-3.0-or-later"}),
        "FAKE_DIGEST": AMD, "REPOSITORY": repository,
    }


def write_tar(path: Path, members: Mapping[str, bytes]) -> None:
    with tarfile.open(path, "w:gz") as archive:
        for name, payload in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))


# 64-bit little-endian executable headers for each Rust target's architecture.
ELF = {"x86_64": 62, "aarch64": 183}


def executable(target: str, payload: bytes = b"") -> bytes:
    """The start of an executable for `target`, followed by `payload`."""
    if "-windows-" in target:
        return b"MZ" + bytes(0x3A) + (0x40).to_bytes(4, "little") + b"PE\0\0" + (0x8664).to_bytes(2, "little") + payload
    machine = ELF[target.split("-", 1)[0]]
    return b"\x7fELF\x02\x01\x01" + bytes(9) + (2).to_bytes(2, "little") + machine.to_bytes(2, "little") + payload


def reviewed(notices: bytes) -> bytes:
    """The marker of a reviewed build of `notices`."""
    return b"graphite-meter reviewed notices sha256:" + hashlib.sha256(notices).hexdigest().encode()


def notices(build: str) -> bytes:
    return f"notices of {build}\n".encode()


def server_binary(platform: str) -> bytes:
    """The image's server for `platform`, the reviewed build of its notices in the source offer."""
    return b"server\0" + reviewed(notices(build_name(SERVER, platform)))


def write_part(path: Path, version: str, package: str, platform: str, profile: str = "release",
               extra: Mapping[str, bytes] | None = None) -> None:
    """A build's part of the source offer as the collector writes it, against this checkout."""
    top, build = offer_name(version).removesuffix(".tar.gz"), build_name(package, platform)
    shipped = load().server if package == SERVER else load().tui
    inventory = {"schemaVersion": 1, "package": package, "target": shipped[platform], "profile": profile,
                 "cargoLockSha256": hashlib.sha256((ROOT / "rust/Cargo.lock").read_bytes()).hexdigest(),
                 "components": [{"component": {"name": "dependency", "version": "1.0.0"}}],
                 "browser": [{"name": "package", "version": "1.0.0"}] if package == SERVER else []}
    write_tar(path, {f"{top}/builds/{build}/inventory.json": json.dumps(inventory).encode(),
                     f"{top}/builds/{build}/LEGAL.txt": notices(build),
                     f"{top}/legal/rust-forks.json": (ROOT / "legal/rust-forks.json").read_bytes(),
                     f"{top}/third_party/cargo/dependency-1.0.0/src/lib.rs": b"code\n",
                     **({f"{top}/third_party/npm/package-1.0.0/index.js": b"code\n"} if package == SERVER else {}),
                     **(extra or {})})


def write_client(directory: Path, version: str, platform: str, changes: Mapping[str, bytes] | None = None) -> None:
    """A client archive as the packager writes it; `changes` replaces archive members."""
    archive, base, binary = tui_archive(version, platform)
    target, build = load().tui[platform], build_name(CLIENT, platform)
    members = {binary: executable(target, reviewed(notices(build))), "LICENSE": (ROOT / "LICENSE").read_bytes(),
               "COPYRIGHT": (ROOT / "COPYRIGHT").read_bytes(), "THIRD_PARTY_NOTICES.txt": notices(build),
               "SOURCE.txt": source_txt(REPOSITORY_URL, version, build, target).encode()} | dict(changes or {})
    files = {f"{base}/{name}": payload for name, payload in members.items()}
    if archive.endswith(".zip"):
        with zipfile.ZipFile(directory / archive, "w") as handle:
            for name, payload in files.items():
                handle.writestr(name, payload)
    else:
        write_tar(directory / archive, files)


def write_exports(root: Path, version: str, revision: str, repository: str,
                  server_profile: str = "release") -> JsonObject:
    """Write what the builds export into `root`: the OCI archive, server-export with each platform's source offer
    part and tui-export with the client archives and their parts; return the image's index. A prerelease's image,
    which BuildKit fetched from GitHub, names the repository."""
    workspace = load()
    server, client = root / "server-export", root / "tui-export"
    client.mkdir(parents=True)
    files: dict[str, Mapping[str, bytes]] = {}
    for platform, target in workspace.server.items():
        build = build_name(SERVER, platform)
        (server / platform.replace("/", "_")).mkdir(parents=True)
        write_part(server / platform.replace("/", "_") / build_offer(SERVER, platform), version, SERVER, platform,
                   server_profile)
        files[platform.split("/")[1]] = {
            "usr/share/licenses/graphite-meter/SOURCE.txt": source_txt(REPOSITORY_URL, version, build, target).encode(),
            "graphite-meter": server_binary(platform)}
    for platform in workspace.tui:
        write_part(client / build_offer(CLIENT, platform), version, CLIENT, platform)
        write_client(client, version, platform)
    return write_oci(root / OCI, repository, revision, remote=prerelease(version), arch_files=files)


def write_staged(out: Path, version: str, revision: str, repository: str) -> JsonObject:
    """Stage a release of `version` into `out` as assets.py stage does; return the image's index."""
    with tempfile.TemporaryDirectory() as directory:
        exports = Path(directory)
        oci = write_exports(exports, version, revision, repository)
        out.mkdir(parents=True, exist_ok=True)
        (out / OCI).write_bytes((exports / OCI).read_bytes())
        (out / f"{OCI}.sha256").write_text(f"{hashlib.sha256((out / OCI).read_bytes()).hexdigest()}  {OCI}\n")
        if not prerelease(version):
            stage_assets(version, exports / "server-export", exports / "tui-export", out)
    return oci
