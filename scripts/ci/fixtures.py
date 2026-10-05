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
from rust_workspace import ROOT, load, offer_name, tui_archive
from verify_release_assets import TARGETS, TUI_FILES, tui_archives

AMD, ARM = "sha256:" + "a" * 64, "sha256:" + "b" * 64
INDEX_TYPE = "application/vnd.oci.image.index.v1+json"
MANIFEST_TYPE = "application/vnd.oci.image.manifest.v1+json"
SLSA = "https://slsa.dev/provenance/v1"
# The Rust archive, when FAKE_RUST_INDEX is set, has its own index and labels.
ENGINE = """#!/bin/sh
printf '%s\\n' "$*" >>"$FAKE_ENGINE_LOG"
case "$*" in
  *-rust.oci.tar:*) [ -z "$FAKE_RUST_INDEX" ] || { FAKE_INDEX=$FAKE_RUST_INDEX; FAKE_LABELS=$FAKE_RUST_LABELS; } ;;
esac
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
              arch_files: Mapping[str, Mapping[str, bytes]] | None = None) -> JsonObject:
    """Write two images of `files`, each with its `arch_files`, and BuildKit-shaped provenance into an OCI archive;
    return its index."""
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
           oci: JsonObject | None = None, rust: JsonObject | None = None) -> dict[str, str]:
    """Serve a two-platform image, `oci` or one with placeholder provenance, from a fake `docker`, and `rust` as
    the VERSION-rust image of an archive named like the Rust one."""
    script = directory / "bin" / "docker"
    script.parent.mkdir(exist_ok=True)
    script.write_text(ENGINE)
    script.chmod(0o755)

    def labels(version: str) -> str:
        return json.dumps({"org.opencontainers.image.source": f"https://github.com/{repository}",
                           "org.opencontainers.image.revision": revision,
                           "org.opencontainers.image.version": version,
                           "org.opencontainers.image.licenses": "AGPL-3.0-or-later"})

    return {
        "CONTAINER_ENGINE": "docker", "FAKE_ENGINE_LOG": str(directory / "engine.log"),
        "PATH": f"{script.parent}{os.pathsep}{os.environ['PATH']}",
        "SKOPEO_IMAGE": "quay.io/containers/skopeo:v1.22.3@sha256:" + "e" * 64,
        "FAKE_INDEX": json.dumps(oci or index(*RUNNABLE, *ATTESTED)),
        "FAKE_LABELS": labels(version),
        "FAKE_RUST_INDEX": json.dumps(rust) if rust else "", "FAKE_RUST_LABELS": labels(f"{version}-rust"),
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


RUST_OCI = "graphite-meter-rust.oci.tar"
# 64-bit little-endian executable headers for each Rust target's architecture.
ELF = {"x86_64": 62, "aarch64": 183}


def executable(target: str, payload: bytes = b"") -> bytes:
    """The start of an executable for `target`, followed by `payload`."""
    if "-windows-" in target:
        return b"MZ" + bytes(0x3A) + (0x40).to_bytes(4, "little") + b"PE\0\0" + (0x8664).to_bytes(2, "little") + payload
    machine = ELF[target.split("-", 1)[0]]
    return b"\x7fELF\x02\x01\x01" + bytes(9) + (2).to_bytes(2, "little") + machine.to_bytes(2, "little") + payload


def rust_source(version: str, offer: str, target: str) -> bytes:
    return (f"Graphite Meter source: https://github.com/zR-JB/graphite-meter/tree/v{version}\n"
            f"Matching release: v{version}\nDependency source archive: {offer}\nRust target: {target}\n").encode()


def write_rust_offer(path: Path, package: str, target: str, profile: str = "release",
                     extra: Mapping[str, bytes] | None = None) -> None:
    """A source offer as the collector writes it, against this checkout."""
    top = path.name.removesuffix(".tar.gz")
    inventory = {"schemaVersion": 1, "package": package, "target": target, "profile": profile,
                 "cargoLockSha256": hashlib.sha256((ROOT / "rust/Cargo.lock").read_bytes()).hexdigest(),
                 "components": [{"component": {"name": "dependency", "version": "1.0.0"}}],
                 "browser": [{"name": "package", "version": "1.0.0"}] if package == "graphite-meter-server" else []}
    write_tar(path, {f"{top}/inventory.json": json.dumps(inventory).encode(),
                     f"{top}/LEGAL.txt": f"notices of {package} for {target}\n".encode(),
                     f"{top}/legal/rust-forks.json": (ROOT / "legal/rust-forks.json").read_bytes(),
                     f"{top}/third_party/cargo/dependency-1.0.0/src/lib.rs": b"code\n",
                     **({f"{top}/third_party/npm/package-1.0.0/index.js": b"code\n"}
                        if package == "graphite-meter-server" else {}),
                     **(extra or {})})


def write_rust_tui(directory: Path, version: str, platform: str, target: str,
                   changes: Mapping[str, bytes] | None = None) -> None:
    """A TUI archive and its source offer as package_rust writes them; `changes` replaces archive members."""
    archive, base, binary = tui_archive(version, platform)
    offer = offer_name("graphite-meter-client", version, platform)
    write_rust_offer(directory / offer, "graphite-meter-client", target)
    members = {binary: executable(target), "LICENSE": (ROOT / "LICENSE").read_bytes(),
               "COPYRIGHT": (ROOT / "COPYRIGHT").read_bytes(),
               "THIRD_PARTY_NOTICES.txt": f"notices of graphite-meter-client for {target}\n".encode(),
               "SOURCE.txt": rust_source(version, offer, target)} | dict(changes or {})
    files = {f"{base}/{name}": payload for name, payload in members.items()}
    if archive.endswith(".zip"):
        with zipfile.ZipFile(directory / archive, "w") as handle:
            for name, payload in files.items():
                handle.writestr(name, payload)
    else:
        write_tar(directory / archive, files)


def write_rust_release(image: Path, tui: Path | None, version: str, revision: str, repository: str) -> JsonObject:
    """Stage the Rust image with its server source offers and the TUI archives; return the image's index. Without
    `tui`, stage a prerelease's image, which BuildKit fetched from GitHub and whose SOURCE.txt names the repository."""
    image.mkdir(parents=True, exist_ok=True)
    workspace = load()
    sources: dict[str, Mapping[str, bytes]] = {}
    for platform, target in workspace.server.items():
        offer = offer_name("graphite-meter-server", version, platform)
        source = rust_source(version, offer, target)
        if tui is None:
            source = f"https://github.com/{repository}\n".encode()
        else:
            write_rust_offer(image / offer, "graphite-meter-server", target)
        sources[platform.split("/")[1]] = {"usr/share/licenses/graphite-meter/SOURCE.txt": source}
    oci = write_oci(image / RUST_OCI, repository, revision, remote=tui is None, arch_files=sources)
    (image / f"{RUST_OCI}.sha256").write_text(
        f"{hashlib.sha256((image / RUST_OCI).read_bytes()).hexdigest()}  {RUST_OCI}\n")
    if tui is not None:
        tui.mkdir(parents=True, exist_ok=True)
        for platform, target in workspace.tui.items():
            write_rust_tui(tui, version, platform, target)
    return oci
