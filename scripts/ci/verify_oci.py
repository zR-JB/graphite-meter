#!/usr/bin/env python3
"""Verify release OCI platforms, provenance and labels with the pinned Skopeo image."""

from __future__ import annotations

import os
import re
import shutil
import subprocess
from pathlib import Path

from github_api import (
    ControlPlaneError,
    JsonObject,
    decode_json,
    expect_array,
    expect_object,
    object_field,
    str_field,
)
from trust import env

DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}")
PLATFORMS = {"amd64", "arm64"}
INDEX_TYPE = "application/vnd.oci.image.index.v1+json"
MANIFEST_TYPE = "application/vnd.oci.image.manifest.v1+json"
ARCHIVE = "oci-archive:/work/image.oci.tar"
ENGINES = ("docker", "podman")


class VerificationError(ControlPlaneError):
    pass


def validate_index_descriptors(index: JsonObject) -> dict[str, str]:
    """Return the runnable digests, each of which needs exactly one linked provenance manifest."""
    if index.get("schemaVersion") != 2 or index.get("mediaType") != INDEX_TYPE:
        raise VerificationError(f"OCI index must be a schemaVersion 2 {INDEX_TYPE}")
    runnable: dict[str, str] = {}
    attested: list[str] = []
    for position, value in enumerate(expect_array(index.get("manifests"), "OCI manifests")):
        context = f"OCI index.manifests[{position}]"
        manifest = expect_object(value, context)
        platform = object_field(manifest, "platform", context)
        system = str_field(platform, "os", context)
        arch = str_field(platform, "architecture", context)
        digest = str_field(manifest, "digest", context)
        if DIGEST_RE.fullmatch(digest) is None or manifest.get("mediaType") != MANIFEST_TYPE:
            raise VerificationError(f"{context} must be an OCI manifest with a sha256 digest")
        if system == "linux" and arch in PLATFORMS and arch not in runnable:
            runnable[arch] = digest
        elif (system, arch) == ("unknown", "unknown"):
            annotations = object_field(manifest, "annotations", context)
            if annotations.get("vnd.docker.reference.type") != "attestation-manifest":
                raise VerificationError(f"{context} is not a provenance attestation manifest")
            attested.append(str_field(annotations, "vnd.docker.reference.digest", context))
        else:
            raise VerificationError(f"unexpected or duplicate OCI platform {system}/{arch}")
    if runnable.keys() != PLATFORMS:
        raise VerificationError(f"OCI archive needs linux/amd64 and linux/arm64, got {runnable}")
    if sorted(attested) != sorted(runnable.values()):
        raise VerificationError("OCI archive needs one provenance attestation per image")
    return runnable


def select_engine() -> str:
    """Return the configured engine name, or the first installed one; never a path."""
    configured = os.environ.get("CONTAINER_ENGINE")
    if configured and configured not in ENGINES:
        raise VerificationError(f"CONTAINER_ENGINE must be one of {', '.join(ENGINES)}")
    for candidate in ENGINES:
        if configured in (None, "", candidate) and shutil.which(candidate):
            return candidate
    raise VerificationError("OCI verification requires Docker or Podman")


def run(*args: str) -> str:
    result = subprocess.run(args, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise VerificationError(f"command failed ({' '.join(args)}): {detail}")
    return result.stdout.strip()


def skopeo(engine: str, image: str, *args: str, archive: Path | None = None) -> str:
    mount = ("-v", f"{archive.resolve()}:/work/image.oci.tar:ro") if archive else ()
    return run(engine, "run", "--rm", "--network", "none", "--entrypoint", "skopeo", *mount,
               image, *args)


def verify(version: str, revision: str, archive: Path) -> str:
    """Verify the archive and return its manifest digest."""
    if archive.is_symlink() or not archive.is_file() or archive.stat().st_size == 0:
        raise VerificationError(f"OCI archive is missing, empty, or not a regular file: {archive}")
    engine, image = select_engine(), env("SKOPEO_IMAGE")
    repository = env("REPOSITORY")

    def inspect(*args: str) -> JsonObject:
        output = skopeo(engine, image, "inspect", *args, ARCHIVE, archive=archive)
        return expect_object(decode_json(output, "skopeo inspect"), "skopeo inspect")

    validate_index_descriptors(inspect("--raw"))
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
                raise VerificationError(
                    f"OCI label {key} for linux/{arch} is {labels.get(key)!r}; expected {value!r}")
    digest = skopeo(engine, image, "inspect", "--format", "{{.Digest}}", ARCHIVE, archive=archive)
    if DIGEST_RE.fullmatch(digest) is None:
        raise VerificationError(f"OCI archive digest is {digest!r}")
    print(f"OCI verification passed: {version} @ {revision} as {digest}")
    return digest
