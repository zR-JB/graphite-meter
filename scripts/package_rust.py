"""Rust TUI release archives: Go's archive layout and names with a `_rust` marker, each with its source offer.

    python3 -m scripts.package_rust VERSION [--output DIR] [--check]

Builds the TUI with reviewed notices for every TUI platform of rust/Cargo.toml's workspace metadata. It runs in
the pinned builder image (container/Dockerfile.rust), which provides the toolchain and the cross compilers; a
build for the builder's own system and architecture is run to check its version and allocator setting.
Archives are reproducible: fixed times, owners and modes. --check builds nothing: it runs the TUI built for this
machine out of its archive in DIR; rust_release.py verifies every archive and source offer as a release does.
"""
from __future__ import annotations

import argparse
import gzip
import io
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
from .ci.rust_workspace import ROOT, load, offer_name, tui_archive
from .legal.model import LegalError
from .legal.rust import VERSION, Build, collect


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
    archive, base, binary = tui_archive(version, platform_name)
    offer = offer_name("graphite-meter-client", version, platform_name)
    cargo = ROOT / "rust/target"
    cargo.mkdir(parents=True, exist_ok=True)
    # The notices must lie inside the checkout, where the build script reads them.
    with tempfile.TemporaryDirectory(prefix=".rust-notices-", dir=cargo) as notices, \
            tempfile.TemporaryDirectory(prefix=".rust-package-", dir=output) as staging:
        legal, stage = Path(notices), Path(staging)
        executable = collect(Build("graphite-meter-client", target, "release", version, legal))
        if platform_name == host_platform():
            probe(executable, version, platform_name)
        texts = {"LICENSE": ROOT / "LICENSE", "COPYRIGHT": ROOT / "COPYRIGHT",
                 "THIRD_PARTY_NOTICES.txt": legal / "LEGAL.txt", "SOURCE.txt": legal / "SOURCE.txt"}
        files = {binary: (executable.read_bytes(), 0o755)} | {name: (path.read_bytes(), 0o644)
                                                              for name, path in texts.items()}
        write_archive(confined_path(stage / archive, stage), base, files)
        shutil.copyfile(confined_path(legal / offer, legal), confined_path(stage / offer, stage))
        for name in (archive, offer):
            os.replace(confined_path(stage / name, stage), confined_path(output / name, output))
    print(f"Rust TUI {platform_name}: {archive} and {offer}", flush=True)


def write_archive(path: Path, base: str, files: dict[str, tuple[bytes, int]]) -> None:
    """Go's layout, `base/` holding `files` with their modes, at fixed times and owners."""
    if path.suffix == ".zip":
        with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
            directory = zipfile.ZipInfo(f"{base}/", (1980, 1, 1, 0, 0, 0))
            directory.create_system, directory.external_attr = 3, (0o40755 << 16) | 0x10
            archive.writestr(directory, b"")
            for name, (data, mode) in sorted(files.items()):
                entry = zipfile.ZipInfo(f"{base}/{name}", (1980, 1, 1, 0, 0, 0))
                entry.create_system, entry.external_attr = 3, (0o100000 | mode) << 16
                archive.writestr(entry, data, zipfile.ZIP_DEFLATED)
        return
    with path.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed, \
            tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
        directory = tarfile.TarInfo(base)
        directory.type, directory.mode = tarfile.DIRTYPE, 0o755
        archive.addfile(directory)
        for name, (data, mode) in sorted(files.items()):
            entry = tarfile.TarInfo(f"{base}/{name}")
            entry.size, entry.mode = len(data), mode
            archive.addfile(entry, io.BytesIO(data))


def check(version: str, output: Path) -> None:
    """Run the TUI built for this machine out of its archive."""
    platform_name = host_platform()
    if platform_name not in load().tui:
        raise LegalError(f"no Rust TUI is built for {platform_name}")
    archive, base, binary = tui_archive(version, platform_name)
    path = confined_path(output / archive, output)
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as files:
            data = files.read(f"{base}/{binary}")
    else:
        with tarfile.open(path, "r:gz") as files:
            handle = files.extractfile(f"{base}/{binary}")
            if handle is None:
                raise LegalError(f"{archive} holds no executable {binary}")
            data = handle.read()
    with tempfile.TemporaryDirectory() as directory:
        executable = Path(directory) / binary
        executable.write_bytes(data)
        executable.chmod(0o755)
        probe(executable, version, platform_name)
    print(f"Rust TUI {platform_name}: {archive} runs", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("version")
    parser.add_argument("--output", type=Path, default=ROOT / "go/dist")
    parser.add_argument("--check", action="store_true", help="run this machine's TUI out of --output instead of building")
    args = parser.parse_args()
    if VERSION.fullmatch(args.version) is None:
        parser.error("VERSION must be a release identifier")
    try:
        output = local_path(args.output, ROOT)
        output.mkdir(parents=True, exist_ok=True)
        if args.check:
            check(args.version, output)
            return
        for platform_name, target in load().tui.items():
            package(args.version, platform_name, target, output)
    except (ControlPlaneError, LegalError, OSError, ValueError, KeyError, subprocess.CalledProcessError,
            tarfile.TarError, zipfile.BadZipFile) as error:
        sys.exit(f"Rust TUI packaging: {error}")


if __name__ == "__main__":
    main()
