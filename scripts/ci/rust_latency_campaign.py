"""Temporary campaign for the Rust review pull request, fifth run: ACK frequency on lossy and slow links.

build: ci-profile musl binaries of the head and of the head asking for an ACK every ten packets.
measure lossy|slow: HTTP/3 over loopback shaped by netem (40 ms, 50 Mbit/s, 0.5% loss; 60 ms, 10 Mbit/s): throughput
    and CPU per GiB of both sides, the server pinned to two cores and the TUI to the other two.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import statistics
import subprocess
import sys
from pathlib import Path

from rust.interop.fixture import Fixture, Server, musl_target

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "rust/target/campaign"
BIN = OUT / "bin"
ACK_EVERY_TEN = """    let mut ack = noq::AckFrequencyConfig::default();
    ack.ack_eliciting_threshold(VarInt::from_u32(9));
    transport.ack_frequency_config(Some(ack));
    transport
}"""
# Each build: its edits (path, old, new, occurrences) on the head.
BUILDS: dict[str, list[tuple[str, str, str, int]]] = {
    "head": [], "ackfreq": [("rust/net/src/quic.rs", "    transport\n}", ACK_EVERY_TEN, 2)],
}
VARIANTS = tuple(BUILDS)
SECONDS = int(os.environ.get("CAMPAIGN_SECONDS", "15"))
REPEATS = int(os.environ.get("CAMPAIGN_REPEATS", "6"))
SERVER_CPUS, CLIENT_CPUS = "0,1", "2,3"
CASES = [("http3", "download"), ("http3", "upload")]
# netem applies to loopback in both directions: each delay counts twice in a round trip.
SHAPES = {
    "lossy": ["delay", "20ms", "loss", "0.5%", "rate", "50mbit", "limit", "20000"],
    "slow": ["delay", "30ms", "rate", "10mbit", "limit", "20000"],
}


def git(*arguments: str) -> str:
    return subprocess.run(["git", *arguments], cwd=ROOT, check=True, capture_output=True, text=True).stdout.strip()


def build() -> None:
    BIN.mkdir(parents=True, exist_ok=True)
    musl = musl_target()
    subprocess.run(["rustup", "target", "add", musl], cwd=ROOT / "rust", check=True)
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("GM_", "CARGO_PROFILE_")) and not key.endswith("RUSTFLAGS")}
    env |= {"CARGO_INCREMENTAL": "0", "CC_" + musl.replace("-", "_"): f"{musl.split('-')[0]}-linux-musl-gcc"}
    for label, edits in BUILDS.items():
        tree = ROOT if label == "head" else OUT / f"{label}-src"
        if tree != ROOT and not tree.exists():
            git("worktree", "add", "--detach", str(tree), git("rev-parse", "HEAD"))
            for path, old, new, count in edits:
                text = (tree / path).read_text()
                if text.count(old) != count:
                    sys.exit(f"{label}: {old!r} is not in {path} {count} times")
                (tree / path).write_text(text.replace(old, new))
        # Legal notice embedding needs each tree's outputs under its own target directory.
        built = tree / "rust/target/campaign-build"
        command = ["cargo", "build", "--locked", "--profile", "ci", "--target", musl,
                   "-p", "graphite-meter-server", "-p", "graphite-meter-client"]
        with (OUT / f"build-{label}.log").open("w") as log:
            status = subprocess.run(command, cwd=tree / "rust", env=env | {"CARGO_TARGET_DIR": str(built)},
                                    stdout=log, stderr=subprocess.STDOUT).returncode
        if status:
            sys.exit(f"{label} build failed:\n" + (OUT / f"build-{label}.log").read_text()[-6000:])
        for side in ("server", "client"):
            shutil.copy2(built / musl / "ci" / f"graphite-meter-{side}", BIN / f"{label}-{side}")


def cpu(pid: int) -> float:
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def pinned(binary: Path, directory: Path) -> Path:
    """`binary` behind `exec taskset`, which keeps the process ID the CPU accounting reads."""
    wrapper = directory / f"{binary.name}-pinned"
    wrapper.write_text(f"#!/bin/sh\nexec taskset -c {SERVER_CPUS} {binary} \"$@\"\n")
    wrapper.chmod(0o755)
    return wrapper


def rate(output: str) -> float | None:
    found = re.search(r"[↓↑] (?:Download|Upload)\s+([\d.]+) (Gbit|Mbit)/s", output)
    return float(found[1]) / (1 if found[2] == "Gbit" else 1000) if found else None


def per_gib(seconds: float | None, gbps: float | None) -> float | None:
    return round(seconds / (gbps * 1e9 / 8 * SECONDS / 2**30), 3) if seconds is not None and gbps else None


def command_for(client: Path, server: Server, listener: str, direction: str) -> list[str]:
    return ["taskset", "-c", CLIENT_CPUS, str(client), "--report", "--insecure", "--url", server.origin("http1"),
            "--stages", direction, "--streams", "4", "--throughput-origin", server.origin(listener),
            "--throughput-protocol", listener, "--throughput-transport", "fetch-stream", "--warmup", "500ms",
            f"--{direction}-duration", f"{SECONDS}s", "--loaded-latency=false"]


def cell(fixture: Fixture, variant: str, listener: str, direction: str) -> dict[str, object]:
    server = fixture.server(pinned(BIN / f"{variant}-server", fixture.directory), variant,
                            {"TOKIO_WORKER_THREADS": "2"})
    with server:
        assert server.process is not None
        pid = server.process.pid
        before = cpu(pid)
        command = command_for(BIN / f"{variant}-client", server, listener, direction)
        run = subprocess.run(["/usr/bin/time", "-f", "GM_TIME %U %S", *command], capture_output=True, text=True,
                             timeout=SECONDS * 8)
        seconds = cpu(pid) - before
    output = run.stdout + run.stderr
    clock = re.search(r"GM_TIME ([\d.]+) ([\d.]+)", output)
    used = float(clock[1]) + float(clock[2]) if clock else None
    gbps = rate(run.stdout)
    return {"variant": variant, "case": f"{listener} {direction}", "mbps": gbps * 1000 if gbps else None,
            "clientCpuPerGiB": per_gib(used, gbps), "serverCpuPerGiB": per_gib(seconds, gbps), "report": run.stdout}


def measure(study: str) -> None:
    results = OUT / "results"
    results.mkdir(parents=True, exist_ok=True)
    fixture = Fixture(f"campaign-{study}-")
    subprocess.run(["sudo", "tc", "qdisc", "replace", "dev", "lo", "root", "netem", *SHAPES[study]], check=True)
    rows = []
    try:
        with (results / f"{study}-runs.jsonl").open("w") as sink:
            for repeat in range(REPEATS):
                for index in range(len(CASES)):
                    listener, direction = CASES[(index + repeat) % len(CASES)]
                    shift = (repeat + index) % len(VARIANTS)
                    for variant in VARIANTS[shift:] + VARIANTS[:shift]:
                        row = cell(fixture, variant, listener, direction) | {"repeat": repeat}
                        rows.append(row)
                        sink.write(json.dumps(row) + "\n")
                        sink.flush()
                        print(json.dumps({key: value for key, value in row.items() if key != "report"}), flush=True)
    finally:
        subprocess.run(["sudo", "tc", "qdisc", "del", "dev", "lo", "root"], check=False)
    summarize(study, rows, results)


def summarize(study: str, rows: list[dict[str, object]], results: Path) -> None:
    """Medians over repeats with their range, variants side by side."""
    lines = [f"### Campaign · {study} study · {os.uname().machine} · ci profile · median (range)", "",
             "| Case | Metric | " + " | ".join(VARIANTS) + " |", "|---|---|" + "---|" * len(VARIANTS)]
    for case in sorted({str(row["case"]) for row in rows}):
        for metric in ("mbps", "clientCpuPerGiB", "serverCpuPerGiB"):
            cells = []
            for variant in VARIANTS:
                values = [float(value) for row in rows if row["variant"] == variant and row["case"] == case
                          and isinstance(value := row[metric], (int, float))]
                cells.append(f"{statistics.median(values):.3f} ({min(values):.3f}–{max(values):.3f})"
                             if values else "—")
            lines.append(f"| {case} | {metric} | " + " | ".join(cells) + " |")
    text = "\n".join(lines) + "\n"
    (results / f"{study}-summary.md").write_text(text)
    print(text, flush=True)


if __name__ == "__main__":
    if sys.argv[1] == "build":
        build()
    else:
        measure(sys.argv[2])
