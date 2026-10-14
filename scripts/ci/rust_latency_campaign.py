"""Temporary campaign for the Rust review pull request, fourth run: the open measurement questions at once.

build: ci-profile musl binaries of the head and of variants (edits on the head, or another noq pin), plus a PGO build
    trained on loopback transfers.
measure tcp|quic: the server pinned to two cores; a Python WebSocket prober on its own core records every RTT while the
    head TUI moves bulk traffic on the fourth. RTT tails, throughput and server CPU per GiB.
measure client: the TUI CPU-bound on two cores against the head server: throughput and CPU per GiB.
measure shaped: HTTP/3 over loopback shaped by netem (20 ms round trip, 1 Gbit/s): throughput and CPU per GiB.
measure wt: the TUI's loaded WebTransport latency during an HTTP/3 download, with and without kernel routing.
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
from dataclasses import dataclass
from pathlib import Path

from rust.interop.fixture import Fixture, Server, musl_target

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "rust/target/campaign"
BIN = OUT / "bin"
NOQ = 'noq = { git = "https://github.com/zR-JB/noq", rev = "%s" }'
HEAD_NOQ = "a459ad8525063ddae3bc9fa1385826d6848a0ce3"
# The pin before the fork update, and its upstream fixes without the two graphite-meter performance commits.
OLD_NOQ = "94e4e10bf84f5ffcad04f8c471dc8e98b980298f"
MID_NOQ = "136498398b639310fced6d0c9db20e255a5e5db7"
ACK_EVERY_TEN = """    let mut ack = noq::AckFrequencyConfig::default();
    ack.ack_eliciting_threshold(VarInt::from_u32(9));
    transport.ack_frequency_config(Some(ack));
    transport
}"""
# Each build: its edits (path, old, new, occurrences), whether Cargo may relock, and the binaries it provides.
Edit = tuple[str, str, str, int]
BUILDS: dict[str, tuple[list[Edit], bool, tuple[str, ...]]] = {
    "head": ([], False, ("server", "client")),
    "yield": ([("rust/server/src/transport/mod.rs", "        self.0 = 0;\n        // A fresh",
                "        self.0 = 0;\n        std::thread::yield_now();\n        // A fresh", 1)], False, ("server",)),
    "norouting": ([("rust/server/src/transport/quic/endpoint.rs",
                    "let routed = if sockets > 1 { route_in_kernel(&socket) } else { Ok(()) };",
                    "let routed = Err(io::ErrorKind::Unsupported.into());", 1)], False, ("server",)),
    "oldnoq": ([("rust/Cargo.toml", NOQ % HEAD_NOQ, NOQ % OLD_NOQ, 1)], True, ("server",)),
    "midnoq": ([("rust/Cargo.toml", NOQ % HEAD_NOQ, NOQ % MID_NOQ, 1)], True, ("server",)),
    "ackfreq": ([("rust/net/src/quic.rs", "    transport\n}", ACK_EVERY_TEN, 2)], False, ("server", "client")),
}
SECONDS = int(os.environ.get("CAMPAIGN_SECONDS", "15"))
REPEATS = int(os.environ.get("CAMPAIGN_REPEATS", "6"))
# The server; the TUI beside the prober, or alone on two cores; the prober.
SERVER_CPUS, BULK_CPUS, CLIENT_CPUS, PROBE_CPU = "0,1", "2", "2,3", 3
# The probe interval; RTTs are measured, not the cadence.
PROBE_GAP = 0.001
SHAPE = ["netem", "delay", "10ms", "rate", "1gbit", "limit", "20000"]


@dataclass(frozen=True)
class Study:
    """Its cases (listener, direction), how a cell runs, and its variants' server and client builds."""
    cases: tuple[tuple[str, str], ...]
    kind: str
    variants: dict[str, tuple[str, str]]


STUDIES = {
    "tcp": Study((("http1-tls", "upload"), ("http2", "download")), "probe",
                 {"head": ("head", "head"), "yield": ("yield", "head"), "pgo": ("pgo", "head")}),
    "quic": Study((("http3", "download"), ("http3", "upload")), "probe",
                  {"head": ("head", "head"), "oldnoq": ("oldnoq", "head"), "midnoq": ("midnoq", "head"),
                   "ackfreq": ("ackfreq", "ackfreq"), "pgo": ("pgo", "head")}),
    "client": Study((("http3", "download"), ("http3", "upload"), ("http2", "download")), "client",
                    {"head": ("head", "head"), "ackfreq": ("ackfreq", "ackfreq"), "pgo": ("head", "pgo")}),
    "shaped": Study((("http3", "download"), ("http3", "upload")), "client",
                    {"head": ("head", "head"), "ackfreq": ("ackfreq", "ackfreq")}),
    "wt": Study((("http3", "download"),), "wt", {"head": ("head", "head"), "norouting": ("norouting", "head")}),
}


def git(*arguments: str) -> str:
    return subprocess.run(["git", *arguments], cwd=ROOT, check=True, capture_output=True, text=True).stdout.strip()


def environment() -> dict[str, str]:
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("GM_", "CARGO_PROFILE_")) and not key.endswith("RUSTFLAGS")}
    musl = musl_target()
    return env | {"CARGO_INCREMENTAL": "0", "CC_" + musl.replace("-", "_"): f"{musl.split('-')[0]}-linux-musl-gcc"}


def compile_tree(label: str, tree: Path, sides: tuple[str, ...], locked: bool, rustflags: str = "") -> Path:
    """The tree's binaries for `sides`; legal notice embedding needs each tree's outputs under its own target."""
    target, built = musl_target(), tree / "rust/target/campaign-build"
    env = environment() | {"CARGO_TARGET_DIR": str(built)}
    if rustflags:
        env["RUSTFLAGS"] = rustflags
    packages = [argument for side in sides for argument in ("-p", f"graphite-meter-{side}")]
    command = ["cargo", "build", *(["--locked"] if locked else []), "--profile", "ci", "--target", target, *packages]
    with (OUT / f"build-{label}.log").open("w") as log:
        status = subprocess.run(command, cwd=tree / "rust", env=env, stdout=log, stderr=subprocess.STDOUT).returncode
    if status:
        sys.exit(f"{label} build failed:\n" + (OUT / f"build-{label}.log").read_text()[-6000:])
    return built / target / "ci"


def variant_tree(label: str, edits: list[Edit]) -> Path:
    tree = OUT / f"{label}-src"
    if not tree.exists():
        git("worktree", "add", "--detach", str(tree), git("rev-parse", "HEAD"))
        for path, old, new, count in edits:
            text = (tree / path).read_text()
            if text.count(old) != count:
                sys.exit(f"{label}: {old!r} is not in {path} {count} times")
            (tree / path).write_text(text.replace(old, new))
    return tree


def build() -> None:
    BIN.mkdir(parents=True, exist_ok=True)
    subprocess.run(["rustup", "target", "add", musl_target()], cwd=ROOT / "rust", check=True)
    subprocess.run(["rustup", "component", "add", "llvm-tools"], cwd=ROOT / "rust", check=True)
    for label, (edits, relock, sides) in BUILDS.items():
        tree = ROOT if label == "head" else variant_tree(label, edits)
        output = compile_tree(label, tree, sides, locked=not relock)
        for side in sides:
            shutil.copy2(output / f"graphite-meter-{side}", BIN / f"{label}-{side}")
    # A failed PGO build drops its variants rather than the campaign.
    try:
        pgo()
    except (SystemExit, subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError) as error:
        (OUT / "pgo-failed.txt").write_text(f"{error!r}\n")
        print(f"PGO build failed: {error!r}", flush=True)


def pgo() -> None:
    """Instrumented binaries trained on every listener and direction, then a build that uses the merged profile."""
    profiles = OUT / "pgo-profiles"
    shutil.rmtree(profiles, ignore_errors=True)
    profiles.mkdir(parents=True)
    tree = variant_tree("pgo", [])
    output = compile_tree("pgo-instrumented", tree, ("server", "client"), True, f"-Cprofile-generate={profiles}")
    for side in ("server", "client"):
        shutil.copy2(output / f"graphite-meter-{side}", BIN / f"pgo-instrumented-{side}")
    fixture = Fixture("campaign-pgo-")
    server = fixture.server(pinned(BIN / "pgo-instrumented-server", fixture.directory), "train",
                            {"TOKIO_WORKER_THREADS": "2"})
    with server:
        for listener, direction in [("http3", "download"), ("http3", "upload"), ("http2", "download"),
                                    ("http2", "upload"), ("http1-tls", "upload"), ("http1-tls", "download")]:
            subprocess.run(bulk(CLIENT_CPUS, BIN / "pgo-instrumented-client", server, listener, direction,
                                "--loaded-latency=true", seconds=8), capture_output=True, timeout=120)
        subprocess.run(wt_command(BIN / "pgo-instrumented-client", server, seconds=8), capture_output=True,
                       timeout=120)
    raw = sorted(profiles.glob("*.profraw"))
    if not raw:
        sys.exit("PGO training wrote no profiles")
    sysroot = subprocess.run(["rustc", "--print", "sysroot"], cwd=ROOT / "rust", check=True, capture_output=True,
                             text=True).stdout.strip()
    host = re.search(r"host: (\S+)", subprocess.run(["rustc", "-vV"], cwd=ROOT / "rust", check=True,
                                                     capture_output=True, text=True).stdout)
    assert host is not None
    merged = OUT / "pgo.profdata"
    tool = Path(sysroot) / "lib/rustlib" / host[1] / "bin/llvm-profdata"
    subprocess.run([str(tool), "merge", "-o", str(merged), *map(str, raw)], check=True)
    shutil.rmtree(tree / "rust/target/campaign-build")
    output = compile_tree("pgo", tree, ("server", "client"), True, f"-Cprofile-use={merged}")
    for side in ("server", "client"):
        shutil.copy2(output / f"graphite-meter-{side}", BIN / f"pgo-{side}")


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


def timed(command: list[str]) -> tuple[str, float | None]:
    run = subprocess.run(["/usr/bin/time", "-f", "GM_TIME %U %S", *command], capture_output=True, text=True,
                         timeout=SECONDS * 6)
    output = run.stdout + run.stderr
    clock = re.search(r"GM_TIME ([\d.]+) ([\d.]+)", output)
    return output, float(clock[1]) + float(clock[2]) if clock else None


def bulk(cpus: str, client: Path, server: Server, listener: str, direction: str, *extra: str,
         seconds: int = SECONDS) -> list[str]:
    protocol = "http1" if listener.startswith("http1") else listener
    return ["taskset", "-c", cpus, str(client), "--report", "--insecure", "--url", server.origin("http1"),
            "--stages", direction, "--streams", "4", "--throughput-origin", server.origin(listener),
            "--throughput-protocol", protocol, "--throughput-transport", "fetch-stream", "--warmup", "500ms",
            f"--{direction}-duration", f"{seconds}s", *extra]


def wt_command(client: Path, server: Server, seconds: int = SECONDS) -> list[str]:
    return bulk(CLIENT_CPUS, client, server, "http3", "download", "--loaded-latency=true", "--loaded-ping",
                "reply-driven", "--latency-transport", "webtransport", "--latency-origin", server.origin("http3"),
                seconds=seconds)


def per_gib(seconds: float | None, gbps: float | None) -> float | None:
    return round(seconds / (gbps * 1e9 / 8 * SECONDS / 2**30), 3) if seconds is not None and gbps else None


def cell(fixture: Fixture, study: str, variant: str, case: tuple[str, str]) -> dict[str, object]:
    spec = STUDIES[study]
    server_build, client_build = spec.variants[variant]
    server = fixture.server(pinned(BIN / f"{server_build}-server", fixture.directory), f"{study}-{variant}",
                            {"TOKIO_WORKER_THREADS": "2"})
    client = BIN / f"{client_build}-client"
    listener, direction = case[:2]
    row: dict[str, object] = {"study": study, "variant": variant, "case": " ".join(case)}
    with server:
        assert server.process is not None
        pid = server.process.pid
        before = cpu(pid)
        if spec.kind == "probe":
            prober = Prober(server.h1)
            prober.start()
            run = subprocess.run(bulk(BULK_CPUS, client, server, listener, direction, "--loaded-latency=false"),
                                 capture_output=True, text=True, timeout=SECONDS * 6)
            output, gbps = run.stdout + run.stderr, rate(run.stdout)
            prober.stop.set()
            prober.join(timeout=5)
            row |= {"rtt": tails(prober.rtts), "probeError": prober.error}
        elif spec.kind == "client":
            output, used = timed(bulk(CLIENT_CPUS, client, server, listener, direction, "--loaded-latency=false"))
            gbps = rate(output)
            row |= {"clientCpu": used, "clientCpuPerGiB": per_gib(used, gbps)}
        else:
            run = subprocess.run(wt_command(client, server), capture_output=True, text=True, timeout=SECONDS * 6)
            output, gbps = run.stdout + run.stderr, rate(run.stdout)
            loaded = re.search(r"Loaded down\s+(.+?)\s+(\d[\d,]*) / ([\d,]+)\s*$", run.stdout, re.MULTILINE)
            if loaded:
                columns = re.split(r"\s{2,}", loaded[1].strip())
                row |= {"loaded": loaded[0].strip(), "probesPerSecond": round(int(loaded[3].replace(",", "")) / SECONDS),
                        "median": columns[0], "p95": columns[2] if len(columns) > 2 else None}
        seconds = cpu(pid) - before
        row |= {"gbps": gbps, "serverCpu": round(seconds, 3), "serverCpuPerGiB": per_gib(seconds, gbps)}
        (fixture.directory / f"{study}-{variant}-{listener}-{direction}.log").write_text(output)
    return row


def measure(study: str) -> None:
    results = OUT / "results"
    results.mkdir(parents=True, exist_ok=True)
    fixture = Fixture(f"campaign-{study}-")
    spec = STUDIES[study]
    cases = list(spec.cases)
    # Variants whose builds failed are left out.
    variants = tuple(name for name, (server, client) in spec.variants.items()
                     if (BIN / f"{server}-server").exists() and (BIN / f"{client}-client").exists())
    repeats = REPEATS * 2 if study == "wt" else REPEATS
    shaped = study == "shaped"
    if shaped and subprocess.run(["sudo", "tc", "qdisc", "replace", "dev", "lo", "root", *SHAPE]).returncode:
        (results / "shaped-unavailable.txt").write_text("netem could not shape loopback\n")
        return
    gc.disable()
    rows = []
    try:
        with (results / f"{study}-runs.jsonl").open("w") as sink:
            for repeat in range(repeats):
                for index in range(len(cases)):
                    case = cases[(index + repeat) % len(cases)]
                    shift = (repeat + index) % len(variants)
                    for variant in variants[shift:] + variants[:shift]:
                        row = cell(fixture, study, variant, case) | {"repeat": repeat}
                        rows.append(row)
                        sink.write(json.dumps(row) + "\n")
                        sink.flush()
                        print(json.dumps(row), flush=True)
    finally:
        if shaped:
            subprocess.run(["sudo", "tc", "qdisc", "del", "dev", "lo", "root"], check=False)
    summarize(study, variants, rows, results)
    shutil.copytree(fixture.directory, results / f"evidence-{study}", dirs_exist_ok=True)


def summarize(study: str, variants: tuple[str, ...], rows: list[dict[str, object]], results: Path) -> None:
    """Medians over repeats with their range, variants side by side."""
    def number(row: dict[str, object], *path: str) -> float | None:
        value: object = row
        for key in path:
            value = value.get(key) if isinstance(value, dict) else None
        return float(value) if isinstance(value, (int, float)) else None

    kind = STUDIES[study].kind
    metrics = {
        "probe": [("rtt", "p50"), ("rtt", "p99"), ("rtt", "p999"), ("rtt", "max"), ("gbps",), ("serverCpuPerGiB",)],
        "client": [("gbps",), ("clientCpuPerGiB",), ("serverCpuPerGiB",)],
        "wt": [("probesPerSecond",), ("gbps",), ("serverCpuPerGiB",)],
    }[kind]
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
    if kind == "wt":
        for variant in variants:
            medians = [str(row.get("median")) for row in rows if row["variant"] == variant]
            lines.append(f"\n{variant} medians: {', '.join(medians)}")
    (results / f"{study}-summary.json").write_text(json.dumps(summary, indent=1) + "\n")
    text = "\n".join(lines) + "\n"
    (results / f"{study}-summary.md").write_text(text + "\n")
    print(text, flush=True)


if __name__ == "__main__":
    if sys.argv[1] == "build":
        build()
    else:
        measure(sys.argv[2])
