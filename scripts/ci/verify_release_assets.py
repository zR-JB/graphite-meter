#!/usr/bin/env python3
"""Verify native release archives, checksums, legal files and embedded versions."""

from __future__ import annotations

import argparse
import os
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
import zipfile
from pathlib import Path, PurePosixPath

from .github_api import (
    TLS_NAME, ControlPlaneError, decode_json, expect_array, expect_object, fail, file_sha256,
    int_field, local_path, object_field, str_field, write_checksums,
)
from ..legal.model import manual_files, manual_sources
from .toolchains import host_platform, tui_targets
from .verify_oci import BLOB_LIMIT, source_commit

CHECKSUM_LINE = re.compile(r"([0-9a-fA-F]{64})[ \t]+[* ]?(.+)")
SAFE_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]*")
TARGETS = Path("scripts/tui-targets.txt")
TUI_FILES = ("LICENSE", "COPYRIGHT", "THIRD_PARTY_NOTICES.txt", "SOURCE.txt")


def require_same(label: str, expected: set[str], actual: set[str]) -> None:
    if actual != expected:
        fail(
            f"{label}: missing={sorted(expected - actual)} unexpected={sorted(actual - expected)}")


def verify_checksums(dist: Path) -> set[str]:
    path = dist / "checksums.txt"
    if path.is_symlink() or not path.is_file():
        fail("checksums.txt is missing or not a regular file")
    names: set[str] = set()
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if (match := CHECKSUM_LINE.fullmatch(line)) is None:
            fail(f"invalid checksums.txt line {number}: {line!r}")
        name = match.group(2)
        if SAFE_NAME.fullmatch(name) is None or name in names:
            fail(f"unsafe or duplicate release artifact name: {name!r}")
        names.add(name)
        artifact = dist / name
        if artifact.is_symlink() or not artifact.is_file():
            fail(f"checksummed artifact is not a regular file: {name}")
        if file_sha256(artifact) != match.group(1).lower():
            fail(f"checksum mismatch for {name}")
    if not names:
        fail("checksums.txt is empty")
    return names


def verify_release_file_set(dist: Path, checksummed: set[str]) -> None:
    entries = list(dist.iterdir())
    names = {entry.name for entry in entries}
    if irregular := sorted(e.name for e in entries if e.is_symlink() or not e.is_file()):
        fail(f"release directory contains non-regular entries: {irregular}")
    require_same("release files", {*checksummed, "checksums.txt"}, names)


def merge(source: Path, destination: Path) -> set[str]:
    """Copy the checksummed files of one untrusted artifact, each name arriving only once."""
    names = verify_checksums(source)
    verify_release_file_set(source, names)
    destination.mkdir(parents=True, exist_ok=True)
    for name in names:
        if (destination / name).exists():
            fail(f"{name} arrives in more than one artifact")
        shutil.copyfile(source / name, destination / name)
    return names


def tui_archive(version: str, platform: str, marker: str = "") -> tuple[str, str, str]:
    goos, goarch = platform.split("/")
    base = f"graphite-meter-client_{version}_{goos}_{goarch}{marker}"
    if goos == "windows":
        return f"{base}.zip", base, "graphite-meter-client.exe"
    return f"{base}.tar.gz", base, "graphite-meter-client"


def tui_archives(version: str, targets: Path) -> dict[str, tuple[str, str]]:
    """Map each supported TUI archive name to its root directory and binary name."""
    archives = (tui_archive(version, platform) for platform in tui_targets(targets))
    return {name: (base, binary) for name, base, binary in archives}


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
            fail(f"unsupported release archive type: {path}")
    except (OSError, tarfile.TarError, zipfile.BadZipFile) as exc:
        raise ControlPlaneError(f"cannot inspect {path}: {exc}") from exc
    names: set[str] = set()
    for name, regular in members:
        if not name or "\\" in name or name.startswith("/") or ".." in PurePosixPath(name).parts:
            fail(f"{path.name} contains unsafe archive path: {name!r}")
        if not regular:
            fail(f"{path.name} contains a link or special entry: {name!r}")
        if (normalized := name.rstrip("/")) in names:
            fail(f"{path.name} contains duplicate archive entry: {name!r}")
        names.add(normalized)
    return names


def member(archive: tarfile.TarFile, name: str) -> bytes:
    if (handle := archive.extractfile(name)) is None:
        fail(f"cannot read {name}")
    return handle.read()


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
        fail(f"{source.name} is missing source-offer metadata: {missing}")
    if unexpected := sorted(names - metadata - sources):
        fail(
            f"{source.name} contains unexpected non-third-party source paths: {unexpected[:5]}")
    if not sources:
        fail(f"{source.name} contains no third-party source material")
    # Upstream Go and npm sources may ship test keys; manually provided sources may not.
    if keys := sorted(name for name in sources
                      if TLS_NAME.search(name) and not name.startswith(upstream)):
        fail(
            f"{source.name} contains certificate/key material outside upstream dependency source: "
            f"{keys[:5]}")
    try:
        with tarfile.open(source, mode="r:gz") as archive:
            inventory = decode_json(member(archive, f"{root}/LEGAL_INVENTORY.json").decode(), root)
            provenance = decode_json(member(archive, f"{root}/PROVENANCE.json").decode(), root)
            readme = member(archive, f"{root}/README.txt").decode()
    except (OSError, UnicodeDecodeError, tarfile.TarError) as exc:
        raise ControlPlaneError(f"cannot read {source.name} metadata: {exc}") from exc
    components = ("server", "tui", "container")
    if not isinstance(inventory, dict) or set(inventory) != set(components) or not all(
            isinstance(inventory[key], list) for key in components):
        fail(f"{source.name} has an invalid LEGAL_INVENTORY.json")
    if not isinstance(provenance, list):
        fail(f"{source.name} has an invalid PROVENANCE.json")
    for phrase in ("Source code (tar.gz)", "Source code (zip)",
                   "does not duplicate Graphite Meter's own repository source"):
        if phrase not in readme:
            fail(f"{source.name} README does not describe the source offer")


def verify_client_archives(dist: Path, version: str, targets: Path) -> None:
    for name, (base, binary) in tui_archives(version, targets).items():
        names = archive_names(dist / name)
        if missing := sorted({f"{base}/{file}" for file in (binary, *TUI_FILES)} - names):
            fail(f"{name} is missing: {missing}")
        if keys := sorted(entry for entry in names if TLS_NAME.search(entry)):
            fail(f"{name} contains certificate/key material: {keys[:5]}")


def verify_tui_version(version: str, dist: Path) -> None:
    """Run the archived TUI built for this host; the trusted consumer never executes candidates."""
    host = f"graphite-meter-client_{version}_{host_platform().replace('/', '_')}"
    archives = [(name, binary) for name, (base, binary) in tui_archives(version, TARGETS).items()
                if base == host]
    if not archives:
        fail(f"no TUI archive runs on this host: {host}")
    name, binary = archives[0]
    with tempfile.TemporaryDirectory() as directory:
        executable = Path(directory) / binary
        if name.endswith(".zip"):
            with zipfile.ZipFile(dist / name) as archive:
                executable.write_bytes(archive.read(f"{host}/{binary}"))
        else:
            with tarfile.open(dist / name, mode="r:gz") as tar:
                executable.write_bytes(member(tar, f"{host}/{binary}"))
        executable.chmod(0o755)
        result = subprocess.run([executable, "--version"], capture_output=True, text=True,
                                check=False)
    if result.stdout.strip() != f"graphite-meter-client {version}":
        fail(f"{name} reports {result.stdout.strip()!r} {result.stderr.strip()}")


MACHINES = {"x86_64": (62, 0x8664, 0x01000007), "aarch64": (183, 0xAA64, 0x0100000C)}


def native_executable(data: bytes, target: str) -> bool:
    elf, pe, macho = MACHINES.get(target.split("-")[0], (0, 0, 0))

    def field(offset: int, size: int) -> int:
        return int.from_bytes(data[offset:offset + size], "little")

    if "-windows-" in target:
        start = field(0x3C, 4)
        return data[:2] == b"MZ" and data[start:start + 4] == b"PE\0\0" and field(start + 4, 2) == pe
    if "-apple-" in target:
        return field(0, 4) == 0xFEEDFACF and field(4, 4) == macho
    return (data[:7] == b"\x7fELF\x02\x01\x01" and field(16, 2) in (2, 3) and field(18, 2) == elf
            and field(20, 4) == 1 and field(52, 2) == 64)


def rust_builds(selection: str) -> tuple[list[str], list[str]]:
    """The platforms a Rust selection builds the server for, and those it builds the TUI for."""
    if selection not in ("none", "server", "tui", "both"):
        fail("invalid Rust artifact selection")
    platforms = list(tui_targets(TARGETS))
    server = [platform for platform in platforms if platform.startswith("linux/")]
    return (server if selection in ("server", "both") else []), (platforms if selection in ("tui", "both") else [])


def rust_files(version: str, package: str, platform: str) -> set[str]:
    """The archive and matching source offer of one Rust build; the server image ships only its source."""
    base = f"graphite-meter-server_{version}_{platform.replace('/', '_')}_rust"
    if package == "graphite-meter-server":
        return {f"{base}_third-party-source.tar.gz"}
    name, base, _ = tui_archive(version, platform, "_rust")
    return {name, f"{base}_third-party-source.tar.gz"}


def rust_statements(version: str, server: list[str], tui: list[str]) -> dict[str, set[str]]:
    """BuildKit's provenance statement of each Docker export, named as released, and the files it attests.

    Each server platform is its own export; one export holds every TUI but the natively built macOS ones.
    """
    statements = {f"graphite-meter-server_{version}_{platform.replace('/', '_')}_rust.provenance.json":
                  rust_files(version, "graphite-meter-server", platform) for platform in server}
    if docker := [platform for platform in tui if not platform.startswith("darwin/")]:
        statements[f"graphite-meter-client_{version}_rust.provenance.json"] = set().union(
            *(rust_files(version, "graphite-meter-client", platform) for platform in docker))
    return statements


def expected_rust_artifacts(version: str, server: list[str], tui: list[str]) -> set[str]:
    statements = rust_statements(version, server, tui)
    return set(statements).union(*statements.values(), *(
        rust_files(version, "graphite-meter-client", platform) for platform in tui))


def stage_rust(export: Path, dist: Path, version: str, server: list[str], tui: list[str]) -> None:
    """Copy exactly the Docker-built artifacts and their provenance statements out of `export`, then checksum them.

    BuildKit writes a statement as provenance.json beside the files it attests, in one directory per platform
    when an export has several; everything else in the export stays behind.
    """
    statements = rust_statements(version, server, tui)
    wanted = set().union(*statements.values())
    dist.mkdir(parents=True, exist_ok=True)
    for path in sorted(export.rglob("*")):
        if path.is_symlink() or not path.is_file():
            continue
        name = path.name if path.name in wanted else None
        if path.name == "provenance.json":
            beside = {entry.name for entry in path.parent.iterdir()}
            if len(names := [name for name, files in statements.items() if files <= beside]) != 1:
                fail(f"{path} attests no single expected Rust export")
            name = names[0]
        if name is not None:
            if (dist / name).exists():
                fail(f"the Rust export holds {name} twice")
            shutil.copyfile(path, dist / name)
    if missing := sorted((set(statements) | wanted) - {entry.name for entry in dist.iterdir()}):
        fail(f"the Rust export lacks {missing}")
    write_checksums(dist)


def read_archive(path: Path, name: str, limit: int = 4 * 1024 * 1024) -> bytes:
    try:
        if path.suffix == ".zip":
            with zipfile.ZipFile(path) as archive:
                info = archive.getinfo(name)
                if info.is_dir() or info.file_size > limit:
                    fail(f"{name} is not a regular file or exceeds limit {limit}")
                return archive.read(info)
        with tarfile.open(path, "r:gz") as archive:
            item = archive.getmember(name)
            if not item.isfile() or item.size > limit:
                fail(f"{name} is not a regular file or exceeds limit {limit}")
            return member(archive, name)
    except (KeyError, OSError, tarfile.TarError, zipfile.BadZipFile) as exc:
        raise ControlPlaneError(f"cannot read {path.name}/{name}: {exc}") from exc


def read_archive_text(path: Path, name: str) -> str:
    try:
        return read_archive(path, name).decode()
    except UnicodeDecodeError as exc:
        raise ControlPlaneError(f"cannot read {path.name}/{name}: {exc}") from exc


def verify_rust_source(path: Path, package: str, target: str, lock_sha256: str | None = None) -> None:
    names = archive_names(path)
    inventory = expect_object(decode_json(read_archive_text(path, "inventory.json"), path.name), path.name)
    if int_field(inventory, "schemaVersion", path.name) != 1 or any(
            inventory.get(key) != value for key, value in {
        "package": package, "profile": "release", "target": target,
    }.items()):
        fail(f"{path.name} has invalid Rust build identity")
    lock = inventory.get("cargoLockSha256")
    if not isinstance(lock, str) or re.fullmatch(r"[0-9a-f]{64}", lock) is None:
        fail("invalid Rust Cargo lock identity")
    if lock != (lock_sha256 or file_sha256(Path("rust/Cargo.lock"))):
        fail("Rust source inventory does not match release Cargo lock")
    components = expect_array(inventory.get("components"), "Rust components")
    if not components:
        fail(f"{path.name} has no valid dependency inventory")
    trees: set[str] = set()
    for item in components:
        component = object_field(expect_object(item, "Rust dependency"), "component", "Rust dependency")
        name = str_field(component, "name", "Rust dependency")
        version = str_field(component, "version", "Rust dependency")
        trees.add(f"third_party/cargo/{name}-{version}/")
    manual: list[tuple[str, ...]] = []
    for key in ("browserComponents", "imageComponents"):
        for item in expect_array(inventory.get(key, []), f"Rust {key}"):
            component = expect_object(item, f"Rust {key}")
            identity = tuple(str_field(component, field, f"Rust {key}") for field in ("ecosystem", "name", "version"))
            if identity[0] == "npm":
                trees.add(f"third_party/npm/{identity[1]}-{identity[2]}/")
            else:
                manual.append(identity)
    if decode_json(read_archive_text(path, "legal/rust-forks.json"), "Rust forks") != decode_json(
            Path("legal/rust-forks.json").read_text(), "release Rust forks"):
        fail("Rust source fork identities differ from release tooling")
    try:
        reviewed: dict[tuple[str, ...], set[str]] = {
            (entry.ecosystem, entry.name, entry.version): set(manual_files(entry))
            for entry in manual_sources(Path("."), package)}
    except ValueError as exc:
        raise ControlPlaneError(f"invalid reviewed provenance: {exc}") from exc
    allowed = {"inventory.json", "LEGAL.txt", "legal/rust-forks.json"}.union(*reviewed.values())
    for identity in manual:
        if identity not in reviewed:
            fail("Rust inventory contains unreviewed manual source")
        if missing := reviewed[identity] - names:
            fail(f"Rust source offer is missing reviewed inputs: {sorted(missing)}")
    for tree in trees:
        if not any(name.startswith(tree) for name in names):
            fail(f"{path.name} lacks declared dependency source {tree}")
    for name in names:
        upstream = any(name.startswith(tree) or name == tree.rstrip("/") for tree in trees)
        directory = any(tree.startswith(name + "/") for tree in trees)
        if not upstream and not directory and name not in allowed:
            fail(f"{path.name} contains undeclared source {name}")
        if TLS_NAME.search(name) and not upstream:
            fail(f"{path.name} contains certificate/key material outside dependency source")
    if not read_archive_text(path, "LEGAL.txt").strip():
        fail("Rust source offer has empty notices")


def verify_rust_client_archive(dist: Path, version: str, platform: str, target: str,
                               lock_sha256: str | None = None) -> None:
    name, base, binary = tui_archive(version, platform, "_rust")
    path = dist / name
    require_same("Rust TUI archive files", {base, *(f"{base}/{file}" for file in (binary, *TUI_FILES))},
                 archive_names(path))
    if not native_executable(read_archive(path, f"{base}/{binary}", 128 * 1024 * 1024), target):
        fail(f"{name} does not hold a {target} executable")
    source = f"{base}_third-party-source.tar.gz"
    verify_rust_source(dist / source, "graphite-meter-client", target, lock_sha256)
    for filename in ("LICENSE", "COPYRIGHT"):
        if read_archive_text(path, f"{base}/{filename}") != Path(filename).read_text():
            fail(f"Rust TUI {filename} differs from release source")
    if source not in read_archive_text(path, f"{base}/SOURCE.txt"):
        fail("Rust TUI source notice lacks matching source archive")
    if read_archive_text(path, f"{base}/THIRD_PARTY_NOTICES.txt") != read_archive_text(dist / source, "LEGAL.txt"):
        fail("Rust TUI notices differ from source offer")


def verify_rust_provenance(dist: Path, name: str, files: set[str], commit: str, repository: str) -> None:
    """Require a BuildKit statement to attest exactly `files` as they are, built from `commit` of `repository`."""
    if (dist / name).stat().st_size > BLOB_LIMIT:
        fail(f"{name} exceeds {BLOB_LIMIT} bytes")
    statement = expect_object(decode_json((dist / name).read_bytes().decode(errors="replace"), name), name)
    if statement.get("_type") not in ("https://in-toto.io/Statement/v0.1", "https://in-toto.io/Statement/v1"):
        fail(f"{name} is not an in-toto statement")
    subjects = [expect_object(item, name) for item in expect_array(statement.get("subject"), f"{name} subject")]
    attested = {str_field(subject, "name", name): str_field(object_field(subject, "digest", name), "sha256", name)
                for subject in subjects}
    if len(attested) != len(subjects) or attested != {file: file_sha256(dist / file) for file in files}:
        fail(f"{name} does not attest exactly {sorted(files)} as released")
    if (source := source_commit(statement, repository)) != commit:
        fail(f"{name} records source {source}, not {commit}")


def verify_rust_artifacts(dist: Path, version: str, server: list[str], tui: list[str],
                          lock_sha256: str | None = None) -> None:
    targets = tui_targets(TARGETS)
    for platform in server:
        source, = rust_files(version, "graphite-meter-server", platform)
        verify_rust_source(dist / source, "graphite-meter-server", targets[platform], lock_sha256)
    for platform in tui:
        verify_rust_client_archive(dist, version, platform, targets[platform], lock_sha256)


def verify_rust(parts: list[Path], assets: Path, version: str, server: list[str], tui: list[str],
                commit: str, repository: str, lock_sha256: str | None = None) -> None:
    """Merge the untrusted Rust artifacts into `assets` and verify them as a release requires."""
    names = set().union(*(merge(part, assets) for part in parts))
    require_same("Rust artifacts", expected_rust_artifacts(version, server, tui), names)
    for name, files in rust_statements(version, server, tui).items():
        verify_rust_provenance(assets, name, files, commit, repository)
    verify_rust_artifacts(assets, version, server, tui, lock_sha256)


def verify_artifacts(version: str, dist: Path) -> None:
    source = f"graphite-meter_{version}_third-party-source.tar.gz"
    checksummed = verify_checksums(dist)
    require_same("checksummed release artifacts", {source, *tui_archives(version, TARGETS)}, checksummed)
    verify_release_file_set(dist, checksummed)
    verify_third_party_source_archive(dist, version)
    verify_client_archives(dist, version, TARGETS)


def verify(version: str, dist: Path) -> None:
    verify_artifacts(version, dist)
    verify_tui_version(version, dist)
    print(f"release asset verification passed: {version}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("version")
    version = parser.parse_args().version
    try:
        verify(version, local_path(os.environ.get("RELEASE_DIST", "go/dist"), os.getcwd()))
    except (ControlPlaneError, OSError) as exc:
        raise SystemExit(f"release verification failed: {exc}") from exc


if __name__ == "__main__":
    main()
