#!/usr/bin/env python3
"""Verify release OCI platforms, provenance and labels with the pinned Skopeo image."""

from __future__ import annotations

import hashlib
import os
import re
import shutil
import subprocess
import tarfile
from pathlib import Path

from github_api import (
    ControlPlaneError,
    JsonObject,
    decode_json,
    expect_array,
    expect_object,
    fail,
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
ARCHIVE = "oci-archive:/work/image.oci.tar"
ENGINES = ("docker", "podman")


def validate_index_descriptors(index: JsonObject) -> list[str]:
    """Return the provenance manifest digests, exactly one linked to each runnable image."""
    if index.get("schemaVersion") != 2 or index.get("mediaType") != INDEX_TYPE:
        fail(f"OCI index must be a schemaVersion 2 {INDEX_TYPE}")
    runnable: dict[str, str] = {}
    attested: list[str] = []
    attestations: list[str] = []
    for position, value in enumerate(expect_array(index.get("manifests"), "OCI manifests")):
        context = f"OCI index.manifests[{position}]"
        manifest = expect_object(value, context)
        platform = object_field(manifest, "platform", context)
        system = str_field(platform, "os", context)
        arch = str_field(platform, "architecture", context)
        digest = str_field(manifest, "digest", context)
        if DIGEST_RE.fullmatch(digest) is None or manifest.get("mediaType") != MANIFEST_TYPE:
            fail(f"{context} must be an OCI manifest with a sha256 digest")
        if system == "linux" and arch in PLATFORMS and arch not in runnable:
            runnable[arch] = digest
        elif (system, arch) == ("unknown", "unknown"):
            annotations = object_field(manifest, "annotations", context)
            if annotations.get("vnd.docker.reference.type") != "attestation-manifest":
                fail(f"{context} is not a provenance attestation manifest")
            attested.append(str_field(annotations, "vnd.docker.reference.digest", context))
            attestations.append(digest)
        else:
            fail(f"unexpected or duplicate OCI platform {system}/{arch}")
    if runnable.keys() != PLATFORMS:
        fail(f"OCI archive needs linux/amd64 and linux/arm64, got {runnable}")
    if sorted(attested) != sorted(runnable.values()):
        fail("OCI archive needs one provenance attestation per image")
    return attestations


def blob(archive: tarfile.TarFile, digest: str) -> JsonObject:
    """Decode a JSON blob of the OCI layout after checking its size and digest."""
    try:
        member = archive.getmember("blobs/sha256/" + digest.removeprefix("sha256:"))
    except KeyError:
        raise ControlPlaneError(f"OCI archive lacks blob {digest}") from None
    handle = archive.extractfile(member) if member.isfile() and member.size <= BLOB_LIMIT else None
    if handle is None:
        fail(f"OCI blob {digest} is not a bounded regular file")
    data = handle.read()
    if hashlib.sha256(data).hexdigest() != digest.removeprefix("sha256:"):
        fail(f"OCI blob {digest} does not match its digest")
    return expect_object(decode_json(data.decode(errors="replace"), digest), digest)


def source_commit(statement: JsonObject, repository: str) -> str:
    """Return the commit a BuildKit SLSA provenance statement says it built from `repository`."""
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
    return commit


def provenance_sources(archive: tarfile.TarFile, attestation: str, repository: str) -> set[str]:
    """Return the commits that the SLSA statements in `attestation` say BuildKit built."""
    sources = set()
    for item in expect_array(blob(archive, attestation).get("layers"), attestation):
        layer = expect_object(item, attestation)
        if object_field(layer, "annotations", attestation).get("in-toto.io/predicate-type") == SLSA:
            statement = blob(archive, str_field(layer, "digest", attestation))
            sources.add(source_commit(statement, repository))
    if not sources:
        fail(f"{attestation} holds no SLSA provenance statement")
    return sources


def select_engine() -> str:
    """Return the configured engine name, or the first installed one; never a path."""
    configured = os.environ.get("CONTAINER_ENGINE")
    if configured and configured not in ENGINES:
        fail(f"CONTAINER_ENGINE must be one of {', '.join(ENGINES)}")
    for candidate in ENGINES:
        if configured in (None, "", candidate) and shutil.which(candidate):
            return candidate
    fail("OCI verification requires Docker or Podman")


def run(*args: str) -> str:
    result = subprocess.run(args, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        fail(f"command failed ({' '.join(args)}): {detail}")
    return result.stdout.strip()


def skopeo(engine: str, image: str, *args: str, archive: Path | None = None) -> str:
    mount = ("-v", f"{archive.resolve()}:/work/image.oci.tar:ro") if archive else ()
    return run(engine, "run", "--rm", "--network", "none", "--entrypoint", "skopeo", *mount,
               image, *args)


def verify(version: str, revision: str, archive: Path) -> str:
    """Verify the archive and return its manifest digest."""
    if archive.is_symlink() or not archive.is_file() or archive.stat().st_size == 0:
        fail(f"OCI archive is missing, empty, or not a regular file: {archive}")
    engine, image = select_engine(), env("SKOPEO_IMAGE")
    repository = env("REPOSITORY")

    def inspect(*args: str) -> JsonObject:
        output = skopeo(engine, image, "inspect", *args, ARCHIVE, archive=archive)
        return expect_object(decode_json(output, "skopeo inspect"), "skopeo inspect")

    attestations = validate_index_descriptors(inspect("--raw"))
    try:
        with tarfile.open(archive, mode="r:") as tar:
            sources = set().union(*(provenance_sources(tar, digest, repository)
                                    for digest in attestations))
    except tarfile.TarError as exc:
        raise ControlPlaneError(f"cannot read OCI archive layout: {exc}") from exc
    if sources != {revision}:
        fail(f"provenance records sources {sorted(sources)}, not {revision}")
    # Copying every blob proves the archive is complete; the copy stays inside the container.
    skopeo(engine, image, "copy", "--all", ARCHIVE, "oci:/tmp/graphite-meter-verified:verified",
           archive=archive)
    expected = {
        "org.opencontainers.image.source": f"https://github.com/{repository}",
        "org.opencontainers.image.revision": revision,
        "org.opencontainers.image.version": version,
        "org.opencontainers.image.licenses": "AGPL-3.0-or-later",
    }
    for arch in sorted(PLATFORMS):
        labels = inspect("--override-os", "linux", "--override-arch", arch,
                         "--format", "{{json .Labels}}")
        for key, value in expected.items():
            if labels.get(key) != value:
                fail(
                    f"OCI label {key} for linux/{arch} is {labels.get(key)!r}; expected {value!r}")
    digest = skopeo(engine, image, "inspect", "--format", "{{.Digest}}", ARCHIVE, archive=archive)
    if DIGEST_RE.fullmatch(digest) is None:
        fail(f"OCI archive digest is {digest!r}")
    print(f"OCI verification passed: {version} @ {revision} as {digest}")
    return digest
