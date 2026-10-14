"""Temporary campaign for the Rust review pull request, second run: what nicing bulk threads buys and costs.

build: ci-profile musl server and client of the merge commit's base (HEAD^1) and of the merge commit; the merge
    commit's server with its pinned runtimes niced as the TUI's are; and the merge commit's TUI with un-niced lanes.
measure server: the server pinned to two cores; a Python WebSocket prober on its own core records every RTT while the
    candidate TUI moves bulk traffic on the fourth. Variants: base, cand, and cand with niced pinned runtimes. Reports
    RTT tails, throughput and server CPU per GiB.
measure client: the TUI with niced and with un-niced lanes against the candidate server, CPU-bound on two cores: its
    throughput and loaded probes under reply-driven and the default Medium loaded cadence.
"""
from __future__ import annotations

import base64
import gc
import json
import os
import re
import shutil
import socket
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

from rust.interop.fixture import Fixture, Server, musl_target

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "rust/target/campaign"
BIN = OUT / "bin"
SERVER_NICED = [("rust/server/src/main.rs", "let pool = Pool::beside(runtime.handle())",
                 "let pool = Pool::probing(runtime.handle())")]
LANES_UNNICED = [("rust/net/src/runtime.rs", "const PINNED_NICE: i32 = 5;", "const PINNED_NICE: i32 = 0;")]
# Each build: its tree's commit, its edits and the binaries it provides.
BUILDS = {
    "base": ("HEAD^1", [], ("server", "client")),
    "cand": ("HEAD", [], ("server", "client")),
    "srvnice": ("HEAD", SERVER_NICED, ("server",)),
    "unniced": ("HEAD", LANES_UNNICED, ("client",)),
}
SECONDS, REPEATS = 20, 5
# The server; the TUI beside the prober, or alone on two cores; the prober.
SERVER_CPUS, BULK_CPUS, CLIENT_CPUS, PROBE_CPU = "0,1", "2", "2,3", 3
# The probe interval; RTTs are measured, not the cadence.
PROBE_GAP = 0.001
# Each study's variants: the server build, its extra settings, and the client build.
VARIANTS: dict[str, dict[str, tuple[str, dict[str, str], str]]] = {
    "server": {"base": ("base", {}, "cand"), "cand": ("cand", {}, "cand"), "niced": ("srvnice", {}, "cand")},
    "client": {"niced": ("cand", {}, "cand"), "unniced": ("cand", {}, "unniced")},
}
# Bulk work beside the probes: listener, direction.
SERVER_CASES = [("http3", "upload"), ("http1-tls", "upload"), ("http3", "download")]
# The TUI's download listener, latency transport and loaded cadence.
CLIENT_CASES = [("http1-tls", "websocket", "reply-driven"), ("http1-tls", "websocket", "medium"),
                ("http3", "webtransport", "reply-driven")]


def git(*arguments: str) -> str:
    return subprocess.run(["git", *arguments], cwd=ROOT, check=True, capture_output=True, text=True).stdout.strip()


def build() -> None:
    BIN.mkdir(parents=True, exist_ok=True)
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(("GM_", "CARGO_PROFILE_")) and not key.endswith("RUSTFLAGS")}
    environment |= {"CARGO_INCREMENTAL": "0"}
    musl = musl_target()
    environment |= {"CC_" + musl.replace("-", "_"): f"{musl.split('-')[0]}-linux-musl-gcc"}
    subprocess.run(["rustup", "target", "add", musl], cwd=ROOT / "rust", check=True)
    target = musl
    for label, (commit, edits, sides) in BUILDS.items():
        tree = ROOT if label == "cand" else OUT / f"{label}-src"
        if tree != ROOT and not tree.exists():
            git("worktree", "add", "--detach", str(tree), git("rev-parse", commit))
            for path, old, new in edits:
                text = (tree / path).read_text()
                if text.count(old) != 1:
                    sys.exit(f"{label}: {old!r} is not in {path} exactly once")
                (tree / path).write_text(text.replace(old, new))
        # Legal notice embedding needs each workspace's build outputs under its own target directory.
        built = tree / "rust/target/campaign-build"
        packages = [argument for side in sides for argument in ("-p", f"graphite-meter-{side}")]
        command = ["cargo", "build", "--locked", "--profile", "ci", "--target", target, *packages]
        with (OUT / f"build-{label}.log").open("w") as log:
            status = subprocess.run(command, cwd=tree / "rust", env={**environment, "CARGO_TARGET_DIR": str(built)},
                                    stdout=log, stderr=subprocess.STDOUT).returncode
        if status:
            sys.exit(f"{label} build failed:\n" + (OUT / f"build-{label}.log").read_text()[-6000:])
        for side in sides:
            shutil.copy2(built / target / "ci" / f"graphite-meter-{side}", BIN / f"{label}-{side}")


def cpu(pid: int) -> float:
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def pinned(binary: Path, directory: Path) -> Path:
    """`binary` behind `exec taskset`, which keeps the process ID the CPU accounting reads."""
    wrapper = directory / f"{binary.name}-pinned"
    wrapper.write_text(f"#!/bin/sh\nexec taskset -c {SERVER_CPUS} {binary} \"$@\"\n")
    wrapper.chmod(0o755)
    return wrapper


class Prober(threading.Thread):
    """Reply-paced WebSocket probes every PROBE_GAP over the clear listener, each RTT kept in nanoseconds."""

    def __init__(self, port: int) -> None:
        super().__init__(daemon=True)
        self.port, self.stop, self.rtts = port, threading.Event(), list[int]()
        self.error: str | None = None

    def run(self) -> None:
        os.sched_setaffinity(0, {PROBE_CPU})
        try:
            with socket.create_connection(("127.0.0.1", self.port)) as connection:
                connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                reader = connection.makefile("rb")
                nonce = base64.b64encode(os.urandom(16))
                connection.sendall(b"GET /ws/ping HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\n"
                                   b"Connection: Upgrade\r\nSec-WebSocket-Key: " + nonce + b"\r\n"
                                   b"Sec-WebSocket-Version: 13\r\n\r\n")
                head = b""
                while not head.endswith(b"\r\n\r\n"):
                    head += reader.read(1)
                if b" 101 " not in head.split(b"\r\n", 1)[0]:
                    raise RuntimeError(head.decode(errors="replace"))
                number = 0
                while not self.stop.is_set():
                    started = time.perf_counter_ns()
                    payload, mask = f"PING,{number}".encode(), os.urandom(4)
                    masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
                    connection.sendall(bytes([0x81, 0x80 | len(payload)]) + mask + masked)
                    while True:
                        first, length = reader.read(2)
                        reply = reader.read(length & 0x7F)
                        if first & 0x0F == 1 and reply.startswith(f"PONG,{number},".encode()):
                            break
                    self.rtts.append(time.perf_counter_ns() - started)
                    number += 1
                    time.sleep(max(0.0, PROBE_GAP - (time.perf_counter_ns() - started) / 1e9))
        except (OSError, RuntimeError, ValueError) as error:
            self.error = repr(error)


def tails(rtts: list[int]) -> dict[str, float]:
    if not rtts:
        return {}
    ordered = sorted(rtts)
    at = lambda share: ordered[min(len(ordered) - 1, int(share * len(ordered)))] / 1e6
    return {"count": len(ordered), "p50": at(0.5), "p90": at(0.9), "p99": at(0.99), "p999": at(0.999),
            "max": ordered[-1] / 1e6}


def rate(output: str) -> float | None:
    found = re.search(r"[↓↑] (?:Download|Upload)\s+([\d.]+) (Gbit|Mbit)/s", output)
    return float(found[1]) / (1 if found[2] == "Gbit" else 1000) if found else None


def loaded(output: str) -> dict[str, float]:
    """The loaded row's resolved probes per second and its P95, `< 0.1 ms` read as 0.1."""
    found = re.search(r"Loaded down\s+(.+?)\s+(\d[\d,]*) / ([\d,]+)\s*$", output, re.MULTILINE)
    if not found:
        return {}
    # Median, Added, P95 and Jitter.
    columns = re.split(r"\s{2,}", found[1].strip())
    p95 = re.fullmatch(r"(?:<\s)?([\d.]+) ms", columns[2]) if len(columns) > 2 else None
    row = {"probesPerSecond": round(int(found[3].replace(",", "")) / SECONDS, 1)}
    return row | ({"p95": float(p95[1])} if p95 else {})


def timed(command: list[str]) -> tuple[str, float | None]:
    run = subprocess.run(["/usr/bin/time", "-f", "GM_TIME %U %S", *command], capture_output=True, text=True,
                         timeout=SECONDS * 4)
    output = run.stdout + run.stderr
    clock = re.search(r"GM_TIME ([\d.]+) ([\d.]+)", output)
    return output, float(clock[1]) + float(clock[2]) if clock else None


def bulk(cpus: str, client: Path, server: Server, listener: str, direction: str, *extra: str) -> list[str]:
    protocol = "http1" if listener.startswith("http1") else listener
    return ["taskset", "-c", cpus, str(client), "--report", "--insecure", "--url", server.origin("http1"),
            "--stages", direction, "--streams", "4", "--throughput-origin", server.origin(listener),
            "--throughput-protocol", protocol, "--throughput-transport", "fetch-stream", "--warmup", "500ms",
            f"--{direction}-duration", f"{SECONDS}s", *extra]


def cell(fixture: Fixture, study: str, variant: str, case: tuple[str, ...]) -> dict[str, object]:
    server_build, settings, client_build = VARIANTS[study][variant]
    settings = {"TOKIO_WORKER_THREADS": "2"} | settings
    server = fixture.server(pinned(BIN / f"{server_build}-server", fixture.directory), f"{study}-{variant}", settings)
    client = BIN / f"{client_build}-client"
    listener, second = case[:2]
    row: dict[str, object] = {"study": study, "variant": variant, "case": " ".join(case)}
    with server:
        assert server.process is not None
        pid = server.process.pid
        before = cpu(pid)
        output, gbps = "", None
        if study == "server":
            prober = Prober(server.h1)
            prober.start()
            run = subprocess.run(bulk(BULK_CPUS, client, server, listener, second, "--loaded-latency=false"),
                                 capture_output=True, text=True, timeout=SECONDS * 4)
            output, gbps = run.stdout + run.stderr, rate(run.stdout)
            prober.stop.set()
            prober.join(timeout=5)
            seconds = cpu(pid) - before
            row |= {"rtt": tails(prober.rtts), "probeError": prober.error}
        else:
            origin = server.origin("http3" if second == "webtransport" else "http1")
            latency = ["--latency-transport", second, "--latency-origin", origin]
            command = bulk(CLIENT_CPUS, client, server, listener, "download", "--loaded-latency=true",
                           "--loaded-ping", case[2], *latency)
            output, used = timed(command)
            gbps = rate(output)
            row |= {"loaded": loaded(output), "clientCpu": used}
            seconds = cpu(pid) - before
        row |= {"gbps": gbps, "serverCpu": round(seconds, 3),
                "serverCpuPerGiB": round(seconds / (gbps * 1e9 / 8 * SECONDS / 2**30), 3) if gbps else None}
        (fixture.directory / f"{study}-{variant}-{listener}-{second}.log").write_text(output)
    return row


def measure(study: str) -> None:
    results = OUT / "results"
    results.mkdir(parents=True, exist_ok=True)
    fixture = Fixture(f"campaign-{study}-")
    cases: list[tuple[str, ...]] = list({"server": SERVER_CASES, "client": CLIENT_CASES}[study])
    variants = tuple(VARIANTS[study])
    gc.disable()
    rows = []
    with (results / f"{study}-runs.jsonl").open("w") as sink:
        for repeat in range(REPEATS):
            for index in range(len(cases)):
                case = cases[(index + repeat) % len(cases)]
                shift = (repeat + index) % len(variants)
                for variant in variants[shift:] + variants[:shift]:
                    row = cell(fixture, study, variant, case) | {"repeat": repeat}
                    rows.append(row)
                    sink.write(json.dumps(row) + "\n")
                    sink.flush()
                    print(json.dumps(row), flush=True)
    summarize(study, variants, rows, results)
    shutil.copytree(fixture.directory, results / f"evidence-{study}", dirs_exist_ok=True)


def summarize(study: str, variants: tuple[str, ...], rows: list[dict[str, object]], results: Path) -> None:
    """Medians over repeats with their range, variants side by side."""
    def number(row: dict[str, object], *path: str) -> float | None:
        value: object = row
        for key in path:
            value = value.get(key) if isinstance(value, dict) else None
        return float(value) if isinstance(value, (int, float)) else None

    metrics = {
        "server": [("rtt", "p50"), ("rtt", "p99"), ("rtt", "p999"), ("rtt", "max"), ("gbps",), ("serverCpuPerGiB",)],
        "client": [("gbps",), ("loaded", "probesPerSecond"), ("loaded", "p95"), ("clientCpu",)],
    }[study]
    lines = [f"### Campaign · {study} study · {os.uname().machine} · ci profile · median (range)", "",
             "| Case | Metric | " + " | ".join(variants) + " |", "|---|---|" + "---|" * len(variants)]
    summary: dict[str, dict[str, dict[str, object]]] = {}
    for case in sorted({str(row["case"]) for row in rows}):
        for metric in metrics:
            cells = []
            for variant in variants:
                values = [value for row in rows if row["variant"] == variant and row["case"] == case
                          and (value := number(row, *metric)) is not None]
                entry = {"median": round(statistics.median(values), 3), "min": round(min(values), 3),
                         "max": round(max(values), 3), "n": len(values)} if values else {}
                summary.setdefault(case, {}).setdefault(".".join(metric), {})[variant] = entry
                cells.append(f"{entry['median']} ({entry['min']}–{entry['max']})" if entry else "—")
            if any(cell != "—" for cell in cells):
                lines.append(f"| {case} | {'.'.join(metric)} | " + " | ".join(cells) + " |")
    (results / f"{study}-summary.json").write_text(json.dumps(summary, indent=1) + "\n")
    text = "\n".join(lines) + "\n"
    (results / f"{study}-summary.md").write_text(text + "\n")
    print(text, flush=True)


if __name__ == "__main__":
    if sys.argv[1] == "build":
        build()
    else:
        measure(sys.argv[2])
