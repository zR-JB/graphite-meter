"""Temporary campaign for the release PGO pull request: what the image builder's profiles give its binaries.

export: the release-profile server of container/Dockerfile.rust for linux/amd64 and linux/arm64, profiles included.
baseline: the same source built for this host with the release profile and no profile.
measure cpu|client: server CPU per GiB of the image's server against the baseline (the baseline TUI drives both), and
    TUI CPU per GiB of the PGO TUI archive against the baseline TUI (the baseline server answers both).
"""
from __future__ import annotations

import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import tarfile
from collections.abc import Sequence
from pathlib import Path

from rust.interop.fixture import Fixture, Server, musl_target

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "rust/target/campaign"
BIN = OUT / "bin"
RESULTS = OUT / "results"
EXPORT = OUT / "export"
SECONDS = 15
REPEATS = 6
SERVER_CPUS, CLIENT_CPUS = "0,1", "2,3"
ARCH = {"x86_64": "amd64", "aarch64": "arm64"}[os.uname().machine]
CASES = {"cpu": [("http3", "download"), ("http3", "upload"), ("http2", "download"), ("http1-tls", "upload")],
         "client": [("http3", "download"), ("http3", "upload"), ("http2", "download")]}


def export() -> None:
    subprocess.run(["docker", "buildx", "build", "-f", "container/Dockerfile.rust", "--target", "server",
                    "--platform", "linux/amd64,linux/arm64", "--build-arg", "VERSION=0.0.0-pgo",
                    "--build-arg", "PROFILE=release", "--build-arg", "GM_CLIENT_VALIDATE=0",
                    "--output", f"type=local,dest={OUT / 'image'}", "."], cwd=ROOT, check=True)
    EXPORT.mkdir(parents=True, exist_ok=True)
    for arch in ("amd64", "arm64"):
        shutil.copy2(OUT / "image" / f"linux_{arch}" / "graphite-meter", EXPORT / f"{arch}-server")


def baseline() -> None:
    BIN.mkdir(parents=True, exist_ok=True)
    musl = musl_target()
    subprocess.run(["rustup", "target", "add", musl], cwd=ROOT / "rust", check=True)
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("GM_", "CARGO_PROFILE_")) and not key.endswith("RUSTFLAGS")}
    env |= {"CARGO_INCREMENTAL": "0", "CC_" + musl.replace("-", "_"): f"{musl.split('-')[0]}-linux-musl-gcc",
            "CARGO_TARGET_DIR": str(OUT / "baseline-build")}
    subprocess.run(["cargo", "build", "--locked", "--profile", "release", "--target", musl,
                    "-p", "graphite-meter-server", "-p", "graphite-meter-client"], cwd=ROOT / "rust", env=env, check=True)
    for side in ("server", "client"):
        shutil.copy2(OUT / "baseline-build" / musl / "release" / f"graphite-meter-{side}", BIN / f"release-{side}")
    # The pipeline's server from the image export, its TUI from CI's release archive.
    shutil.copy2(OUT / "pipeline" / f"{ARCH}-server", BIN / "pgo-server")
    archive = next((OUT / "tui").glob(f"graphite-meter-client_*_linux_{ARCH}_rust.tar.gz"))
    with tarfile.open(archive) as bundle:
        member = next(item for item in bundle.getmembers() if item.name.endswith("/graphite-meter-client")
                      or item.name == "graphite-meter-client")
        source = bundle.extractfile(member)
        assert source is not None
        (BIN / "pgo-client").write_bytes(source.read())
    for name in ("pgo-server", "pgo-client"):
        (BIN / name).chmod(0o755)


def cpu(pid: int) -> float:
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def pinned(binary: Path, directory: Path) -> Path:
    """`binary` behind `exec taskset`, which keeps the process ID the CPU accounting reads."""
    wrapper = directory / f"{binary.name}-pinned"
    wrapper.write_text(f"#!/bin/sh\nexec taskset -c {SERVER_CPUS} {binary} \"$@\"\n")
    wrapper.chmod(0o755)
    return wrapper


def bulk(client: Path, server: Server, listener: str, direction: str) -> list[str]:
    protocol = "http1" if listener.startswith("http1") else listener
    return ["taskset", "-c", CLIENT_CPUS, str(client), "--report", "--insecure", "--url", server.origin("http1"),
            "--stages", direction, "--streams", "4", "--throughput-origin", server.origin(listener),
            "--throughput-protocol", protocol, "--throughput-transport", "fetch-stream", "--warmup", "500ms",
            f"--{direction}-duration", f"{SECONDS}s", "--loaded-latency=false"]


def rate(output: str) -> float | None:
    found = re.search(r"[↓↑] (?:Download|Upload)\s+([\d.]+) (Gbit|Mbit)/s", output)
    return float(found[1]) / (1 if found[2] == "Gbit" else 1000) if found else None


def per_gib(seconds: float | None, gbps: float | None) -> float | None:
    return round(seconds / (gbps * 1e9 / 8 * SECONDS / 2**30), 3) if seconds is not None and gbps else None


def transfer(fixture: Fixture, server_bin: str, client_bin: str, listener: str, direction: str) -> dict[str, object]:
    server = fixture.server(pinned(BIN / f"{server_bin}-server", fixture.directory), server_bin,
                            {"TOKIO_WORKER_THREADS": "2"})
    with server:
        assert server.process is not None
        pid = server.process.pid
        before = cpu(pid)
        run = subprocess.run(["/usr/bin/time", "-f", "GM_TIME %U %S",
                              *bulk(BIN / f"{client_bin}-client", server, listener, direction)],
                             capture_output=True, text=True, timeout=SECONDS * 8)
        seconds = cpu(pid) - before
    output = run.stdout + run.stderr
    clock = re.search(r"GM_TIME ([\d.]+) ([\d.]+)", output)
    used = float(clock[1]) + float(clock[2]) if clock else None
    gbps = rate(run.stdout)
    return {"mbps": gbps * 1000 if gbps else None, "clientCpuPerGiB": per_gib(used, gbps),
            "serverCpuPerGiB": per_gib(seconds, gbps)}


def measure(study: str) -> None:
    RESULTS.mkdir(parents=True, exist_ok=True)
    fixture = Fixture(f"campaign-{study}-")
    variants = ("release", "pgo")
    cases: Sequence[tuple[str, str]] = CASES[study]
    rows = []
    with (RESULTS / f"{study}-runs.jsonl").open("w") as sink:
        for repeat in range(REPEATS):
            for index in range(len(cases)):
                listener, direction = cases[(index + repeat) % len(cases)]
                order = variants if (repeat + index) % 2 == 0 else variants[::-1]
                for variant in order:
                    server, client = (variant, "release") if study == "cpu" else ("release", variant)
                    row = {"variant": variant, "case": f"{listener} {direction}", "repeat": repeat} \
                        | transfer(fixture, server, client, listener, direction)
                    rows.append(row)
                    sink.write(json.dumps(row) + "\n")
                    sink.flush()
                    print(json.dumps(row), flush=True)
    metrics = ("mbps", "serverCpuPerGiB") if study == "cpu" else ("mbps", "clientCpuPerGiB", "serverCpuPerGiB")
    lines = [f"### PGO campaign · {study} · {os.uname().machine} · release profile · median (range)", "",
             "| Case | Metric | " + " | ".join(variants) + " |", "|---|---|" + "---|" * len(variants)]
    for case in sorted({str(row["case"]) for row in rows}):
        for metric in metrics:
            cells = []
            for variant in variants:
                values = [float(value) for row in rows if row["variant"] == variant and row["case"] == case
                          and isinstance(value := row.get(metric), (int, float))]
                cells.append(f"{statistics.median(values):.3f} ({min(values):.3f}–{max(values):.3f})"
                             if values else "—")
            lines.append(f"| {case} | {metric} | " + " | ".join(cells) + " |")
    text = "\n".join(lines) + "\n"
    (RESULTS / f"{study}-summary.md").write_text(text)
    print(text, flush=True)


if __name__ == "__main__":
    if sys.argv[1] == "export":
        export()
    elif sys.argv[1] == "baseline":
        baseline()
    else:
        # The study name comes from this module's constants, never from the command line.
        measure(next(name for name in CASES if name == sys.argv[2]))
