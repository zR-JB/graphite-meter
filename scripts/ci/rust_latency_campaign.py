"""Temporary campaign for the Rust review pull request: base against candidate, alternating, on pinned cores.

build: ci-profile server and client of the merge commit's base (HEAD^1) and of the merge commit, musl; and the merge
    commit's server on glibc with mimalloc, so libc's memcpy shows apart from the allocator.
measure server: the server pinned to two cores; a Python WebSocket prober on its own core records every RTT while the
    candidate TUI moves bulk traffic on the fourth. Variants: base, cand, cand with MIMALLOC_PURGE_DELAY=600000, and
    the glibc build. Reports RTT tails, throughput, server CPU per GiB, peak RSS and RSS once idle.
measure client: the TUI varied against the candidate server: its CPU per reply in a reply-driven idle stage, and its
    own loaded latency, over WebTransport datagrams and WebSocket.
measure handshake: full TLS 1.3 handshakes against the candidate server with a P-256 and an RSA-2048 leaf; reports
    server CPU per handshake.
"""
from __future__ import annotations

import base64
import gc
import json
import os
import platform
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
# mimalloc on glibc too.
GLIBC_MIMALLOC = [
    ("rust/server/src/main.rs", '#[cfg(target_env = "musl")]\n#[global_allocator]',
     '#[cfg(any(target_env = "musl", target_env = "gnu"))]\n#[global_allocator]'),
    ("rust/server/Cargo.toml", "[target.'cfg(target_env = \"musl\")'.dependencies]\nrustfs-mimalloc",
     "[target.'cfg(any(target_env = \"musl\", target_env = \"gnu\"))'.dependencies]\nrustfs-mimalloc"),
]
# Each build: its tree's commit, its edits and its libc.
BUILDS = {"base": ("HEAD^1", [], "musl"), "cand": ("HEAD", [], "musl"), "gnu": ("HEAD", GLIBC_MIMALLOC, "gnu")}
SECONDS, REPEATS = 20, 5
# The server; the TUI beside the prober, or alone on two cores; the prober.
SERVER_CPUS, BULK_CPUS, CLIENT_CPUS, PROBE_CPU = "0,1", "2", "2,3", 3
# The probe interval; RTTs are measured, not the cadence.
PROBE_GAP = 0.001
# Each study's variants: the server build, its extra settings, and the client build.
VARIANTS: dict[str, dict[str, tuple[str, dict[str, str], str]]] = {
    "server": {
        "base": ("base", {}, "cand"),
        "cand": ("cand", {}, "cand"),
        "purge": ("cand", {"MIMALLOC_PURGE_DELAY": "600000"}, "cand"),
        "gnu": ("gnu", {}, "cand"),
    },
    "client": {"base": ("cand", {}, "base"), "cand": ("cand", {}, "cand")},
    "handshake": {"ecdsa": ("cand", {}, "cand"), "rsa": ("cand", {"rsa": "1"}, "cand")},
}
# Bulk work beside the probes: listener, direction.
SERVER_CASES = [("http3", "upload"), ("http1-tls", "upload"), ("http3", "download")]
# The TUI: throughput listener or idle, and latency transport.
CLIENT_CASES = [("idle", "webtransport"), ("idle", "websocket"), ("http3", "webtransport"), ("http1-tls", "websocket")]
HANDSHAKE_CASES = [("http1-tls", "full")]


def git(*arguments: str) -> str:
    return subprocess.run(["git", *arguments], cwd=ROOT, check=True, capture_output=True, text=True).stdout.strip()


def target_of(libc: str) -> str:
    return musl_target() if libc == "musl" else f"{platform.machine()}-unknown-linux-gnu"


def build() -> None:
    BIN.mkdir(parents=True, exist_ok=True)
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(("GM_", "CARGO_PROFILE_")) and not key.endswith("RUSTFLAGS")}
    environment |= {"CARGO_INCREMENTAL": "0"}
    musl = musl_target()
    environment |= {"CC_" + musl.replace("-", "_"): f"{musl.split('-')[0]}-linux-musl-gcc"}
    subprocess.run(["rustup", "target", "add", musl], cwd=ROOT / "rust", check=True)
    for label, (commit, edits, libc) in BUILDS.items():
        tree = ROOT if label == "cand" else OUT / f"{label}-src"
        if tree != ROOT and not tree.exists():
            git("worktree", "add", "--detach", str(tree), git("rev-parse", commit))
            for path, old, new in edits:
                text = (tree / path).read_text()
                if text.count(old) != 1:
                    sys.exit(f"{label}: {old!r} is not in {path} exactly once")
                (tree / path).write_text(text.replace(old, new))
        target = target_of(libc)
        # Legal notice embedding needs each workspace's build outputs under its own target directory.
        built = tree / "rust/target/campaign-build"
        packages = ["-p", "graphite-meter-server"] + ([] if label == "gnu" else ["-p", "graphite-meter-client"])
        command = ["cargo", "build", "--locked", "--profile", "ci", "--target", target, *packages]
        with (OUT / f"build-{label}.log").open("w") as log:
            status = subprocess.run(command, cwd=tree / "rust", env={**environment, "CARGO_TARGET_DIR": str(built)},
                                    stdout=log, stderr=subprocess.STDOUT).returncode
        if status:
            sys.exit(f"{label} build failed:\n" + (OUT / f"build-{label}.log").read_text()[-6000:])
        for side in ("server",) if label == "gnu" else ("server", "client"):
            shutil.copy2(built / target / "ci" / f"graphite-meter-{side}", BIN / f"{label}-{side}")


def cpu(pid: int) -> float:
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def memory(pid: int) -> dict[str, int]:
    """Peak and current resident set in KiB."""
    status = Path(f"/proc/{pid}/status").read_text()
    fields = dict(re.findall(r"^(\w+):\s+(\d+) kB", status, re.MULTILINE))
    return {"peakKiB": int(fields["VmHWM"]), "rssKiB": int(fields["VmRSS"])}


def pinned(binary: Path, directory: Path) -> Path:
    """`binary` behind `exec taskset`, which keeps the process ID the CPU accounting reads."""
    wrapper = directory / f"{binary.name}-pinned"
    wrapper.write_text(f"#!/bin/sh\nexec taskset -c {SERVER_CPUS} {binary} \"$@\"\n")
    wrapper.chmod(0o755)
    return wrapper


def rsa_identity(fixture: Fixture) -> tuple[Path, Path]:
    """An RSA-2048 leaf for localhost, signed by the fixture's CA."""
    directory = fixture.directory
    cert, key, request = directory / "rsa-cert.pem", directory / "rsa-key.pem", directory / "rsa.csr"
    if not cert.exists():
        for command in (
            ["req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost", "-keyout", key, "-out", request],
            ["x509", "-req", "-in", request, "-CA", fixture.ca, "-CAkey", directory / "ca.key", "-set_serial", "2",
             "-days", "10", "-out", cert, "-extfile", directory / "server.ext"],
        ):
            subprocess.run(["openssl", *map(str, command)], check=True, capture_output=True)
    return cert, key


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
    found = re.search(r"Loaded (?:down|up)\s+([\d.]+) ms\s+\S+(?: ms)?\s+([\d.]+) ms", output)
    return {"median": float(found[1]), "p95": float(found[2])} if found else {}


def replies(output: str) -> int | None:
    found = re.search(r"Idle latency: ([\d,]+) replies", output)
    return int(found[1].replace(",", "")) if found else None


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


def handshakes(server: Server) -> int:
    """Full TLS handshakes two `openssl s_time` clients complete in SECONDS against the HTTPS HTTP/1.1 listener."""
    command = ["taskset", "-c", CLIENT_CPUS, "openssl", "s_time", "-connect", f"127.0.0.1:{server.h1_tls}", "-new",
               "-time", str(SECONDS)]
    clients = [subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
               for _ in range(2)]
    counts = []
    for client in clients:
        output, _ = client.communicate(timeout=SECONDS * 4)
        found = re.search(r"(\d+) connections in [\d.]+s", output)
        counts.append(int(found[1]) if found else 0)
    return sum(counts)


def cell(fixture: Fixture, study: str, variant: str, case: tuple[str, str]) -> dict[str, object]:
    server_build, settings, client_build = VARIANTS[study][variant]
    settings = {"TOKIO_WORKER_THREADS": "2"} | settings
    if settings.pop("rsa", None):
        cert, key = rsa_identity(fixture)
        settings |= {"GM_TLS_CERT": str(cert), "GM_TLS_KEY": str(key)}
    server = fixture.server(pinned(BIN / f"{server_build}-server", fixture.directory), f"{study}-{variant}", settings)
    client = BIN / f"{client_build}-client"
    listener, second = case
    row: dict[str, object] = {"study": study, "variant": variant, "case": f"{listener} {second}"}
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
            peak = memory(pid)["peakKiB"]
            # The server returns freed pages two seconds after its last connection closes.
            time.sleep(3)
            row |= {"rtt": tails(prober.rtts), "probeError": prober.error, "peakKiB": peak,
                    "idleKiB": memory(pid)["rssKiB"]}
        elif study == "client":
            origin = server.origin("http3" if second == "webtransport" else "http1")
            latency = ["--latency-transport", second, "--latency-origin", origin]
            if listener == "idle":
                command = ["taskset", "-c", CLIENT_CPUS, str(client), "--report", "--insecure", "--url",
                           server.origin("http1"), "--stages", "latency", "--ping", "reply-driven",
                           "--latency-duration", f"{SECONDS}s", "--warmup", "500ms", *latency]
                output, used = timed(command)
                count = replies(output)
                row |= {"replies": count, "clientCpu": used,
                        "clientCpuPerReplyUs": round(used / count * 1e6, 3) if used and count else None}
            else:
                command = bulk(CLIENT_CPUS, client, server, listener, "download", "--loaded-latency=true",
                               "--loaded-ping", "reply-driven", *latency)
                output, used = timed(command)
                gbps = rate(output)
                row |= {"loaded": loaded(output), "clientCpu": used}
            seconds = cpu(pid) - before
        else:
            count = handshakes(server)
            seconds = cpu(pid) - before
            row |= {"handshakes": count,
                    "serverCpuPerHandshakeUs": round(seconds / count * 1e6, 2) if count else None}
        row |= {"gbps": gbps, "serverCpu": round(seconds, 3),
                "serverCpuPerGiB": round(seconds / (gbps * 1e9 / 8 * SECONDS / 2**30), 3) if gbps else None}
        (fixture.directory / f"{study}-{variant}-{listener}-{second}.log").write_text(output)
    return row


def measure(study: str) -> None:
    results = OUT / "results"
    results.mkdir(parents=True, exist_ok=True)
    fixture = Fixture(f"campaign-{study}-")
    cases = {"server": SERVER_CASES, "client": CLIENT_CASES, "handshake": HANDSHAKE_CASES}[study]
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
        "server": [("rtt", "p50"), ("rtt", "p99"), ("rtt", "p999"), ("rtt", "max"), ("gbps",), ("serverCpuPerGiB",),
                   ("peakKiB",), ("idleKiB",)],
        "client": [("clientCpuPerReplyUs",), ("replies",), ("loaded", "median"), ("loaded", "p95"), ("gbps",),
                   ("clientCpu",)],
        "handshake": [("serverCpuPerHandshakeUs",), ("handshakes",)],
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
