#!/usr/bin/env python3
"""Verify the release image's OCI archive as data: every blob, the platforms, provenance, notices and labels."""

from __future__ import annotations

import hashlib
import io
import re
import tarfile
from collections.abc import Callable
from pathlib import Path, PurePosixPath

from github_api import (
    ControlPlaneError,
    JsonObject,
    decode_json,
    expect_array,
    expect_object,
    fail,
    int_field,
    object_field,
    str_field,
)
from trust import env

DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}")
PLATFORMS = {"amd64", "arm64"}
INDEX_TYPE = "application/vnd.oci.image.index.v1+json"
MANIFEST_TYPE = "application/vnd.oci.image.manifest.v1+json"
SLSA = "https://slsa.dev/provenance/v1"
BLOB_LIMIT = 4 * 1024 * 1024
LAYER_LIMIT = 256 * 1024 * 1024
# Where the image keeps its server, its third-party notices and its SOURCE.txt.
SERVER, NOTICES = "graphite-meter", "usr/share/licenses/graphite-meter/THIRD_PARTY_NOTICES.txt"
SOURCE = "usr/share/licenses/graphite-meter/SOURCE.txt"
# Opens an unreviewed development build's notices; its executable carries it too (rust/legal/src/lib.rs).
DEVELOPMENT = "UNREVIEWED DEVELOPMENT BUILD"
# The Dockerfile and target that build the image.
STAGE = ("container/Dockerfile", "server")


def validate_index_descriptors(index: JsonObject) -> tuple[list[JsonObject], dict[str, JsonObject]]:
    """Return the provenance manifests, exactly one linked to each runnable image, and the images by architecture."""
    if index.get("schemaVersion") != 2 or index.get("mediaType") != INDEX_TYPE:
        fail(f"OCI index must be a schemaVersion 2 {INDEX_TYPE}")
    runnable: dict[str, JsonObject] = {}
    attested: list[str] = []
    attestations: list[JsonObject] = []
    for position, value in enumerate(expect_array(index.get("manifests"), "OCI manifests")):
        context = f"OCI index.manifests[{position}]"
        manifest = expect_object(value, context)
        platform = object_field(manifest, "platform", context)
        system, arch = str_field(platform, "os", context), str_field(platform, "architecture", context)
        if DIGEST_RE.fullmatch(str_field(manifest, "digest", context)) is None \
                or manifest.get("mediaType") != MANIFEST_TYPE:
            fail(f"{context} must be an OCI manifest with a sha256 digest")
        if system == "linux" and arch in PLATFORMS and arch not in runnable:
            runnable[arch] = manifest
        elif (system, arch) == ("unknown", "unknown"):
            annotations = object_field(manifest, "annotations", context)
            if annotations.get("vnd.docker.reference.type") != "attestation-manifest":
                fail(f"{context} is not a provenance attestation manifest")
            attested.append(str_field(annotations, "vnd.docker.reference.digest", context))
            attestations.append(manifest)
        else:
            fail(f"unexpected or duplicate OCI platform {system}/{arch}")
    if runnable.keys() != PLATFORMS:
        fail(f"OCI archive needs linux/amd64 and linux/arm64, got {sorted(runnable)}")
    if sorted(attested) != sorted(str(image["digest"]) for image in runnable.values()):
        fail("OCI archive needs one provenance attestation per image")
    return attestations, dict(sorted(runnable.items()))


def member(archive: tarfile.TarFile, descriptor: JsonObject, limit: int) -> bytes:
    """The blob a descriptor names, after checking its presence, size and digest."""
    digest = str_field(descriptor, "digest", "OCI descriptor")
    if DIGEST_RE.fullmatch(digest) is None:
        fail(f"OCI descriptor digest {digest!r} is not a sha256 digest")
    try:
        entry = archive.getmember("blobs/sha256/" + digest.removeprefix("sha256:"))
    except KeyError:
        raise ControlPlaneError(f"OCI archive lacks blob {digest}") from None
    size = int_field(descriptor, "size", digest)
    handle = archive.extractfile(entry) if entry.isfile() and entry.size == size <= limit else None
    if handle is None:
        fail(f"OCI blob {digest} is not a bounded regular file of its descriptor's size")
    data = handle.read()
    if hashlib.sha256(data).hexdigest() != digest.removeprefix("sha256:"):
        fail(f"OCI blob {digest} does not match its digest")
    return data


def blob(archive: tarfile.TarFile, descriptor: JsonObject) -> JsonObject:
    """A JSON blob of the OCI layout after checking its size and digest."""
    digest = str(descriptor.get("digest"))
    return expect_object(decode_json(member(archive, descriptor, BLOB_LIMIT).decode(errors="replace"), digest), digest)


def layers(manifest: JsonObject, context: str) -> list[JsonObject]:
    return [expect_object(item, context) for item in expect_array(manifest.get("layers"), context)]


def check_image_files(archive: tarfile.TarFile, manifest: JsonObject, image: str) -> tuple[str, bytes]:
    """The image ships the server and its notices, and no copy is a development build's; return its last
    SOURCE.txt, or nothing, and its last server."""
    found = {SERVER: 0, NOTICES: 0}
    source = server = b""
    for layer in layers(manifest, image):
        with tarfile.open(fileobj=io.BytesIO(member(archive, layer, LAYER_LIMIT))) as files:
            for entry in files:
                name = entry.name.removeprefix("./").removeprefix("/")
                if name == SOURCE and (handle := files.extractfile(entry)) is not None:
                    source = handle.read(BLOB_LIMIT)
                if name in found and (handle := files.extractfile(entry)) is not None:
                    found[name] += 1
                    if DEVELOPMENT.encode() in (data := handle.read()):
                        fail(f"image {image} ships an unreviewed development build's {name}")
                    server = data if name == SERVER else server
    if missing := sorted(name for name, count in found.items() if count == 0):
        fail(f"image {image} lacks {missing}")
    return source.decode(errors="replace"), server


def source_commit(statement: JsonObject, repository: str) -> str:
    """Return the commit a BuildKit SLSA provenance statement says it built from `repository`, requiring it to have
    built STAGE: the Dockerfile by its path in the context, or for a local context its name."""
    def nested(*keys: str) -> JsonObject:
        value = statement
        for key in keys:
            value = object_field(value, key, "provenance")
        return value

    # A remote Git context records the commit it fetched; a local one records the checkout.
    source = nested("predicate", "buildDefinition", "externalParameters", "configSource")
    if "digest" in source:
        commit = str_field(object_field(source, "digest", "configSource"), "sha1", "configSource")
        origin = str_field(source, "uri", "configSource").removesuffix(f"#{commit}")
        expected = f"https://github.com/{repository}.git"
    else:
        vcs = nested("predicate", "runDetails", "metadata", "buildkit_metadata", "vcs")
        commit, origin = str_field(vcs, "revision", "vcs"), str_field(vcs, "source", "vcs")
        expected = f"https://github.com/{repository}"
    if statement.get("predicateType") != SLSA or origin != expected:
        fail(f"provenance built {origin!r}, not {expected!r}")
    dockerfile, target = STAGE
    request = nested("predicate", "buildDefinition", "externalParameters", "request")
    built = (str_field(source, "path", "configSource"), object_field(request, "args", "request").get("target"))
    if built[0] not in (dockerfile, PurePosixPath(dockerfile).name) or built[1] != target:
        fail(f"provenance built target {built[1]!r} of {built[0]!r}, not {target!r} of {dockerfile!r}")
    return commit


def provenance_sources(archive: tarfile.TarFile, attestation: JsonObject, repository: str) -> set[str]:
    """Return the commits that the SLSA statements in `attestation` say BuildKit built."""
    manifest = blob(archive, attestation)
    member(archive, object_field(manifest, "config", "attestation"), BLOB_LIMIT)
    sources = set()
    for layer in layers(manifest, "attestation"):
        statement = blob(archive, layer)
        if object_field(layer, "annotations", "attestation").get("in-toto.io/predicate-type") == SLSA:
            sources.add(source_commit(statement, repository))
    if not sources:
        fail(f"{attestation['digest']} holds no SLSA provenance statement")
    return sources


def verify(version: str, revision: str, archive: Path,
           check_files: Callable[[str, str, bytes], None] | None = None) -> str:
    """Verify the archive, passing each architecture's SOURCE.txt and server to `check_files`, and return the digest
    of its image index, which publication pushes."""
    if archive.is_symlink() or not archive.is_file() or archive.stat().st_size == 0:
        fail(f"OCI archive is missing, empty, or not a regular file: {archive}")
    repository = env("REPOSITORY")
    labels = {
        "org.opencontainers.image.source": f"https://github.com/{repository}",
        "org.opencontainers.image.revision": revision,
        "org.opencontainers.image.version": version,
        "org.opencontainers.image.licenses": "AGPL-3.0-or-later",
    }
    # Every blob the index reaches is read once and checked against its descriptor, so the archive is complete.
    try:
        with tarfile.open(archive, mode="r:") as tar:
            entry = tar.getmember("index.json")
            layout = tar.extractfile(entry) if entry.isfile() and entry.size <= BLOB_LIMIT else None
            if layout is None:
                fail("OCI index.json is not a bounded regular file")
            top = expect_object(decode_json(layout.read().decode(errors="replace"), "index.json"), "index.json")
            listed = expect_array(top.get("manifests"), "index.json manifests")
            if len(listed) != 1:
                fail("OCI index.json must list exactly one image index")
            index = expect_object(listed[0], "index.json")
            attestations, images = validate_index_descriptors(blob(tar, index))
            sources = set().union(*(provenance_sources(tar, item, repository) for item in attestations))
            for arch, image in images.items():
                manifest = blob(tar, image)
                config = object_field(blob(tar, object_field(manifest, "config", arch)), "config", arch)
                for key, value in labels.items():
                    if (found := object_field(config, "Labels", arch).get(key)) != value:
                        fail(f"OCI label {key} for linux/{arch} is {found!r}; expected {value!r}")
                source, server = check_image_files(tar, manifest, str(image["digest"]))
                if check_files is not None:
                    check_files(arch, source, server)
    except (KeyError, tarfile.TarError) as exc:
        raise ControlPlaneError(f"cannot read OCI archive layout: {exc}") from exc
    if sources != {revision}:
        fail(f"provenance records sources {sorted(sources)}, not {revision}")
    digest = str(index["digest"])
    print(f"OCI verification passed: {version} @ {revision} as {digest}")
    return digest
