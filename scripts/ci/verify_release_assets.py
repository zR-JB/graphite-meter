#!/usr/bin/env python3
"""Verify native release archives, checksums, legal files and embedded versions."""

from __future__ import annotations

import argparse
import os
import platform
import re
import stat
import subprocess
import tarfile
import tempfile
import zipfile
from pathlib import Path, PurePosixPath

from github_api import (
    TLS_NAME, ControlPlaneError, JsonObject, confined_path, decode_json, expect_array,
    expect_object, fail, file_sha256, int_field, object_field, str_field,
)

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


def tui_archives(version: str, targets: Path) -> dict[str, tuple[str, str]]:
    """Map each supported TUI archive name to its root directory and binary name."""
    archives: dict[str, tuple[str, str]] = {}
    for target in targets.read_text(encoding="utf-8").split():
        if re.fullmatch(r"[a-z0-9]+/[a-z0-9]+", target) is None:
            fail(f"invalid TUI target: {target!r}")
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
    machine = platform.machine().lower()
    goarch = {"x86_64": "amd64", "aarch64": "arm64"}.get(machine, machine)
    host = f"graphite-meter-client_{version}_{platform.system().lower()}_{goarch}"
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


RUST_TARGET = "x86_64-unknown-linux-gnu"


def expected_rust_artifacts(version: str, selection: str) -> set[str]:
    if selection not in ("none", "server", "tui", "both"):
        fail("invalid Rust artifact selection")
    names: set[str] = set()
    if selection in ("server", "both"):
        names.add(f"graphite-meter-server_{version}_linux_amd64_rust_third-party-source.tar.gz")
    if selection in ("tui", "both"):
        base = f"graphite-meter-client_{version}_linux_amd64_rust"
        names |= {f"{base}.tar.gz", f"{base}_third-party-source.tar.gz"}
    return names


def read_tar_text(path: Path, name: str, limit: int = 4 * 1024 * 1024) -> str:
    try:
        with tarfile.open(path, "r:gz") as archive:
            item = archive.getmember(name)
            if not item.isfile() or item.size > limit:
                fail(f"{name} is not a regular file or exceeds limit {limit}")
            return member(archive, name).decode()
    except (KeyError, UnicodeDecodeError, tarfile.TarError) as exc:
        raise ControlPlaneError(f"cannot read {path.name}/{name}: {exc}") from exc


def verify_rust_source(path: Path, package: str, lock_sha256: str | None = None) -> JsonObject:
    names = archive_names(path)
    inventory = expect_object(decode_json(read_tar_text(path, "inventory.json"), path.name), path.name)
    if int_field(inventory, "schemaVersion", path.name) != 1 or any(
            inventory.get(key) != value for key, value in {
        "package": package, "profile": "release", "target": RUST_TARGET,
    }.items()):
        fail(f"{path.name} has invalid Rust build identity")
    lock = inventory.get("cargoLockSha256")
    if not isinstance(lock, str) or re.fullmatch(r"[0-9a-f]{64}", lock) is None:
        fail("invalid Rust Cargo lock identity")
    if lock != (lock_sha256 or file_sha256(Path("rust/Cargo.lock"))):
        fail("Rust source inventory does not match release Cargo lock")
    components = expect_array(inventory.get("components"), "Rust components")
    browser = expect_array(inventory.get("browserComponents", []), "Rust browser components")
    if not components:
        fail(f"{path.name} has no valid dependency inventory")
    trees: set[str] = set()
    for item in components:
        component = object_field(expect_object(item, "Rust dependency"), "component", "Rust dependency")
        name = str_field(component, "name", "Rust dependency")
        version = str_field(component, "version", "Rust dependency")
        trees.add(f"third_party/cargo/{name}-{version}/")
    browser_manual: list[tuple[str, str, str]] = []
    for item in browser:
        component = expect_object(item, "Rust browser component")
        identity = tuple(str_field(component, key, "Rust browser component")
                         for key in ("ecosystem", "name", "version"))
        if identity[0] == "npm":
            trees.add(f"third_party/npm/{identity[1]}-{identity[2]}/")
        else:
            browser_manual.append((identity[0], identity[1], identity[2]))
    allowed = {"inventory.json", "LEGAL.txt", "legal/rust-forks.json"}
    if decode_json(read_tar_text(path, "legal/rust-forks.json"), "Rust forks") != decode_json(
            Path("legal/rust-forks.json").read_text(), "release Rust forks"):
        fail("Rust source fork identities differ from release tooling")
    manual_sources: dict[tuple[str, str, str], set[str]] = {}
    for filename, scope in (("legal/rust-provenance.json", "rust"),
                           ("legal/provenance.json", "server/browser")):
        if scope == "server/browser" and package != "graphite-meter-server":
            continue
        entries = expect_array(decode_json(Path(filename).read_text(), filename), filename)
        for item in entries:
            entry = expect_object(item, filename)
            scopes = expect_array(entry.get("artifactScopes", []), filename)
            if any(not isinstance(value, str) for value in scopes):
                fail(f"{filename} artifact scopes must be strings")
            if scope not in scopes:
                continue
            identity = (str_field(entry, "ecosystem", filename), str_field(entry, "name", filename),
                        str_field(entry, "version", filename))
            files = {str_field(expect_object(file, filename), "name", filename)
                     for file in expect_array(entry.get("localLegalFiles", []), filename)}
            for value in expect_array(entry.get("localPaths", []), filename):
                if not isinstance(value, str):
                    fail(f"{filename} local paths must be strings")
                files.add(value)
            manual_sources[identity] = files
            allowed.update(files)
    for identity in browser_manual:
        if identity not in manual_sources:
            fail("Rust browser inventory contains unreviewed manual source")
        if missing := manual_sources[identity] - names:
            fail(f"Rust browser source offer is missing reviewed inputs: {sorted(missing)}")
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
    if not read_tar_text(path, "LEGAL.txt").strip():
        fail("Rust source offer has empty notices")
    return inventory


def verify_rust_server_source(dist: Path, version: str, lock_sha256: str | None = None) -> None:
    name = f"graphite-meter-server_{version}_linux_amd64_rust_third-party-source.tar.gz"
    verify_rust_source(dist / name, "graphite-meter-server", lock_sha256)


def verify_rust_client_archive(dist: Path, version: str, lock_sha256: str | None = None) -> None:
    base = f"graphite-meter-client_{version}_linux_amd64_rust"
    path = dist / f"{base}.tar.gz"
    names = archive_names(path)
    required = {f"{base}/{name}" for name in (
        "graphite-meter-client", "BUILD.json", "LEGAL.txt", "LICENSE", "COPYRIGHT", "SOURCE.txt")}
    require_same("Rust TUI archive files", required | {base}, names)
    with tarfile.open(path, "r:gz") as archive:
        executable = archive.getmember(f"{base}/graphite-meter-client")
        if not executable.isfile() or not 64 <= executable.size <= 128 * 1024 * 1024:
            fail("Rust TUI executable is not a bounded regular file")
        handle = archive.extractfile(executable)
        if handle is None:
            fail("Rust TUI executable is unreadable")
        header = handle.read(64)
    if (header[:7] != b"\x7fELF\x02\x01\x01"
            or int.from_bytes(header[16:18], "little") not in (2, 3)
            or int.from_bytes(header[18:20], "little") != 62
            or int.from_bytes(header[20:24], "little") != 1
            or int.from_bytes(header[52:54], "little") != 64):
        fail("Rust TUI executable is not a Linux AMD64 ELF binary")
    metadata = expect_object(decode_json(read_tar_text(path, f"{base}/BUILD.json"), path.name), path.name)
    if int_field(metadata, "schemaVersion", path.name) != 1 or any(
            metadata.get(key) != value for key, value in {
        "implementation": "rust", "version": version + "-rust",
        "target": RUST_TARGET,
    }.items()):
        fail("invalid Rust TUI build identity")
    glibc = str_field(metadata, "minimumGlibc", "Rust TUI build")
    if re.fullmatch(r"[0-9]+\.[0-9]+", glibc) is None:
        fail("invalid Rust TUI glibc requirement")
    libraries = metadata.get("neededLibraries")
    if not isinstance(libraries, list) or not libraries or any(
            not isinstance(name, str) or SAFE_NAME.fullmatch(name) is None for name in libraries):
        fail("invalid Rust TUI shared library requirements")
    inventory = verify_rust_source(dist / f"{base}_third-party-source.tar.gz",
                                   "graphite-meter-client", lock_sha256)
    if not isinstance(metadata.get("rustc"), str) or metadata["rustc"] != inventory.get("rustc"):
        fail("Rust TUI compiler does not match source inventory")
    for filename in ("LICENSE", "COPYRIGHT"):
        if read_tar_text(path, f"{base}/{filename}") != Path(filename).read_text():
            fail(f"Rust TUI {filename} differs from release source")
    if f"{base}_third-party-source.tar.gz" not in read_tar_text(path, f"{base}/SOURCE.txt"):
        fail("Rust TUI source notice lacks matching source archive")
    if read_tar_text(path, f"{base}/LEGAL.txt") != read_tar_text(
            dist / f"{base}_third-party-source.tar.gz", "LEGAL.txt"):
        fail("Rust TUI notices differ from source offer")


def verify_artifacts(version: str, dist: Path, rust: str = "none") -> None:
    source = f"graphite-meter_{version}_third-party-source.tar.gz"
    checksummed = verify_checksums(dist)
    require_same("checksummed release artifacts", {source, *tui_archives(version, TARGETS), *expected_rust_artifacts(version, rust)},
                 checksummed)
    verify_release_file_set(dist, checksummed)
    verify_third_party_source_archive(dist, version)
    verify_client_archives(dist, version, TARGETS)
    if rust in ("server", "both"):
        verify_rust_server_source(dist, version)
    if rust in ("tui", "both"):
        verify_rust_client_archive(dist, version)


def verify(version: str, dist: Path) -> None:
    verify_artifacts(version, dist, os.environ.get("RUST", "none"))
    verify_tui_version(version, dist)
    print(f"release asset verification passed: {version}")


def release_dist() -> Path:
    """RELEASE_DIST, which must lie in the checkout or a temporary directory."""
    roots = (os.getcwd(), tempfile.gettempdir(), os.environ.get("RUNNER_TEMP") or os.getcwd())
    return confined_path(os.environ.get("RELEASE_DIST", "go/dist"), *roots)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("version")
    version = parser.parse_args().version
    try:
        verify(version, release_dist())
    except (ControlPlaneError, OSError) as exc:
        raise SystemExit(f"release verification failed: {exc}") from exc


if __name__ == "__main__":
    main()
