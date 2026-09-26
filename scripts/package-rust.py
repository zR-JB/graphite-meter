#!/usr/bin/env python3
"""Build a separately named, target-specific experimental Rust TUI archive."""
from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
from pathlib import Path

TARGET = "x86_64-unknown-linux-gnu"
REPO = Path(__file__).resolve().parents[1]


def build(version: str, output: Path, supplement: Path) -> None:
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.+-]*", version):
        raise ValueError("invalid release version")
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise ValueError(f"Rust packaging currently requires a native {TARGET} builder")
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    channel = tomllib.loads((REPO / "rust/rust-toolchain.toml").read_text())["toolchain"]["channel"]
    base = f"graphite-meter-client_{version}_linux_amd64_rust"
    environment = dict(os.environ, GM_ENGINE_VERSION=f"{version}-rust")
    # Keep a predictable Cargo output location; legal generation and packaging must
    # refer to the same freshly rebuilt executable, never another user's cache.
    environment["CARGO_TARGET_DIR"] = str(REPO / "rust/target")
    with tempfile.TemporaryDirectory(prefix=".rust-package-", dir=output) as temporary:
        stage = Path(temporary)
        legal = stage / "legal"
        subprocess.run([
            "python3", "-m", "scripts.legal.rust", "--package", "graphite-meter-client",
            "--target", TARGET, "--profile", "release", "--out", str(legal),
            "--reviews", "legal/rust-reviewed-components.json", "--supplement", str(supplement.resolve()),
        ], cwd=REPO, env=environment, check=True)
        binary = REPO / "rust/target" / TARGET / "release/graphite-meter-client"
        package = stage / base
        package.mkdir()
        shutil.copy2(binary, package / binary.name)
        actual = subprocess.check_output([str(binary), "--version"], text=True).strip()
        if actual != f"{version}-rust":
            raise ValueError(f"Rust executable version mismatch: {actual!r}")
        report = subprocess.check_output([str(binary), "--legal"])
        if report != (legal / "LEGAL.txt").read_bytes():
            raise ValueError("Rust executable legal report does not match packaged notices")
        version_info = subprocess.check_output(["readelf", "--version-info", str(binary)], text=True)
        glibc = sorted(set(re.findall(r"\bGLIBC_(\d+(?:\.\d+)+)\b", version_info)), key=lambda item: tuple(map(int, item.split("."))))
        if not glibc:
            raise ValueError("could not determine GNU/Linux glibc requirement")
        dependencies = subprocess.check_output(["readelf", "--dynamic", str(binary)], text=True)
        needed = sorted(set(re.findall(r"\(NEEDED\).*\[([^]]+)\]", dependencies)))
        compiler = subprocess.check_output(["rustc", f"+{channel}", "-vV"], text=True)
        metadata = {"schemaVersion": 1, "implementation": "rust", "version": actual,
                    "target": TARGET, "minimumGlibc": glibc[-1], "neededLibraries": needed,
                    "rustc": compiler}
        (package / "BUILD.json").write_text(json.dumps(metadata, indent=2) + "\n")
        for filename in ("LICENSE", "COPYRIGHT"):
            shutil.copyfile(REPO / filename, package / filename)
        shutil.copyfile(legal / "LEGAL.txt", package / "LEGAL.txt")
        source_name = f"{base}_third-party-source.tar.gz"
        (package / "SOURCE.txt").write_text(
            "Graphite Meter source: https://github.com/zR-JB/graphite-meter\n"
            f"Matching release: v{version}\nDependency source archive: {source_name}\n"
            f"Experimental native target: {TARGET}; requires glibc {glibc[-1]} or newer "
            "and the shared libraries recorded in BUILD.json. Other Linux distributions "
            "and targets have not been established as compatible.\n"
        )
        # Finish both staged files before replacing either destination.
        archive_path = stage / f"{base}.tar.gz"
        with tarfile.open(archive_path, "w:gz") as archive:
            archive.add(package, arcname=base)
        shutil.copyfile(legal / "THIRD_PARTY_SOURCE.tar.gz", stage / source_name)
        for filename in (archive_path.name, source_name):
            os.replace(stage / filename, output / filename)



def build_container(version: str, output: Path) -> None:
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.+-]*", version):
        raise ValueError("invalid release version")
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".rust-export-", dir=output) as temporary:
        if "," in temporary:
            raise ValueError("container artifact path must not contain commas")
        subprocess.run([
            "docker", "buildx", "build", "--platform", "linux/amd64",
            "--target", "tui-artifacts", "-f", "container/Dockerfile.rust",
            "--build-arg", f"VERSION={version}", "--output", f"type=local,dest={temporary}", ".",
        ], cwd=REPO, check=True)
        base = f"graphite-meter-client_{version}_linux_amd64_rust"
        expected = {f"{base}.tar.gz", f"{base}_third-party-source.tar.gz"}
        exported = list(Path(temporary).iterdir())
        if {path.name for path in exported} != expected or any(
            not path.is_file() or path.is_symlink() for path in exported
        ):
            raise ValueError("Rust container exported an unexpected artifact set")
        for path in exported:
            os.replace(path, output / path.name)

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version")
    parser.add_argument("--output", type=Path, default=REPO / "go/dist")
    parser.add_argument("--supplement", type=Path, default=REPO / "legal/rust-platform-linux-gnu.json")
    parser.add_argument("--container", action="store_true", help="use the pinned reviewed Debian release builder")
    args = parser.parse_args()
    try:
        if args.container:
            build_container(args.version, args.output)
        else:
            build(args.version, args.output, args.supplement)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"Rust package build failed: {error}") from error


if __name__ == "__main__":
    main()
