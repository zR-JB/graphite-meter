#!/usr/bin/env python3
"""Build experimental Rust TUI archives: Go's archive layout with a _rust marker."""
from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]


def release_directory(requested: Path) -> Path:
    output = os.path.realpath(requested)
    roots = (REPO, Path(tempfile.gettempdir()), Path(os.environ.get("RUNNER_TEMP") or tempfile.gettempdir()))
    if not output.startswith(tuple(os.path.realpath(root) + os.sep for root in roots)):
        raise ValueError("release output must be inside the checkout or temporary directories")
    os.makedirs(output, exist_ok=True)
    return Path(output)


def child(directory: Path, name: str) -> Path:
    path = os.path.realpath(directory / name)
    if not path.startswith(os.path.realpath(directory) + os.sep):
        raise ValueError(f"{name!r} does not name an entry of {directory}")
    return Path(path)


def rust_target(platform: str) -> str:
    for line in (REPO / "scripts/tui-targets.txt").read_text().splitlines():
        if line.split()[:1] == [platform]:
            return line.split()[1]
    raise ValueError(f"no TUI target for {platform}")


def build(version: str, platform: str, output: Path, supplement: Path) -> None:
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.+-]*", version):
        raise ValueError("invalid release version")
    target = rust_target(platform)
    goos, goarch = platform.split("/")
    output = release_directory(output)
    channel = tomllib.loads((REPO / "rust/rust-toolchain.toml").read_text())["toolchain"]["channel"]
    base = f"graphite-meter-client_{version}_{goos}_{goarch}_rust"
    name = "graphite-meter-client.exe" if goos == "windows" else "graphite-meter-client"
    environment = dict(os.environ, GM_ENGINE_VERSION=f"{version}-rust")
    cargo = REPO / "rust/target"
    environment["CARGO_TARGET_DIR"] = str(cargo)
    cargo.mkdir(parents=True, exist_ok=True)
    # build.rs reads reviewed notices only from inside the checkout.
    with tempfile.TemporaryDirectory(prefix=".rust-package-", dir=output) as temporary, \
            tempfile.TemporaryDirectory(prefix=".rust-notices-", dir=cargo) as notices:
        stage = Path(temporary)
        legal = Path(notices) / "legal"
        subprocess.run([
            sys.executable, "-m", "scripts.legal.rust", "--package", "graphite-meter-client",
            "--target", target, "--profile", "release", "--out", str(legal),
            "--reviews", "legal/rust-reviewed-components.json", "--supplement", str(supplement.resolve()),
        ], cwd=REPO, env=environment, check=True)
        binary = cargo / target / "release" / name
        package = child(stage, base)
        package.mkdir()
        shutil.copy2(binary, package / name)
        host = re.search(r"(?m)^host: (\S+)$", subprocess.check_output(["rustc", f"+{channel}", "-vV"], text=True))
        if host and host[1] == target:
            actual = subprocess.check_output([str(binary), "--version"], text=True).strip()
            if actual != f"graphite-meter-client {version}-rust":
                raise ValueError(f"Rust executable version mismatch: {actual!r}")
        for filename in ("LICENSE", "COPYRIGHT"):
            shutil.copyfile(REPO / filename, package / filename)
        shutil.copyfile(legal / "LEGAL.txt", package / "THIRD_PARTY_NOTICES.txt")
        source_name = f"{base}_third-party-source.tar.gz"
        (package / "SOURCE.txt").write_text(
            "Graphite Meter source: https://github.com/zR-JB/graphite-meter\n"
            f"Matching release: v{version}\nDependency source archive: {source_name}\n"
            f"Experimental native target: {target}\n"
        )
        # Finish both staged files before replacing either destination.
        archive_path = child(stage, f"{base}.zip" if goos == "windows" else f"{base}.tar.gz")
        if goos == "windows":
            with zipfile.ZipFile(archive_path, "w", zipfile.ZIP_DEFLATED) as archive:
                archive.write(package, base)
                for path in sorted(package.iterdir()):
                    archive.write(path, f"{base}/{path.name}")
        else:
            with tarfile.open(archive_path, "w:gz") as archive:
                archive.add(package, arcname=base)
        shutil.copyfile(legal / "THIRD_PARTY_SOURCE.tar.gz", child(stage, source_name))
        for filename in (archive_path.name, source_name):
            os.replace(child(stage, filename), child(output, filename))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version")
    parser.add_argument("--platform", nargs="+", required=True, help="GOOS/GOARCH as listed in scripts/tui-targets.txt")
    parser.add_argument("--output", type=Path, default=REPO / "go/dist")
    parser.add_argument("--supplement", type=Path, required=True,
                        help="reviewed platform records of this build environment")
    args = parser.parse_args()
    try:
        for platform in args.platform:
            build(args.version, platform, args.output, args.supplement)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"Rust package build failed: {error}") from error


if __name__ == "__main__":
    main()
