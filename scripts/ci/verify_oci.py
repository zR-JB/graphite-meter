#!/usr/bin/env python3
"""Verify the release image's OCI archive as data: every blob, the platforms, provenance, notices and labels."""

from __future__ import annotations

import hashlib
import io
import posixpath
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
CONFIG_TYPE = "application/vnd.oci.image.config.v1+json"
LAYER_TYPE = "application/vnd.oci.image.layer.v1.tar+gzip"
STATEMENT_TYPE = "application/vnd.in-toto+json"
ATTESTATION_TYPE = "application/vnd.docker.attestation.manifest.v1+json"
EMPTY: JsonObject = {"mediaType": "application/vnd.oci.empty.v1+json", "size": 2,
                     "digest": "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"}
# The fields our build emits; publication may read any other as one of them, or act on it.
FIELDS = {
    INDEX_TYPE: {"schemaVersion", "mediaType", "manifests", "annotations"},
    MANIFEST_TYPE: {"schemaVersion", "mediaType", "config", "layers", "annotations"},
    ATTESTATION_TYPE: {"schemaVersion", "mediaType", "artifactType", "config", "layers", "subject"},
    "descriptor": {"mediaType", "digest", "size", "annotations", "platform"},
    "platform": {"os", "architecture"},
}
SLSA = "https://slsa.dev/provenance/v1"
BLOB_LIMIT = 4 * 1024 * 1024
LAYER_LIMIT = 256 * 1024 * 1024
SERVER, NOTICES = "graphite-meter", "usr/share/licenses/graphite-meter/THIRD_PARTY_NOTICES.txt"
SOURCE = "usr/share/licenses/graphite-meter/SOURCE.txt"
FILE_LIMITS = {SERVER: LAYER_LIMIT, NOTICES: LAYER_LIMIT, SOURCE: BLOB_LIMIT}
# Opens an unreviewed development build's notices; its executable carries it too (rust/legal/src/lib.rs).
DEVELOPMENT = "UNREVIEWED DEVELOPMENT BUILD"
STAGE = ("container/Dockerfile", "server")


def canonical(name: str) -> bool:
    """Whether an archive member's name is the one path every reader resolves it to."""
    return name == posixpath.normpath(name) and name[:1] != "/" and name.split("/", 1)[0] not in (".", "..")


def only(value: JsonObject, kind: str, context: str) -> JsonObject:
    """`value` with only the FIELDS of `kind`, and annotations from strings to strings."""
    annotations = value.get("annotations", {})
    if not value.keys() <= FIELDS[kind] or not isinstance(annotations, dict) \
            or not all(isinstance(item, str) for item in annotations.values()):
        fail(f"{context} has {sorted(value)}, not only {sorted(FIELDS[kind])} with string annotations")
    return value


def document(value: JsonObject, media: str, context: str, kind: str | None = None) -> JsonObject:
    """`value` as a schemaVersion 2 OCI `media` document with only the fields our build emits for `kind`."""
    if int_field(value, "schemaVersion", context) != 2 or value.get("mediaType") != media:
        fail(f"{context} must be a schemaVersion 2 {media}")
    return only(value, kind or media, context)


def validate_index_descriptors(index: JsonObject) -> tuple[list[tuple[JsonObject, str]], dict[str, JsonObject]]:
    """Return the provenance manifests, exactly one linked to each runnable image, with the image digest each
    attests, and the images by architecture."""
    runnable: dict[str, JsonObject] = {}
    attestations: list[tuple[JsonObject, str]] = []
    for position, value in enumerate(expect_array(index.get("manifests"), "OCI manifests")):
        context = f"OCI index.manifests[{position}]"
        manifest = expect_object(value, context)
        platform = object_field(manifest, "platform", context)
        system, arch = str_field(platform, "os", context), str_field(platform, "architecture", context)
        if system == "linux" and arch in PLATFORMS and arch not in runnable:
            runnable[arch] = manifest
        elif (system, arch) == ("unknown", "unknown"):
            annotations = object_field(manifest, "annotations", context)
            if annotations.get("vnd.docker.reference.type") != "attestation-manifest":
                fail(f"{context} is not a provenance attestation manifest")
            attestations.append((manifest, str_field(annotations, "vnd.docker.reference.digest", context)))
        else:
            fail(f"unexpected or duplicate OCI platform {system}/{arch}")
    if runnable.keys() != PLATFORMS:
        fail(f"OCI archive needs linux/amd64 and linux/arm64, got {sorted(runnable)}")
    images = sorted(str_field(image, "digest", "OCI image") for image in runnable.values())
    if sorted(digest for _, digest in attestations) != images:
        fail("OCI archive needs one provenance attestation per image")
    return attestations, dict(sorted(runnable.items()))


def member(archive: tarfile.TarFile, descriptor: JsonObject, media: str, limit: int) -> bytes:
    """The `media` blob a descriptor names, after checking its presence, size and digest."""
    digest = str_field(descriptor, "digest", "OCI descriptor")
    if DIGEST_RE.fullmatch(digest) is None or descriptor.get("mediaType") != media:
        fail(f"OCI descriptor {digest!r} must name a {media} by its sha256 digest")
    only(descriptor, "descriptor", digest)
    if "platform" in descriptor:
        only(object_field(descriptor, "platform", digest), "platform", digest)
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


def blob(archive: tarfile.TarFile, descriptor: JsonObject, media: str, kind: str | None = None) -> JsonObject:
    """A JSON `media` blob of the OCI layout after checking its size and digest."""
    digest = str(descriptor.get("digest"))
    value = expect_object(decode_json(member(archive, descriptor, media, BLOB_LIMIT).decode(errors="replace"), digest),
                          digest)
    return document(value, media, digest, kind) if media in FIELDS else value


def layers(manifest: JsonObject, context: str) -> list[JsonObject]:
    return [expect_object(item, context) for item in expect_array(manifest.get("layers"), context)]


def check_image_files(archive: tarfile.TarFile, manifest: JsonObject, image: str) -> tuple[str, bytes]:
    """The image's layers hold only distinct files and shared directories, each in a directory declared before it,
    among them the server and its notices of a reviewed build; return its SOURCE.txt, or nothing, and its server."""
    shipped: dict[str, bytes] = {}
    directories: set[str] = set()
    for layer in layers(manifest, image):
        with tarfile.open(fileobj=io.BytesIO(member(archive, layer, LAYER_TYPE, LAYER_LIMIT)), mode="r:gz") as files:
            for entry in files:
                name, regular = entry.name, entry.type in (tarfile.REGTYPE, tarfile.AREGTYPE)
                if not canonical(name) or posixpath.basename(name).startswith(".wh.") or name in shipped \
                        or posixpath.dirname(name) not in directories | {""} \
                        or not (entry.isdir() or regular and name not in directories):
                    fail(f"image {image} layer entry {name!r} is not a canonical, unique file or directory")
                if entry.isdir():
                    directories.add(name)
                    continue
                data = b""
                if name in FILE_LIMITS and (handle := files.extractfile(entry)) is not None:
                    if len(data := handle.read(FILE_LIMITS[name] + 1)) > FILE_LIMITS[name]:
                        fail(f"image {image} ships {name} beyond {FILE_LIMITS[name]} bytes")
                    if name != SOURCE and DEVELOPMENT.encode() in data:
                        fail(f"image {image} ships an unreviewed development build's {name}")
                shipped[name] = data
    if missing := sorted({SERVER, NOTICES} - shipped.keys()):
        fail(f"image {image} lacks {missing}")
    return shipped.get(SOURCE, b"").decode(errors="replace"), shipped[SERVER]


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


def provenance_sources(archive: tarfile.TarFile, attestation: JsonObject, image: JsonObject,
                       repository: str) -> set[str]:
    """Return the commits that the SLSA statements in `attestation` of `image` say BuildKit built."""
    manifest = blob(archive, attestation, MANIFEST_TYPE, ATTESTATION_TYPE)
    subject = {key: image[key] for key in ("mediaType", "digest", "size")}
    if manifest.get("artifactType") != ATTESTATION_TYPE or manifest.get("subject") != subject \
            or manifest.get("config") != EMPTY | {"data": "e30="}:
        fail(f"{attestation['digest']} is not BuildKit's empty {ATTESTATION_TYPE} of {image['digest']}")
    member(archive, EMPTY, str(EMPTY["mediaType"]), BLOB_LIMIT)
    sources = set()
    for layer in layers(manifest, "attestation"):
        statement = blob(archive, layer, STATEMENT_TYPE)
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
            names = tar.getnames()
            if len(set(names)) != len(names) or not all(map(canonical, names)):
                fail("OCI archive member names must be canonical and unique")
            entry = tar.getmember("index.json")
            layout = tar.extractfile(entry) if entry.isfile() and entry.size <= BLOB_LIMIT else None
            if layout is None:
                fail("OCI index.json is not a bounded regular file")
            top = document(expect_object(decode_json(layout.read().decode(errors="replace"), "index.json"),
                                         "index.json"), INDEX_TYPE, "index.json")
            listed = expect_array(top.get("manifests"), "index.json manifests")
            if len(listed) != 1:
                fail("OCI index.json must list exactly one image index")
            index = expect_object(listed[0], "index.json")
            attestations, images = validate_index_descriptors(blob(tar, index, INDEX_TYPE))
            for arch, image in images.items():
                manifest = blob(tar, image, MANIFEST_TYPE)
                config = blob(tar, object_field(manifest, "config", arch), CONFIG_TYPE)
                if (config.get("os"), config.get("architecture")) != ("linux", arch):
                    fail(f"linux/{arch} image is configured for {config.get('os')}/{config.get('architecture')}")
                for key, value in labels.items():
                    if (found := object_field(object_field(config, "config", arch), "Labels", arch).get(key)) != value:
                        fail(f"OCI label {key} for linux/{arch} is {found!r}; expected {value!r}")
                source, server = check_image_files(tar, manifest, str(image["digest"]))
                if check_files is not None:
                    check_files(arch, source, server)
            described = {str(image["digest"]): image for image in images.values()}
            sources = set().union(*(provenance_sources(tar, item, described[attested], repository)
                                    for item, attested in attestations))
    except (KeyError, tarfile.TarError) as exc:
        raise ControlPlaneError(f"cannot read OCI archive layout: {exc}") from exc
    if sources != {revision}:
        fail(f"provenance records sources {sorted(sources)}, not {revision}")
    digest = str(index["digest"])
    print(f"OCI verification passed: {version} @ {revision} as {digest}")
    return digest
