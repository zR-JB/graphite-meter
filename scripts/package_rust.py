"""Build experimental Rust TUI archives: Go's archive layout with a _rust marker.

    python3 -m scripts.package_rust VERSION --os GOOS... --supplement RECORDS
"""
from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from .ci.github_api import ControlPlaneError, confined_path, local_path, write_checksums
from .ci.toolchains import host_platform, rust_channel, rust_tui_targets, verify_rust_toolchain
from .ci.verify_release_assets import tui_archive

REPO = Path(__file__).resolve().parents[1]


def build(version: str, platform: str, output: Path, supplement: Path) -> None:
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.+-]*", version):
        raise ValueError("invalid release version")
    if (target := rust_tui_targets(REPO / "scripts/tui-targets.txt").get(platform)) is None:
        raise ValueError(f"no TUI target for {platform}")
    # The archives go to the checkout or a temporary directory, and never leave it.
    output = local_path(output, REPO)
    output.mkdir(parents=True, exist_ok=True)
    archive, base, name = tui_archive(version, platform, "_rust")
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
            "--target", target, "--profile", "release", "--out", str(legal), "--version", version,
            "--reviews", "legal/rust-reviewed-components.json", "--supplement", str(supplement.resolve()),
        ], cwd=REPO, env=environment, check=True)
        binary = cargo / target / "release" / name
        package = confined_path(stage / base, stage)
        package.mkdir()
        shutil.copy2(binary, package / name)
        # A build for this machine's system and architecture runs here, whatever its C library.
        if platform == host_platform():
            actual = subprocess.check_output([str(binary), "--version"], text=True).strip()
            if actual != f"graphite-meter-client {version}-rust":
                raise ValueError(f"the {platform} TUI reports {actual!r}, not version {version}-rust")
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
        archive_path = confined_path(stage / archive, stage)
        shutil.make_archive(str(stage / base), "zip" if archive.endswith(".zip") else "gztar",
                            root_dir=stage, base_dir=base)
        shutil.copyfile(legal / "THIRD_PARTY_SOURCE.tar.gz", confined_path(stage / source_name, stage))
        for filename in (archive_path.name, source_name):
            os.replace(confined_path(stage / filename, stage), confined_path(output / filename, output))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version")
    parser.add_argument("--os", nargs="+", required=True, choices=('linux', 'windows'),
                        help="build each supported Rust platform of these GOOS")
    parser.add_argument("--output", type=Path, default=REPO / "go/dist")
    parser.add_argument("--supplement", type=Path, required=True,
                        help="reviewed platform records of this build environment")
    parser.add_argument("--checksums", action="store_true", help="then list every file of --output in checksums.txt")
    args = parser.parse_args()
    targets = {platform: target for platform, target in rust_tui_targets(REPO / "scripts/tui-targets.txt").items()
               if platform.split("/")[0] in args.os}
    if not targets:
        parser.error(f"scripts/tui-targets.txt lists no platform of {args.os}")
    try:
        channel = rust_channel(REPO)
        subprocess.run(["rustup", "toolchain", "install", "--no-self-update", channel, "--profile", "minimal"], check=True)
        # The targets' standard libraries come from the archives of the manifest checked here.
        verify_rust_toolchain(REPO)
        subprocess.run(["rustup", "target", "add", "--toolchain", channel, *targets.values()], check=True)
        for platform in targets:
            build(args.version, platform, args.output, args.supplement)
        if args.checksums:
            write_checksums(local_path(args.output, REPO))
    except (ControlPlaneError, OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"Rust package build failed: {error}") from error


if __name__ == "__main__":
    main()
