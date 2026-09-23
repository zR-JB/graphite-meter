#!/usr/bin/env python3
"""Build and run the Rust server with notices for its exact browser and Cargo inputs."""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-gnu"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("dev", "release"), required=True)
    parser.add_argument("--version", default="development")
    parser.add_argument("--build-only", action="store_true")
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.+-]*", args.version):
        parser.error("invalid version")

    target_dir = REPO / "rust/target"
    target_dir.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment["GM_ENGINE_VERSION"] = f"{args.version}-rust"
    if args.profile == "release":
        environment["VERSION"] = args.version
    environment["GM_RUST_ASSET_DIR"] = str(REPO / "client/dist")
    environment["CARGO_TARGET_DIR"] = str(target_dir)

    with tempfile.TemporaryDirectory(prefix=".rust-reviewed-", dir=target_dir) as temporary:
        stage = Path(temporary)
        scan = stage / "browser-modules.json"
        browser_environment = dict(environment, GM_LEGAL_SCAN_OUT=str(scan))
        subprocess.run(
            ["mise", "run", "client-build-prod" if args.profile == "release" else "client-build-dev"],
            cwd=REPO,
            env=browser_environment,
            check=True,
        )
        if not scan.is_file():
            raise RuntimeError("browser build did not produce its legal module scan")
        subprocess.run(
            [
                "python3", "-m", "scripts.legal.rust",
                "--package", "graphite-meter-server",
                "--target", TARGET,
                "--profile", args.profile,
                "--out", str(stage / "legal"),
                "--reviews", "legal/rust-reviewed-components.json",
                "--supplement", "legal/rust-platform-linux-gnu.json",
                "--browser-scan", str(scan),
            ],
            cwd=REPO,
            env=environment,
            check=True,
        )
        binary = target_dir / TARGET / ("release" if args.profile == "release" else "debug") / "graphite-meter-server"
    if args.build_only:
        print(binary)
        return
    os.execve(binary, [str(binary)], environment)


if __name__ == "__main__":
    main()
