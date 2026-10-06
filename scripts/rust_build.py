"""Local Rust builds with development notices, for this host.

    python3 -m scripts.rust_build --package NAME --profile {ci,release} [--browser {dev,prod}] [--version V]

The server embeds a browser build kept in rust/target/browser/PROFILE, apart from Go's client/dist, together
with its module scan. It is rebuilt only when a browser input, the Bun release or a client build variable
changes, or when its outputs no longer match their stamp. Cargo then owns the binary's freshness: the notices
keep unchanged files untouched, so a build after a Rust edit reuses every other artifact.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

from .legal.model import LegalError, marshal, sha256
from .legal.rust import PACKAGES, PROFILES, VERSION, Build, collect, write_changed

ROOT = Path(__file__).resolve().parents[1]
BROWSER_PROFILES = ("dev", "prod")
# Tracked and new files the browser build reads, never ignored build products.
BROWSER_SOURCES = ("client", "go/internal/auth/assets", "api", "scripts/rust_build.py")
BROWSER_VARIABLES = ("GM_CLIENT_", "VITE_", "BUN_", "NODE_")


def fingerprint(paths: list[Path], root: Path) -> dict[str, str]:
    return {path.relative_to(root).as_posix(): sha256(path.read_bytes()) for path in sorted(set(paths))
            if path.is_file()}


def browser_inputs(environment: dict[str, str]) -> dict[str, object]:
    """What a browser build depends on: its source files, the Bun release and the client build variables."""
    listed = subprocess.check_output(["git", "ls-files", "-z", "-c", "-o", "--exclude-standard", *BROWSER_SOURCES],
                                     cwd=ROOT).decode().split("\0")
    paths = [ROOT / name for name in listed if name] + list((ROOT / "client").glob(".env*"))
    return {"files": fingerprint(paths, ROOT), "bun": subprocess.check_output(["bun", "--version"], text=True).strip(),
            "environment": {key: value for key, value in sorted(environment.items())
                            if key.startswith(BROWSER_VARIABLES) or key == "VERSION"}}


def browser(profile: str) -> tuple[Path, Path]:
    """The browser build of `profile` and its module scan, rebuilt only when stale."""
    output = ROOT / "rust/target/browser" / profile
    assets, scan, stamp = output / "assets", output / "modules.json", output / "build.json"
    environment = dict(os.environ, GM_CLIENT_BUILD_PROFILE=profile)
    environment.setdefault("GM_CLIENT_REVISION", subprocess.check_output(
        ["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, text=True).strip())
    if profile == "dev":
        environment.pop("VERSION", None)
    inputs = browser_inputs(environment)
    try:
        previous = json.loads(stamp.read_bytes())
    except (OSError, ValueError):
        previous = None
    if previous == {"inputs": inputs, "outputs": fingerprint([scan, *assets.rglob("*")], output)} and \
            (assets / "index.html").is_file():
        print(f"Browser [{profile}]: reuse the cached build and module scan", flush=True)
        return assets, scan
    print(f"Browser [{profile}]: build into {assets.relative_to(ROOT)}", flush=True)
    stamp.unlink(missing_ok=True)
    # Vite empties no output directory outside client/, so this build removes only its own products.
    shutil.rmtree(assets, ignore_errors=True)
    scan.unlink(missing_ok=True)
    output.mkdir(parents=True, exist_ok=True)
    # Vite writes the module scan only inside the temporary directory.
    subprocess.run(["bun", "run", "build"], cwd=ROOT / "client", check=True,
                   env=environment | {"TMPDIR": str(output), "GM_LEGAL_SCAN_OUT": str(scan),
                                      "GM_LEGAL_SCAN_DIR": str(assets)})
    write_changed(stamp, marshal({"inputs": inputs, "outputs": fingerprint([scan, *assets.rglob("*")], output)}))
    return assets, scan


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--package", choices=PACKAGES, required=True)
    parser.add_argument("--profile", choices=PROFILES, required=True)
    parser.add_argument("--browser", choices=BROWSER_PROFILES, help="the server's browser build")
    parser.add_argument("--version", default="development")
    args = parser.parse_args()
    if (args.package == "graphite-meter-server") != (args.browser is not None):
        parser.error("the server, and only the server, takes --browser")
    if VERSION.fullmatch(args.version) is None:
        parser.error("--version must be a release identifier")
    package = PACKAGES[PACKAGES.index(args.package)]
    browser_build = browser(BROWSER_PROFILES[BROWSER_PROFILES.index(args.browser)]) if args.browser else None
    out = ROOT / "rust/target/dev-legal" / package
    executable = collect(Build(package, None, PROFILES[PROFILES.index(args.profile)], args.version, out,
                               browser_build))
    print(f"Rust {package} [{args.profile} profile, development notices]: {executable.relative_to(ROOT)}")


if __name__ == "__main__":
    try:
        main()
    except (LegalError, OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"Rust build: {error}")
