#!/usr/bin/env python3
"""Verify native release artifacts, checksums, legal files, and embedded versions.

This replaces the previous shell verifier with stdlib-only Python so the control
plane has one testable implementation language and no jq/tar/unzip/curl parsing.
"""

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
from github_api import JsonShapeError, JsonValue, decode_json

FORBIDDEN_NAME = re.compile(
    r"(^|/)(\.dev-certs|certs?|certificates?|letsencrypt)(/|$)|"
    r"\.(pem|key|crt|cer|der|csr|p12|pfx|pkcs8|jks|keystore)$",
    re.IGNORECASE,
)
CHECKSUM_LINE = re.compile(r"^([0-9a-fA-F]{64})[ \t]+[* ]?(.+)$")
SAFE_RELEASE_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._+-]*$")


class VerificationError(RuntimeError):
    pass


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_json(path: Path) -> JsonValue:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise VerificationError(f"cannot read JSON {path}: {exc}") from exc
    try:
        return decode_json(text, str(path))
    except JsonShapeError as exc:
        raise VerificationError(str(exc)) from exc


def safe_checksum_path(dist: Path, name: str) -> Path:
    pure = PurePosixPath(name)
    if pure.is_absolute() or ".." in pure.parts or name in {"", "."}:
        raise VerificationError(f"unsafe checksums.txt path: {name!r}")
    if len(pure.parts) != 1 or SAFE_RELEASE_NAME.fullmatch(name) is None:
        raise VerificationError(f"unsafe release artifact name: {name!r}")
    path = dist.joinpath(*pure.parts)
    try:
        path.resolve().relative_to(dist.resolve())
    except ValueError as exc:
        raise VerificationError(f"checksums.txt path escapes release directory: {name!r}") from exc
    return path


def verify_checksums(dist: Path) -> set[str]:
    checksum_file = dist / "checksums.txt"
    if not checksum_file.is_file() or checksum_file.is_symlink():
        raise VerificationError("checksums.txt is missing or not a regular file")
    try:
        lines = checksum_file.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        raise VerificationError(f"cannot read {checksum_file}: {exc}") from exc
    if not lines:
        raise VerificationError("checksums.txt is empty")

    seen: set[str] = set()
    for line_number, line in enumerate(lines, 1):
        match = CHECKSUM_LINE.fullmatch(line)
        if match is None:
            raise VerificationError(f"invalid checksums.txt line {line_number}: {line!r}")
        expected = match.group(1).lower()
        name = match.group(2)
        path = safe_checksum_path(dist, name)
        if name in seen:
            raise VerificationError(f"duplicate checksums.txt entry: {name}")
        seen.add(name)
        if not path.is_file() or path.is_symlink():
            raise VerificationError(f"checksummed artifact is missing or not a regular file: {name}")
        actual = sha256_file(path)
        if actual != expected:
            raise VerificationError(
                f"checksum mismatch for {name}: expected {expected}, got {actual}"
            )
    return seen


def verify_release_file_set(dist: Path, checksummed: set[str]) -> None:
    expected = {*checksummed, "checksums.txt"}
    actual: set[str] = set()
    invalid: list[str] = []
    try:
        entries = list(dist.iterdir())
    except OSError as exc:
        raise VerificationError(f"cannot enumerate release directory {dist}: {exc}") from exc
    for entry in entries:
        if entry.is_symlink() or not entry.is_file():
            invalid.append(entry.name)
        else:
            actual.add(entry.name)
    if invalid:
        raise VerificationError(
            "release directory contains non-regular entries: " + ", ".join(sorted(invalid))
        )
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        detail: list[str] = []
        if missing:
            detail.append("missing=" + ",".join(missing))
        if extra:
            detail.append("unchecksummed=" + ",".join(extra))
        raise VerificationError("release file set does not match checksums.txt: " + "; ".join(detail))


def expected_release_artifacts(version: str, targets_file: Path) -> set[str]:
    try:
        targets = targets_file.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        raise VerificationError(f"cannot read {targets_file}: {exc}") from exc

    expected = {f"graphite-meter_{version}_third-party-source.tar.gz"}
    for raw_target in targets:
        target = raw_target.strip()
        if not target:
            continue
        if target.count("/") != 1:
            raise VerificationError(f"invalid TUI target: {target!r}")
        goos, goarch = target.split("/", 1)
        if not re.fullmatch(r"[a-z0-9]+", goos) or not re.fullmatch(r"[a-z0-9]+", goarch):
            raise VerificationError(f"invalid TUI target: {target!r}")
        base = f"graphite-meter-client_{version}_{goos}_{goarch}"
        expected.add(f"{base}.zip" if goos == "windows" else f"{base}.tar.gz")
    return expected


def require_safe_archive_name(archive: Path, name: str) -> None:
    pure = PurePosixPath(name)
    if (
        not name
        or "\\" in name
        or pure.is_absolute()
        or any(part in {"", ".", ".."} for part in pure.parts)
    ):
        raise VerificationError(f"{archive.name} contains unsafe archive path: {name!r}")


def archive_names(path: Path) -> set[str]:
    names: set[str] = set()
    if path.name.endswith(".tar.gz"):
        try:
            with tarfile.open(path, mode="r:gz") as archive:
                for member in archive.getmembers():
                    require_safe_archive_name(path, member.name)
                    if not (member.isfile() or member.isdir()):
                        raise VerificationError(
                            f"{path.name} contains unsupported link/device entry: {member.name!r}"
                        )
                    normalized = member.name.rstrip("/")
                    if normalized in names:
                        raise VerificationError(
                            f"{path.name} contains duplicate archive entry: {member.name!r}"
                        )
                    names.add(normalized)
                return names
        except (OSError, tarfile.TarError) as exc:
            raise VerificationError(f"cannot inspect {path}: {exc}") from exc
    if path.suffix == ".zip":
        try:
            with zipfile.ZipFile(path) as archive:
                for member in archive.infolist():
                    require_safe_archive_name(path, member.filename)
                    mode = member.external_attr >> 16
                    file_type = stat.S_IFMT(mode)
                    if file_type not in {0, stat.S_IFREG, stat.S_IFDIR}:
                        raise VerificationError(
                            f"{path.name} contains unsupported special entry: {member.filename!r}"
                        )
                    normalized = member.filename.rstrip("/")
                    if normalized in names:
                        raise VerificationError(
                            f"{path.name} contains duplicate archive entry: {member.filename!r}"
                        )
                    names.add(normalized)
                return names
        except (OSError, zipfile.BadZipFile) as exc:
            raise VerificationError(f"cannot inspect {path}: {exc}") from exc
    raise VerificationError(f"unsupported release archive type: {path}")


def verify_no_certificate_material(path: Path, names: set[str]) -> None:
    bad = sorted(name for name in names if FORBIDDEN_NAME.search(name))
    if bad:
        raise VerificationError(
            f"{path.name} contains certificate/key material: {', '.join(bad[:5])}"
        )


def read_tar_text(path: Path, member_name: str) -> str:
    try:
        with tarfile.open(path, mode="r:gz") as archive:
            member = archive.getmember(member_name)
            handle = archive.extractfile(member)
            if handle is None:
                raise VerificationError(f"{path.name} cannot read metadata member: {member_name}")
            return handle.read().decode("utf-8")
    except (KeyError, OSError, UnicodeDecodeError, tarfile.TarError) as exc:
        raise VerificationError(f"cannot read {member_name} from {path.name}: {exc}") from exc


def verify_third_party_source_archive(dist: Path, version: str) -> None:
    source = dist / f"graphite-meter_{version}_third-party-source.tar.gz"
    if not source.is_file() or source.is_symlink() or source.stat().st_size == 0:
        raise VerificationError(f"third-party source archive is missing, empty, or not regular: {source}")

    root = f"graphite-meter_{version}_third-party-source"
    names = archive_names(source)
    required = {
        f"{root}/README.txt",
        f"{root}/LEGAL_INVENTORY.json",
        f"{root}/PROVENANCE.json",
    }
    missing = sorted(required - names)
    if missing:
        raise VerificationError(
            f"{source.name} is missing source-offer metadata: {', '.join(missing)}"
        )

    dependency_prefixes = (
        f"{root}/third_party/go/",
        f"{root}/third_party/npm/",
    )
    manual_prefix = f"{root}/third_party/manual/"
    unexpected = sorted(
        name
        for name in names
        if name not in required
        and not name.startswith(dependency_prefixes)
        and not name.startswith(manual_prefix)
    )
    if unexpected:
        raise VerificationError(
            f"{source.name} contains unexpected non-third-party source paths: "
            + ", ".join(unexpected[:5])
        )
    if not any(
        name.startswith(dependency_prefixes) or name.startswith(manual_prefix)
        for name in names
    ):
        raise VerificationError(f"{source.name} contains no third-party source material")

    # Public upstream source distributions may legitimately contain test
    # certificates and test private keys. Keep the credential-leak invariant
    # strict for Graphite Meter-controlled manual provenance, while not treating
    # immutable Go/npm upstream fixtures as repository secrets.
    bad = sorted(
        name
        for name in names
        if FORBIDDEN_NAME.search(name)
        and not name.startswith(dependency_prefixes)
    )
    if bad:
        raise VerificationError(
            f"{source.name} contains certificate/key material outside upstream dependency source: "
            + ", ".join(bad[:5])
        )

    try:
        inventory = decode_json(
            read_tar_text(source, f"{root}/LEGAL_INVENTORY.json"),
            f"{source.name}:LEGAL_INVENTORY.json",
        )
        provenance = decode_json(
            read_tar_text(source, f"{root}/PROVENANCE.json"),
            f"{source.name}:PROVENANCE.json",
        )
    except JsonShapeError as exc:
        raise VerificationError(str(exc)) from exc
    if not isinstance(inventory, dict) or set(inventory) != {"server", "tui", "container"}:
        raise VerificationError(f"{source.name} has invalid LEGAL_INVENTORY.json shape")
    if any(not isinstance(inventory[key], list) for key in ("server", "tui", "container")):
        raise VerificationError(f"{source.name} has invalid LEGAL_INVENTORY.json component lists")
    if not isinstance(provenance, list):
        raise VerificationError(f"{source.name} has invalid PROVENANCE.json shape")

    readme = read_tar_text(source, f"{root}/README.txt")
    for required_text in (
        "Source code (tar.gz)",
        "Source code (zip)",
        "does not duplicate Graphite Meter's own repository source",
    ):
        if required_text not in readme:
            raise VerificationError(
                f"{source.name} README does not describe the split source offer: {required_text!r}"
            )


def verify_client_archives(dist: Path, version: str, targets_file: Path) -> None:
    try:
        targets = targets_file.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        raise VerificationError(f"cannot read {targets_file}: {exc}") from exc

    for raw_target in targets:
        target = raw_target.strip()
        if not target:
            continue
        if target.count("/") != 1:
            raise VerificationError(f"invalid TUI target: {target!r}")
        goos, goarch = target.split("/", 1)
        base = f"graphite-meter-client_{version}_{goos}_{goarch}"
        path = dist / (f"{base}.zip" if goos == "windows" else f"{base}.tar.gz")
        if not path.is_file() or path.stat().st_size == 0:
            raise VerificationError(f"release archive is missing or empty: {path}")

        names = archive_names(path)
        binary_name = "graphite-meter-client.exe" if goos == "windows" else "graphite-meter-client"
        required = {
            f"{base}/{binary_name}",
            f"{base}/LICENSE",
            f"{base}/COPYRIGHT",
            f"{base}/THIRD_PARTY_NOTICES.txt",
            f"{base}/SOURCE.txt",
        }
        missing = sorted(required - names)
        if missing:
            raise VerificationError(f"{path.name} is missing: {', '.join(missing)}")
        verify_no_certificate_material(path, names)


def verify_client_version(version: str) -> None:
    path = Path("client/dist/version.json")
    if not path.is_file() or path.is_symlink():
        raise VerificationError(f"production client version metadata is missing: {path}")
    value = parse_json(path)
    if not isinstance(value, dict):
        raise VerificationError(f"{path} must contain a JSON object")
    if (
        value.get("version") != version
        or value.get("label") != "prod"
        or not isinstance(value.get("revision"), str)
        or not value["revision"]
    ):
        raise VerificationError(
            f"{path} must contain version={version}, label=prod, and a revision"
        )


def verify_server_version(version: str) -> None:
    binary = Path("go/graphite-meter")
    if not binary.is_file() or binary.is_symlink():
        raise VerificationError(f"production server binary is missing: {binary}")

    result = subprocess.run(
        [str(binary.resolve()), "--version"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise VerificationError(f"server --version failed: {detail}")
    actual = result.stdout.strip()
    if actual != version:
        raise VerificationError(f"server version is {actual!r}; expected {version!r}")


def verify(version: str, dist: Path) -> None:
    if not dist.is_dir():
        raise VerificationError(f"release directory does not exist: {dist}")
    targets_file = Path("scripts/tui-targets.txt")
    checksummed = verify_checksums(dist)
    expected = expected_release_artifacts(version, targets_file)
    if checksummed != expected:
        missing = sorted(expected - checksummed)
        extra = sorted(checksummed - expected)
        detail: list[str] = []
        if missing:
            detail.append("missing=" + ",".join(missing))
        if extra:
            detail.append("unexpected=" + ",".join(extra))
        raise VerificationError("checksums.txt release artifact set is unexpected: " + "; ".join(detail))
    verify_release_file_set(dist, checksummed)
    verify_third_party_source_archive(dist, version)
    verify_client_archives(dist, version, targets_file)
    verify_client_version(version)
    verify_server_version(version)
    print(f"release asset verification passed: {version}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("version")
    args = parser.parse_args()
    dist = Path(os.environ.get("RELEASE_DIST", "go/dist"))
    try:
        verify(args.version, dist)
    except VerificationError as exc:
        raise SystemExit(f"release verification failed: {exc}") from exc


if __name__ == "__main__":
    main()
