#!/usr/bin/env python3
"""Verify native release archives, checksums, legal files and embedded versions."""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import stat
import subprocess
import tarfile
import zipfile
from pathlib import Path, PurePosixPath

from github_api import ControlPlaneError, decode_json
from precommit import TLS_NAME

CHECKSUM_LINE = re.compile(r"([0-9a-fA-F]{64})[ \t]+[* ]?(.+)")
SAFE_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]*")
TARGETS = Path("scripts/tui-targets.txt")
TUI_FILES = ("LICENSE", "COPYRIGHT", "THIRD_PARTY_NOTICES.txt", "SOURCE.txt")


class VerificationError(ControlPlaneError):
    pass


def sha256_file(path: Path) -> str:
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def require_same(label: str, expected: set[str], actual: set[str]) -> None:
    if actual != expected:
        raise VerificationError(
            f"{label}: missing={sorted(expected - actual)} unexpected={sorted(actual - expected)}")


def verify_checksums(dist: Path) -> set[str]:
    path = dist / "checksums.txt"
    if path.is_symlink() or not path.is_file():
        raise VerificationError("checksums.txt is missing or not a regular file")
    names: set[str] = set()
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if (match := CHECKSUM_LINE.fullmatch(line)) is None:
            raise VerificationError(f"invalid checksums.txt line {number}: {line!r}")
        name = match.group(2)
        if SAFE_NAME.fullmatch(name) is None or name in names:
            raise VerificationError(f"unsafe or duplicate release artifact name: {name!r}")
        names.add(name)
        artifact = dist / name
        if artifact.is_symlink() or not artifact.is_file():
            raise VerificationError(f"checksummed artifact is not a regular file: {name}")
        if sha256_file(artifact) != match.group(1).lower():
            raise VerificationError(f"checksum mismatch for {name}")
    if not names:
        raise VerificationError("checksums.txt is empty")
    return names


def verify_release_file_set(dist: Path, checksummed: set[str]) -> None:
    entries = list(dist.iterdir())
    names = {entry.name for entry in entries}
    if irregular := sorted(e.name for e in entries if e.is_symlink() or not e.is_file()):
        raise VerificationError(f"release directory contains non-regular entries: {irregular}")
    require_same("release files", {*checksummed, "checksums.txt"}, names)


def tui_archives(version: str, targets: Path) -> dict[str, tuple[str, str]]:
    """Map each supported TUI archive name to its root directory and binary name."""
    archives: dict[str, tuple[str, str]] = {}
    for target in targets.read_text(encoding="utf-8").split():
        if re.fullmatch(r"[a-z0-9]+/[a-z0-9]+", target) is None:
            raise VerificationError(f"invalid TUI target: {target!r}")
        goos, goarch = target.split("/")
        base = f"graphite-meter-client_{version}_{goos}_{goarch}"
        if goos == "windows":
            archives[f"{base}.zip"] = (base, "graphite-meter-client.exe")
        else:
            archives[f"{base}.tar.gz"] = (base, "graphite-meter-client")
    return archives


def archive_names(path: Path) -> set[str]:
    """Return member names, refusing traversal, links, special files and duplicates."""
    try:
        if path.name.endswith(".tar.gz"):
            with tarfile.open(path, mode="r:gz") as tar:
                members = [(item.name, item.isfile() or item.isdir()) for item in tar.getmembers()]
        elif path.suffix == ".zip":
            regular = {0, stat.S_IFREG, stat.S_IFDIR}
            with zipfile.ZipFile(path) as archive:
                members = [(item.filename, stat.S_IFMT(item.external_attr >> 16) in regular)
                           for item in archive.infolist()]
        else:
            raise VerificationError(f"unsupported release archive type: {path}")
    except (OSError, tarfile.TarError, zipfile.BadZipFile) as exc:
        raise VerificationError(f"cannot inspect {path}: {exc}") from exc
    names: set[str] = set()
    for name, regular in members:
        if not name or "\\" in name or name.startswith("/") or ".." in PurePosixPath(name).parts:
            raise VerificationError(f"{path.name} contains unsafe archive path: {name!r}")
        if not regular:
            raise VerificationError(f"{path.name} contains a link or special entry: {name!r}")
        if (normalized := name.rstrip("/")) in names:
            raise VerificationError(f"{path.name} contains duplicate archive entry: {name!r}")
        names.add(normalized)
    return names


def member_text(archive: tarfile.TarFile, name: str) -> str:
    if (handle := archive.extractfile(name)) is None:
        raise VerificationError(f"cannot read {name}")
    return handle.read().decode("utf-8")


def verify_third_party_source_archive(dist: Path, version: str) -> None:
    root = f"graphite-meter_{version}_third-party-source"
    source = dist / f"{root}.tar.gz"
    names = archive_names(source)
    metadata = {f"{root}/{name}" for name in ("README.txt", "LEGAL_INVENTORY.json",
                                               "PROVENANCE.json")}
    upstream = (f"{root}/third_party/go/", f"{root}/third_party/npm/")
    manual = f"{root}/third_party/manual/"
    sources = {name for name in names if name.startswith((*upstream, manual))}
    if missing := sorted(metadata - names):
        raise VerificationError(f"{source.name} is missing source-offer metadata: {missing}")
    if unexpected := sorted(names - metadata - sources):
        raise VerificationError(
            f"{source.name} contains unexpected non-third-party source paths: {unexpected[:5]}")
    if not sources:
        raise VerificationError(f"{source.name} contains no third-party source material")
    # Upstream Go and npm sources may ship test keys; manually provided sources may not.
    if keys := sorted(name for name in sources
                      if TLS_NAME.search(name) and not name.startswith(upstream)):
        raise VerificationError(
            f"{source.name} contains certificate/key material outside upstream dependency source: "
            f"{keys[:5]}")
    try:
        with tarfile.open(source, mode="r:gz") as archive:
            inventory = decode_json(member_text(archive, f"{root}/LEGAL_INVENTORY.json"), root)
            provenance = decode_json(member_text(archive, f"{root}/PROVENANCE.json"), root)
            readme = member_text(archive, f"{root}/README.txt")
    except (OSError, UnicodeDecodeError, tarfile.TarError) as exc:
        raise VerificationError(f"cannot read {source.name} metadata: {exc}") from exc
    components = ("server", "tui", "container")
    if not isinstance(inventory, dict) or set(inventory) != set(components) or not all(
            isinstance(inventory[key], list) for key in components):
        raise VerificationError(f"{source.name} has an invalid LEGAL_INVENTORY.json")
    if not isinstance(provenance, list):
        raise VerificationError(f"{source.name} has an invalid PROVENANCE.json")
    for phrase in ("Source code (tar.gz)", "Source code (zip)",
                   "does not duplicate Graphite Meter's own repository source"):
        if phrase not in readme:
            raise VerificationError(f"{source.name} README does not describe the source offer")


def verify_client_archives(dist: Path, version: str, targets: Path) -> None:
    for name, (base, binary) in tui_archives(version, targets).items():
        names = archive_names(dist / name)
        if missing := sorted({f"{base}/{file}" for file in (binary, *TUI_FILES)} - names):
            raise VerificationError(f"{name} is missing: {missing}")
        if keys := sorted(entry for entry in names if TLS_NAME.search(entry)):
            raise VerificationError(f"{name} contains certificate/key material: {keys[:5]}")


def verify_client_version(version: str) -> None:
    path = Path("client/dist/version.json")
    if path.is_symlink() or not path.is_file():
        raise VerificationError(f"production client version metadata is missing: {path}")
    value = decode_json(path.read_text(encoding="utf-8"), str(path))
    fields = (value.get("version"), value.get("label")) if isinstance(value, dict) else ()
    revision = value.get("revision") if isinstance(value, dict) else None
    if fields != (version, "prod") or not isinstance(revision, str) or not revision:
        raise VerificationError(f"{path} must contain version={version}, label=prod and a revision")


def verify_server_version(version: str) -> None:
    binary = Path("go/graphite-meter")
    if binary.is_symlink() or not binary.is_file():
        raise VerificationError(f"production server binary is missing: {binary}")
    result = subprocess.run([str(binary.resolve()), "--version"], capture_output=True, text=True,
                            check=False)
    if result.returncode != 0 or result.stdout.strip() != version:
        raise VerificationError(
            f"server --version returned {result.stdout.strip()!r}; expected {version!r} "
            f"{result.stderr.strip()}")


def verify_artifacts(version: str, dist: Path) -> None:
    source = f"graphite-meter_{version}_third-party-source.tar.gz"
    checksummed = verify_checksums(dist)
    require_same("checksummed release artifacts", {source, *tui_archives(version, TARGETS)},
                 checksummed)
    verify_release_file_set(dist, checksummed)
    verify_third_party_source_archive(dist, version)
    verify_client_archives(dist, version, TARGETS)


def verify(version: str, dist: Path) -> None:
    verify_artifacts(version, dist)
    verify_client_version(version)
    verify_server_version(version)
    print(f"release asset verification passed: {version}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("version")
    version = parser.parse_args().version
    try:
        verify(version, Path(os.environ.get("RELEASE_DIST", "go/dist")))
    except (ControlPlaneError, OSError) as exc:
        raise SystemExit(f"release verification failed: {exc}") from exc


if __name__ == "__main__":
    main()
