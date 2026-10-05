"""Rust TUI release archives: Go's archive layout and names with a `_rust` marker, each with its source offer.

    python3 -m scripts.package_rust VERSION [--output DIR] [--check]

Builds the TUI with reviewed notices for every TUI platform of rust/Cargo.toml's workspace metadata. It runs in
the pinned builder image (container/Dockerfile.rust), which provides the toolchain and the cross compilers; a
build for the builder's own system and architecture is run to check its version and allocator setting.
--check builds nothing: it checks the archives and source offers in DIR and runs the TUI built for this machine.
"""
from __future__ import annotations

import argparse
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

from .ci.github_api import ControlPlaneError, confined_path, local_path
from .ci.rust_workspace import ROOT, load
from .legal.artifacts import release_source
from .legal.model import LegalError, Project
from .legal.rust import DEVELOPMENT, VERSION, Build, collect

def names(version: str, platform_name: str) -> tuple[str, str, str]:
    """The archive, its root directory and the binary of the TUI for `platform_name` (GOOS/GOARCH)."""
    goos, goarch = platform_name.split("/")
    base = f"graphite-meter-client_{version}_{goos}_{goarch}_rust"
    if goos == "windows":
        return f"{base}.zip", base, "graphite-meter-client.exe"
    return f"{base}.tar.gz", base, "graphite-meter-client"


def host_platform() -> str:
    """This machine as GOOS/GOARCH."""
    machine = platform.machine().lower()
    return f"{platform.system().lower()}/{ {'x86_64': 'amd64', 'aarch64': 'arm64'}.get(machine, machine)}"


def probe(executable: Path, version: str, platform_name: str) -> None:
    """A build for this machine reports its version and, on Linux, mimalloc's huge pages off by default."""
    reported = subprocess.run([str(executable), "--version"], capture_output=True, text=True, check=True,
                              timeout=10).stdout.strip()
    if reported != f"graphite-meter-client {version}-rust":
        raise LegalError(f"the {platform_name} TUI reports {reported!r}, not version {version}-rust")
    if not platform_name.startswith("linux/"):
        return
    environment = {key: value for key, value in os.environ.items() if not key.upper().startswith("MIMALLOC_")}
    for setting, override in (("0", {}), ("2", {"MIMALLOC_ALLOW_THP": "2"})):
        verbose = subprocess.run([str(executable), "--version"], capture_output=True, text=True, check=True,
                                 env=environment | {"MIMALLOC_VERBOSE": "1"} | override, timeout=10).stderr
        if not re.search(rf"option 'allow_thp': {setting}(?:\s|$)", verbose):
            raise LegalError(f"the {platform_name} TUI has the wrong allocator THP setting: {verbose}")


def package(version: str, platform_name: str, target: str, output: Path) -> None:
    """Build and write the archive and source offer of one platform, replacing neither until both are complete."""
    archive, base, binary = names(version, platform_name)
    offer = f"{base}_third-party-source.tar.gz"
    cargo = ROOT / "rust/target"
    cargo.mkdir(parents=True, exist_ok=True)
    # The notices must lie inside the checkout, where the build script reads them.
    with tempfile.TemporaryDirectory(prefix=".rust-notices-", dir=cargo) as notices, \
            tempfile.TemporaryDirectory(prefix=".rust-package-", dir=output) as staging:
        legal, stage = Path(notices), Path(staging)
        executable = collect(Build("graphite-meter-client", target, "release", version, legal))
        if platform_name == host_platform():
            probe(executable, version, platform_name)
        directory = confined_path(stage / base, stage)
        directory.mkdir()
        shutil.copy2(executable, directory / binary)
        for name in ("LICENSE", "COPYRIGHT"):
            shutil.copyfile(ROOT / name, directory / name)
        shutil.copyfile(legal / "LEGAL.txt", directory / "THIRD_PARTY_NOTICES.txt")
        source = release_source(Project.read(ROOT), version)[1]
        (directory / "SOURCE.txt").write_text(f"Graphite Meter source: {source}\nMatching release: v{version}\n"
                                              f"Dependency source archive: {offer}\nRust target: {target}\n")
        kind = "zip" if archive.endswith(".zip") else "gztar"
        shutil.make_archive(str(confined_path(stage / base, stage)), kind, root_dir=stage, base_dir=base)
        shutil.copyfile(legal / "THIRD_PARTY_SOURCE.tar.gz", confined_path(stage / offer, stage))
        for name in (archive, offer):
            os.replace(confined_path(stage / name, stage), confined_path(output / name, output))
    print(f"Rust TUI {platform_name}: {archive} and {offer}", flush=True)


def archived(path: Path) -> dict[str, bytes]:
    """The files of an archive by name; an entry that is neither a file nor a directory is refused."""
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            return {entry.filename: archive.read(entry) for entry in archive.infolist() if not entry.is_dir()}
    files: dict[str, bytes] = {}
    with tarfile.open(path, "r:gz") as archive:
        for entry in archive:
            handle = archive.extractfile(entry) if entry.isfile() else None
            if handle is None and not entry.isdir():
                raise LegalError(f"{path.name} holds {entry.name}, which is not a regular file")
            if handle is not None:
                files[entry.name] = handle.read()
    return files


def check(version: str, platform_name: str, output: Path) -> None:
    """The archive of `platform_name` holds exactly Go's layout with reviewed notices beside its source offer."""
    archive, base, binary = names(version, platform_name)
    offer = f"{base}_third-party-source.tar.gz"
    files = archived(confined_path(output / archive, output))
    expected = {f"{base}/{name}" for name in (binary, "LICENSE", "COPYRIGHT", "THIRD_PARTY_NOTICES.txt",
                                              "SOURCE.txt")}
    if files.keys() != expected:
        raise LegalError(f"{archive} holds {sorted(files)}, not {sorted(expected)}")
    notices = files[f"{base}/THIRD_PARTY_NOTICES.txt"]
    if not notices.strip() or any(DEVELOPMENT.encode() in content for content in files.values()):
        raise LegalError(f"{archive} lacks reviewed notices")
    if f"Dependency source archive: {offer}\n".encode() not in files[f"{base}/SOURCE.txt"]:
        raise LegalError(f"{archive}'s SOURCE.txt does not name {offer}")
    with tarfile.open(confined_path(output / offer, output), "r:gz") as source:
        if not any(entry.isfile() for entry in source):
            raise LegalError(f"{offer} holds no source")
    if platform_name == host_platform():
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / binary
            executable.write_bytes(files[f"{base}/{binary}"])
            executable.chmod(0o755)
            probe(executable, version, platform_name)
    print(f"Rust TUI {platform_name}: {archive} and {offer} checked", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("version")
    parser.add_argument("--output", type=Path, default=ROOT / "go/dist")
    parser.add_argument("--check", action="store_true", help="check the archives in --output instead of building")
    args = parser.parse_args()
    if VERSION.fullmatch(args.version) is None:
        parser.error("VERSION must be a release identifier")
    try:
        output = local_path(args.output, ROOT)
        output.mkdir(parents=True, exist_ok=True)
        for platform_name, target in load().tui.items():
            if args.check:
                check(args.version, platform_name, output)
            else:
                package(args.version, platform_name, target, output)
    except (ControlPlaneError, LegalError, OSError, ValueError, subprocess.CalledProcessError, tarfile.TarError,
            zipfile.BadZipFile) as error:
        sys.exit(f"Rust TUI packaging: {error}")


if __name__ == "__main__":
    main()
