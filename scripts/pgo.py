"""Profile-guided optimization for reviewed Rust builds: train one architecture's profile on loopback transfers.

    python3 -m scripts.pgo TARGET [--profile {ci,release}] [--runner COMMAND]

Runs instrumented server and TUI builds for TARGET through every listener and merges the profile that reviewed builds
of its architecture use. RUNNER runs the binaries, such as an emulator; instrumented binaries are never shipped.
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

from rust.interop.fixture import Fixture, Server

from .ci.workspace import load
from .legal.rust import PGO, PROFILES, pgo_profile, remap_flags

ROOT = Path(__file__).resolve().parents[1]
RUST = ROOT / "rust"
SECONDS = 8
ATTEMPTS = 3
# Every listener in both directions; a last HTTP/3 download adds a reply-driven WebTransport bus.
TRAINING = [(listener, direction) for listener in ("http3", "http2", "http1-tls") for direction in ("download", "upload")]


def instrumented(target: str, profile: str, raw: Path) -> dict[str, Path]:
    """The target's instrumented binaries, built apart from every reviewed build's target directory."""
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith("GM_") and key not in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS")}
    flags = [*remap_flags(), f"-Cprofile-generate={raw}"]
    environment["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flags)
    # Profile names follow each crate's features, so each package builds alone as its reviewed build does; their own
    # target directories let both builds, each ending in a single-threaded LTO link, run at once.
    builds = {package: subprocess.Popen(["cargo", "build", "--locked", "--profile", profile, "--target", target,
                                         "-p", package], cwd=RUST,
                                        env=environment | {"CARGO_TARGET_DIR": str(PGO / package)})
              for package in ("graphite-meter-server", "graphite-meter-client")}
    if any([build.wait() for build in builds.values()]):
        sys.exit("an instrumented build failed")
    return {package: PGO / package / target / profile / package for package in builds}


def wrapped(binary: Path, runner: list[str], directory: Path) -> Path:
    """`binary` behind its runner, as one executable the fixture can start."""
    if not runner:
        return binary
    wrapper = directory / f"{binary.name}-run"
    wrapper.write_text(f"#!/bin/sh\nexec {' '.join(runner)} {binary} \"$@\"\n")
    wrapper.chmod(0o755)
    return wrapper


def transfer(client: Path, server: Server, listener: str, direction: str, *extra: str) -> None:
    protocol = "http1" if listener.startswith("http1") else listener
    command = [str(client), "--report", "--insecure", "--url", server.origin("http1"), "--stages", direction,
               "--streams", "4", "--throughput-origin", server.origin(listener), "--throughput-protocol", protocol,
               "--throughput-transport", "fetch-stream", "--warmup", "500ms", f"--{direction}-duration",
               f"{SECONDS}s", *extra]
    # Training wants the code paths, not a measurement: an emulated TUI and server can miss the client's 1.5 s upload
    # checkpoint budget on a busy builder, so a failed transfer runs again.
    for attempt in range(1, ATTEMPTS + 1):
        run = subprocess.run(command, capture_output=True, text=True, timeout=SECONDS * 30)
        if not run.returncode and re.search(r"[↓↑] (?:Download|Upload)\s+[\d.]+ \S+/s", run.stdout):
            return
        reason = (run.stderr.strip() or run.stdout.strip() or "no output").splitlines()[-1]
        print(f"training {listener} {direction}, attempt {attempt} of {ATTEMPTS}, failed: {reason}", file=sys.stderr)
    sys.exit(f"training {listener} {direction} failed:\n{run.stdout[-2000:]}{run.stderr[-2000:]}")


def train(target: str, profile: str, runner: list[str]) -> Path:
    if (merged := pgo_profile(target)) is None:
        sys.exit(f"{target} trains no profile")
    raw = PGO / "raw"
    shutil.rmtree(raw, ignore_errors=True)
    raw.mkdir(parents=True)
    binaries = instrumented(target, profile, raw)
    fixture = Fixture("pgo-")
    server_binary = wrapped(binaries["graphite-meter-server"], runner, fixture.directory)
    client = wrapped(binaries["graphite-meter-client"], runner, fixture.directory)
    with fixture.server(server_binary, "train") as server:
        for listener, direction in TRAINING:
            transfer(client, server, listener, direction, "--loaded-latency=true")
        transfer(client, server, "http3", "download", "--loaded-latency=true", "--loaded-ping", "reply-driven",
                 "--latency-transport", "webtransport", "--latency-origin", server.origin("http3"))
    recorded = sorted(raw.glob("*.profraw"))
    if not recorded:
        sys.exit("training recorded no profile")
    subprocess.run([str(llvm_profdata()), "merge", "--output", str(merged), *map(str, recorded)], check=True)
    shutil.rmtree(raw)
    return merged


def llvm_profdata() -> Path:
    """The pinned toolchain's llvm-profdata, from its llvm-tools component."""
    def rustc(*arguments: str) -> str:
        return subprocess.run(["rustc", *arguments], cwd=RUST, check=True, capture_output=True, text=True).stdout
    host = re.search(r"^host: (\S+)$", rustc("-vV"), re.MULTILINE)
    if host is None:
        sys.exit("rustc -vV names no host")
    return Path(rustc("--print", "sysroot").strip()) / "lib/rustlib" / host[1] / "bin/llvm-profdata"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("target")
    parser.add_argument("--profile", choices=PROFILES, default="release")
    parser.add_argument("--runner", default="", help="a command that runs the target's binaries")
    args = parser.parse_args()
    workspace = load(ROOT)
    shipped = sorted({*workspace.tui.values(), *workspace.server.values()})
    # The target and profile come from the workspace and this module, never from the command line itself.
    target = next((name for name in shipped if name == args.target), None)
    if target is None:
        sys.exit(f"{args.target} is not a shipped target: {', '.join(shipped)}")
    profile = next(name for name in PROFILES if name == args.profile)
    print(f"PGO profile for {target}: {train(target, profile, args.runner.split())}")


if __name__ == "__main__":
    main()
