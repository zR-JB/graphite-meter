#!/usr/bin/env python3
"""Verify release OCI platforms and provenance labels using pinned Skopeo.

The verifier is pure Python apart from invoking the configured container engine.
It intentionally does not depend on jq, grep, or host Skopeo.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path
from github_api import (
    JsonObject,
    JsonShapeError,
    decode_json,
    expect_object,
    int_field,
    object_field,
    str_field,
)


class VerificationError(RuntimeError):
    pass

SHA256_DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
ATTESTATION_TYPE = "attestation-manifest"
ATTESTATION_TYPE_ANNOTATION = "vnd.docker.reference.type"
ATTESTATION_DIGEST_ANNOTATION = "vnd.docker.reference.digest"
EXPECTED_PLATFORMS = {("linux", "amd64"), ("linux", "arm64")}
OCI_INDEX_MEDIA_TYPE = "application/vnd.oci.image.index.v1+json"
OCI_MANIFEST_MEDIA_TYPE = "application/vnd.oci.image.manifest.v1+json"
SKOPEO_VERSION_OUTPUT_RE = re.compile(
    r"^skopeo version (?P<version>[^\s]+)(?: commit: [0-9a-fA-F]+)?$"
)



def parse_skopeo_version(output: str) -> str:
    """Return the exact Skopeo semantic version from supported `--version` output."""
    match = SKOPEO_VERSION_OUTPUT_RE.fullmatch(output.strip())
    if match is None:
        raise VerificationError(f"unexpected Skopeo --version output: {output!r}")
    return match.group("version")

def validate_index_descriptors(index: JsonObject) -> dict[str, str]:
    """Validate the two runnable images and their BuildKit provenance manifests.

    Explicit `provenance: mode=max` creates one provenance attestation manifest
    for each runnable platform in the OCI index. Attestation descriptors use
    platform unknown/unknown and bind back to the runnable manifest digest via
    the BuildKit reference annotations. No other index descriptors are allowed.
    """
    try:
        if int_field(index, "schemaVersion", "OCI index") != 2:
            raise VerificationError("OCI index.schemaVersion must be 2")
        if str_field(index, "mediaType", "OCI index") != OCI_INDEX_MEDIA_TYPE:
            raise VerificationError(f"OCI index.mediaType must be {OCI_INDEX_MEDIA_TYPE}")
    except JsonShapeError as exc:
        raise VerificationError(str(exc)) from exc

    manifests_value = index.get("manifests")
    if not isinstance(manifests_value, list):
        raise VerificationError("OCI index.manifests must be an array")

    runnable: dict[tuple[str, str], str] = {}
    attestations: list[str] = []
    for position, manifest_value in enumerate(manifests_value):
        context = f"OCI index.manifests[{position}]"
        try:
            manifest = expect_object(manifest_value, context)
            platform = object_field(manifest, "platform", context)
            os_name = str_field(platform, "os", f"{context}.platform")
            architecture = str_field(platform, "architecture", f"{context}.platform")
            digest = str_field(manifest, "digest", context)
            media_type = str_field(manifest, "mediaType", context)
        except JsonShapeError as exc:
            raise VerificationError(str(exc)) from exc
        if SHA256_DIGEST_RE.fullmatch(digest) is None:
            raise VerificationError(f"{context}.digest must be a sha256 digest")
        if media_type != OCI_MANIFEST_MEDIA_TYPE:
            raise VerificationError(f"{context}.mediaType must be {OCI_MANIFEST_MEDIA_TYPE}")

        platform_key = (os_name, architecture)
        if platform_key in EXPECTED_PLATFORMS:
            if platform_key in runnable:
                raise VerificationError(
                    f"OCI index contains duplicate runnable platform {os_name}/{architecture}"
                )
            runnable[platform_key] = digest
            continue

        if platform_key != ("unknown", "unknown"):
            raise VerificationError(
                f"OCI index contains unexpected platform {os_name}/{architecture}"
            )
        try:
            annotations = object_field(manifest, "annotations", context)
            reference_type = str_field(
                annotations, ATTESTATION_TYPE_ANNOTATION, f"{context}.annotations"
            )
            reference_digest = str_field(
                annotations, ATTESTATION_DIGEST_ANNOTATION, f"{context}.annotations"
            )
        except JsonShapeError as exc:
            raise VerificationError(str(exc)) from exc
        if reference_type != ATTESTATION_TYPE:
            raise VerificationError(
                f"{context} unknown/unknown descriptor is not a provenance attestation manifest"
            )
        if SHA256_DIGEST_RE.fullmatch(reference_digest) is None:
            raise VerificationError(
                f"{context} attestation reference digest must be a sha256 digest"
            )
        attestations.append(reference_digest)

    if set(runnable) != EXPECTED_PLATFORMS:
        actual = ", ".join(f"{os_name}/{arch}" for os_name, arch in sorted(runnable)) or "none"
        raise VerificationError(
            "OCI archive must contain exactly one runnable linux/amd64 and linux/arm64 "
            f"manifest; got {actual}"
        )

    runnable_digests = set(runnable.values())
    if len(attestations) != len(runnable_digests) or set(attestations) != runnable_digests:
        raise VerificationError(
            "OCI archive must contain exactly one provenance attestation manifest for each "
            "runnable platform manifest"
        )
    if len(attestations) != len(set(attestations)):
        raise VerificationError("OCI archive contains duplicate provenance attestation references")

    return {architecture: digest for (_os_name, architecture), digest in runnable.items()}


def parse_object_json(text: str, context: str) -> JsonObject:
    try:
        return expect_object(decode_json(text, context), context)
    except JsonShapeError as exc:
        raise VerificationError(str(exc)) from exc


def require_env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise VerificationError(f"{name} must be set")
    return value


def select_engine() -> str:
    configured = os.environ.get("CONTAINER_ENGINE")
    if configured:
        if shutil.which(configured) is None:
            raise VerificationError(f"container engine {configured!r} was not found")
        return configured
    for candidate in ("docker", "podman"):
        if shutil.which(candidate) is not None:
            return candidate
    raise VerificationError("OCI verification requires Docker or Podman")


def run(*args: str) -> str:
    result = subprocess.run(
        args,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise VerificationError(f"command failed ({' '.join(args)}): {detail}")
    return result.stdout.strip()


def skopeo(engine: str, image: str, archive: Path, *args: str) -> str:
    return run(
        engine,
        "run",
        "--rm",
        "--entrypoint",
        "skopeo",
        "-v",
        f"{archive.resolve()}:/work/image.oci.tar:ro",
        image,
        *args,
    )


def verify_skopeo_runtime() -> tuple[str, str]:
    """Verify the pinned Skopeo image and its declared human-readable version."""
    engine = select_engine()
    image = require_env("SKOPEO_IMAGE")
    expected_skopeo = require_env("SKOPEO_VERSION")
    version_text = run(
        engine,
        "run",
        "--rm",
        "--entrypoint",
        "skopeo",
        image,
        "--version",
    )
    print(version_text)
    actual_skopeo = parse_skopeo_version(version_text)
    if actual_skopeo != expected_skopeo:
        raise VerificationError(
            f"unexpected Skopeo version; expected {expected_skopeo!r}, got {actual_skopeo!r}"
        )
    return engine, image


def verify_archive_blobs(engine: str, image: str, archive: Path) -> None:
    """Force Skopeo to read and materialize every image in the OCI archive."""
    with tempfile.TemporaryDirectory(prefix="graphite-meter-oci-") as temp_dir:
        destination = Path(temp_dir)
        run(
            engine,
            "run",
            "--rm",
            "--entrypoint",
            "skopeo",
            "-v",
            f"{archive.resolve()}:/work/image.oci.tar:ro",
            "-v",
            f"{destination.resolve()}:/work/oci-copy",
            image,
            "copy",
            "--all",
            "oci-archive:/work/image.oci.tar",
            "oci:/work/oci-copy:verified",
        )
        if not (destination / "oci-layout").is_file() or not (destination / "index.json").is_file():
            raise VerificationError("Skopeo copy did not materialize a readable OCI image layout")


def verify(version: str, revision: str, archive: Path) -> None:
    if not archive.is_file() or archive.stat().st_size == 0:
        raise VerificationError(f"OCI archive is missing or empty: {archive}")

    engine, image = verify_skopeo_runtime()
    repository = require_env("REPOSITORY")

    index = parse_object_json(
        skopeo(engine, image, archive, "inspect", "--raw", "oci-archive:/work/image.oci.tar"),
        "OCI index",
    )
    validate_index_descriptors(index)

    # Inspecting manifests/configs alone does not prove that every referenced
    # layer blob is readable. A full local copy forces Skopeo to consume the
    # complete multi-platform archive before it can become a publication handoff.
    verify_archive_blobs(engine, image, archive)

    expected_labels = {
        "org.opencontainers.image.source": f"https://github.com/{repository}",
        "org.opencontainers.image.revision": revision,
        "org.opencontainers.image.version": version,
        "org.opencontainers.image.licenses": "AGPL-3.0-or-later",
    }
    for architecture in ("amd64", "arm64"):
        labels = parse_object_json(
            skopeo(
                engine,
                image,
                archive,
                "inspect",
                "--override-os",
                "linux",
                "--override-arch",
                architecture,
                "--format",
                "{{json .Labels}}",
                "oci-archive:/work/image.oci.tar",
            ),
            f"OCI labels for linux/{architecture}",
        )
        for key, expected in expected_labels.items():
            if labels.get(key) != expected:
                raise VerificationError(
                    f"OCI label {key!r} for linux/{architecture} is {labels.get(key)!r}; "
                    f"expected {expected!r}"
                )

    print(f"OCI verification passed: {version} @ {revision}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check-skopeo", action="store_true")
    parser.add_argument("version", nargs="?")
    parser.add_argument("revision", nargs="?")
    parser.add_argument("archive", nargs="?", type=Path)
    args = parser.parse_args()
    try:
        if args.check_skopeo:
            if any(value is not None for value in (args.version, args.revision, args.archive)):
                parser.error("--check-skopeo does not accept release arguments")
            verify_skopeo_runtime()
            print("Skopeo runtime contract passed")
            return
        if args.version is None or args.revision is None or args.archive is None:
            parser.error("version, revision, and archive are required")
        verify(args.version, args.revision, args.archive)
    except VerificationError as exc:
        raise SystemExit(f"OCI verification failed: {exc}") from exc


if __name__ == "__main__":
    main()
