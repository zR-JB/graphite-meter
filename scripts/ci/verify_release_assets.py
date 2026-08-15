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
import tempfile
import time
import urllib.error
import urllib.request
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

    expected = {f"graphite-meter_{version}_corresponding-source.tar.gz"}
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
                    if stat.S_ISLNK(mode):
                        raise VerificationError(
                            f"{path.name} contains unsupported symlink entry: {member.filename!r}"
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


def verify_source_archive(dist: Path, version: str) -> None:
    source = dist / f"graphite-meter_{version}_corresponding-source.tar.gz"
    if not source.is_file() or source.stat().st_size == 0:
        raise VerificationError(f"corresponding-source archive is missing or empty: {source}")
    names = archive_names(source)
    verify_no_certificate_material(source, names)


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
        required = {
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
    if not path.is_file():
        return
    value = parse_json(path)
    if not isinstance(value, dict):
        raise VerificationError(f"{path} must contain a JSON object")
    if value.get("version") != f"{version}+prod" or value.get("label") != "prod":
        raise VerificationError(
            f"{path} must contain version={version}+prod and label=prod"
        )


def verify_server_version(version: str) -> None:
    binary = Path("go/graphite-meter")
    if not binary.is_file():
        return

    environment = os.environ.copy()
    environment.update(
        {
            "GM_H1_ADDR": "127.0.0.1:7246",
            "GM_H1_TLS_ADDR": "",
            "GM_H2_ADDR": "",
            "GM_H3_ADDR": "",
        }
    )

    with tempfile.TemporaryDirectory() as temp_dir:
        log_path = Path(temp_dir) / "server.log"
        with log_path.open("wb") as log_handle:
            process = subprocess.Popen(
                [str(binary.resolve())],
                stdout=log_handle,
                stderr=subprocess.STDOUT,
                env=environment,
            )
            try:
                payload: JsonValue | None = None
                for _ in range(30):
                    if process.poll() is not None:
                        break
                    try:
                        with urllib.request.urlopen(
                            "http://127.0.0.1:7246/preflight", timeout=1.0
                        ) as response:
                            body = response.read().decode("utf-8")
                        payload = decode_json(body, "/preflight response")
                        break
                    except (urllib.error.URLError, TimeoutError, JsonShapeError):
                        time.sleep(1)
                if not isinstance(payload, dict):
                    detail = log_path.read_text(encoding="utf-8", errors="replace")
                    raise VerificationError(
                        "server did not expose a valid /preflight response\n" + detail
                    )
                if payload.get("engineVersion") != version:
                    raise VerificationError(
                        f"server engineVersion is {payload.get('engineVersion')!r}; expected {version!r}"
                    )
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)


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
    verify_source_archive(dist, version)
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
