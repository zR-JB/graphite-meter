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



def expected_rust_artifacts(version: str, selection: str) -> set[str]:
    if selection not in {"none", "server", "tui", "both"}:
        raise VerificationError("invalid Rust artifact selection")
    expected: set[str] = set()
    if selection in {"tui", "both"}:
        base = f"graphite-meter-client_{version}_linux_amd64_rust"
        expected.update({f"{base}.tar.gz", f"{base}_third-party-source.tar.gz"})
    if selection in {"server", "both"}:
        expected.add(f"graphite-meter-server_{version}_linux_amd64_rust_third-party-source.tar.gz")
    return expected


def verify_rust_server_source(dist: Path, version: str) -> None:
    source = dist / f"graphite-meter-server_{version}_linux_amd64_rust_third-party-source.tar.gz"
    names = archive_names(source)
    verify_no_certificate_material(source, names)
    if not {"inventory.json", "LEGAL.txt", "rust/vendor/PATCHES.md"} <= names:
        raise VerificationError("Rust server source offer lacks inventory, notices, or patch provenance")
    if not read_tar_text(source, "LEGAL.txt").strip():
        raise VerificationError("Rust server source offer has empty notices")
    inventory = decode_json(read_tar_text(source, "inventory.json", limit=8 * 1024 * 1024), "Rust server inventory")
    if (not isinstance(inventory, dict)
        or type(inventory.get("schemaVersion")) is not int or inventory.get("schemaVersion") != 1
        or inventory.get("package") != "graphite-meter-server"
        or inventory.get("target") != "x86_64-unknown-linux-gnu"
        or inventory.get("profile") != "release"
        or inventory.get("cargoLockSha256") != sha256_file(Path("rust/Cargo.lock"))):
        raise VerificationError("Rust server source inventory build identity mismatch")
    components = inventory.get("components")
    if not isinstance(components, list) or not 1 <= len(components) <= 4096:
        raise VerificationError("Rust server source inventory has no components")
    for item in components:
        component = item.get("component") if isinstance(item, dict) else None
        if not isinstance(component, dict):
            raise VerificationError("invalid Rust server source component")
        name, component_version = component.get("name"), component.get("version")
        if not isinstance(name, str) or not isinstance(component_version, str):
            raise VerificationError("invalid Rust server source component identity")
        if not any(path.startswith(f"third_party/cargo/{name}-{component_version}/") for path in names):
            raise VerificationError(f"Rust server source omits {name} {component_version}")


def verify_rust_client_archive(dist: Path, version: str) -> None:
    """Inspect untrusted archive data; never extract or execute its contents."""
    base = f"graphite-meter-client_{version}_linux_amd64_rust"
    archive_path = dist / f"{base}.tar.gz"
    names = archive_names(archive_path)
    files = {"graphite-meter-client", "LICENSE", "COPYRIGHT", "LEGAL.txt", "SOURCE.txt", "BUILD.json"}
    if names != {base, *(f"{base}/{name}" for name in files)}:
        raise VerificationError("Rust TUI archive contains unexpected or missing members")
    verify_no_certificate_material(archive_path, names)
    metadata = decode_json(read_tar_text(archive_path, f"{base}/BUILD.json", limit=16 * 1024), "Rust build metadata")
    if not isinstance(metadata, dict) or set(metadata) != {
        "schemaVersion", "implementation", "version", "target", "minimumGlibc", "neededLibraries", "rustc"
    }:
        raise VerificationError("invalid Rust build metadata")
    if (type(metadata["schemaVersion"]) is not int or metadata["schemaVersion"] != 1 or metadata["implementation"] != "rust"
        or metadata["version"] != f"{version}-rust"
        or metadata["target"] != "x86_64-unknown-linux-gnu"
        or not isinstance(metadata["minimumGlibc"], str)
        or re.fullmatch(r"[0-9]+(?:\.[0-9]+)+", metadata["minimumGlibc"]) is None
        or not isinstance(metadata["neededLibraries"], list)
        or not 1 <= len(metadata["neededLibraries"]) <= 32
        or not all(isinstance(name, str) and re.fullmatch(r"[A-Za-z0-9_.+-]{1,128}", name) for name in metadata["neededLibraries"])
        or not isinstance(metadata["rustc"], str) or not metadata["rustc"].startswith("rustc ")):
        raise VerificationError("Rust build metadata does not match release/target")
    report = read_tar_text(archive_path, f"{base}/LEGAL.txt")
    if not report.strip():
        raise VerificationError("Rust package has no legal report")
    source = dist / f"{base}_third-party-source.tar.gz"
    source_names = archive_names(source)
    if not {"inventory.json", "LEGAL.txt", "rust/vendor/PATCHES.md"} <= source_names:
        raise VerificationError("Rust source archive is missing inventory, notices, or patch provenance")
    if not any(name.startswith("third_party/cargo/") for name in source_names):
        raise VerificationError("Rust source archive contains no Cargo sources")
    if read_tar_text(source, "LEGAL.txt") != report:
        raise VerificationError("Rust source notices differ from binary package notices")
    inventory = decode_json(read_tar_text(source, "inventory.json", limit=8 * 1024 * 1024), "Rust source inventory")
    if (not isinstance(inventory, dict)
        or type(inventory.get("schemaVersion")) is not int
        or inventory.get("schemaVersion") != 1
        or inventory.get("target") != metadata["target"]
        or inventory.get("package") != "graphite-meter-client"
        or inventory.get("profile") != "release"
        or inventory.get("rustc") != metadata["rustc"]
        or inventory.get("cargoLockSha256") != sha256_file(Path("rust/Cargo.lock"))):
        raise VerificationError("Rust source inventory build identity mismatch")
    components = inventory.get("components")
    if not isinstance(components, list) or not 1 <= len(components) <= 4096:
        raise VerificationError("Rust source inventory has no components")
    for item in components:
        component = item.get("component") if isinstance(item, dict) else None
        if not isinstance(component, dict):
            raise VerificationError("invalid Rust source inventory component")
        name, component_version = component.get("name"), component.get("version")
        if not isinstance(name, str) or not isinstance(component_version, str):
            raise VerificationError("invalid Rust source component identity")
        prefix = f"third_party/cargo/{name}-{component_version}/"
        if not any(path.startswith(prefix) for path in source_names):
            raise VerificationError(f"Rust source archive omits {name} {component_version}")
    for filename in ("LICENSE", "COPYRIGHT"):
        if read_tar_text(archive_path, f"{base}/{filename}") != Path(filename).read_text():
            raise VerificationError(f"Rust package {filename} does not match repository")
    source_offer = read_tar_text(archive_path, f"{base}/SOURCE.txt")
    if source.name not in source_offer or metadata["minimumGlibc"] not in source_offer:
        raise VerificationError("Rust package source/platform offer is incomplete")
    with tarfile.open(archive_path, "r:gz") as archive:
        member = archive.getmember(f"{base}/graphite-meter-client")
        if not member.isfile() or not 0 < member.size <= 128 * 1024 * 1024 or not member.mode & 0o111:
            raise VerificationError("Rust executable size/mode is invalid")


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
                total_size = 0
                for member in archive:
                    if member.size < 0:
                        raise VerificationError(f"{path.name} contains a negative member size")
                    total_size += member.size
                    if len(names) >= 100_000 or total_size > 2 * 1024 * 1024 * 1024:
                        raise VerificationError(f"{path.name} exceeds archive entry/size limits")
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
            with zipfile.ZipFile(path) as zip_archive:
                for zip_member in zip_archive.infolist():
                    require_safe_archive_name(path, zip_member.filename)
                    mode = zip_member.external_attr >> 16
                    file_type = stat.S_IFMT(mode)
                    if file_type not in {0, stat.S_IFREG, stat.S_IFDIR}:
                        raise VerificationError(
                            f"{path.name} contains unsupported special entry: {zip_member.filename!r}"
                        )
                    normalized = zip_member.filename.rstrip("/")
                    if normalized in names:
                        raise VerificationError(
                            f"{path.name} contains duplicate archive entry: {zip_member.filename!r}"
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


def read_tar_text(path: Path, member_name: str, *, limit: int = 16 * 1024 * 1024) -> str:
    try:
        with tarfile.open(path, mode="r:gz") as archive:
            member = archive.getmember(member_name)
            if not member.isfile() or not 0 <= member.size <= limit:
                raise VerificationError(f"{path.name} metadata exceeds limit or is not regular: {member_name}")
            handle = archive.extractfile(member)
            if handle is None:
                raise VerificationError(f"{path.name} cannot read metadata member: {member_name}")
            return handle.read(limit + 1).decode("utf-8")
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


def verify(version: str, dist: Path, rust_artifacts: str = "none") -> None:
    if not dist.is_dir():
        raise VerificationError(f"release directory does not exist: {dist}")
    targets_file = Path("scripts/tui-targets.txt")
    checksummed = verify_checksums(dist)
    expected = expected_release_artifacts(version, targets_file) | expected_rust_artifacts(version, rust_artifacts)
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
    if rust_artifacts in {"tui", "both"}:
        verify_rust_client_archive(dist, version)
    if rust_artifacts in {"server", "both"}:
        verify_rust_server_source(dist, version)
    verify_client_version(version)
    verify_server_version(version)
    print(f"release asset verification passed: {version}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("version")
    parser.add_argument("--rust-artifacts", choices=("none", "server", "tui", "both"), default="none")
    args = parser.parse_args()
    dist = Path(os.environ.get("RELEASE_DIST", "go/dist"))
    try:
        verify(args.version, dist, args.rust_artifacts)
    except (VerificationError, JsonShapeError, subprocess.SubprocessError, OSError) as exc:
        raise SystemExit(f"release verification failed: {exc}") from exc


if __name__ == "__main__":
    main()
