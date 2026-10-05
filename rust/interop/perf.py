"""Temporary perf smoke: this branch's server and client against PR #210's on one runner, never on a laptop.

    python3 rust/interop/perf.py baseline COMMIT
    python3 rust/interop/perf.py candidate
    python3 rust/interop/perf.py servers
    python3 rust/interop/perf.py clients

`baseline` checks out COMMIT and `candidate` uses this checkout; each copies its static musl release server and
client into STORE. `servers` drives both servers with the Go native client: throughput, server CPU per byte, peak RSS,
RSS right after the transfer and after the idle release. `clients` drives both Rust clients against this branch's
server: throughput, client CPU per byte and peak RSS. Both run a fresh server per run in alternating order, keep every
run in runs.jsonl, print medians with their spread and fail only on gross regressions.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import re
import shutil
import statistics
import subprocess
import tempfile
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any, NamedTuple

from fixture import ROOT, Fixture, build_static, native
from github_api import runner_path

Row = dict[str, Any]
Cell = tuple[str, str, str]  # listener, throughput transport, direction
Table = dict[str, dict[str, dict[str, Any]]]

VARIANTS = ("baseline", "candidate")
SERVER, CLIENT = "graphite-meter-server", "graphite-meter-client"
DIRECTIONS = ("download", "upload")
REPEATS, SECONDS, STREAMS = 5, 4, 4
# The server releases freed memory two seconds after its last connection closes.
IDLE = 3.0
MIB = 1 << 20
RATE = re.compile(r"(?:Download|Upload)\s+([\d.]+) (bit|kbit|Mbit|Gbit|Tbit)/s")
UNITS = {"bit": 1e-9, "kbit": 1e-6, "Mbit": 1e-3, "Gbit": 1.0, "Tbit": 1e3}
# TMPDIR; the job sets it to the runner's job directory and caches the baseline from there.
STORE = Path(tempfile.gettempdir()) / "graphite-meter-perf"
COMMIT = re.compile(r"[0-9a-f]{40}")


class Metric(NamedTuple):
    label: str
    digits: int
    value: Callable[[Row], float]
    lower_is_worse: bool = False


def rss(key: str) -> Callable[[Row], float]:
    return lambda row: row["rss"][key] / MIB


METRICS = {
    "gbps": Metric("Gbit/s", 2, lambda row: row["gbps"], lower_is_worse=True),
    # Bytes the timed stage moved; warmup and discovery cost both variants alike.
    "cpuNsPerByte": Metric("CPU ns/B", 2, lambda row: row["cpuSeconds"] * 8 / (row["gbps"] * SECONDS)),
    "peakMiB": Metric("peak MiB", 1, rss("peak")),
    "afterMiB": Metric("after MiB", 1, rss("after")),
    "idleMiB": Metric("idle MiB", 1, rss("idle")),
}


class Comparison(NamedTuple):
    subject: str
    heading: str
    cells: tuple[Cell, ...]
    metrics: tuple[str, ...]
    memory: str  # the RSS metric the gross-regression check guards


SERVERS = Comparison(
    "server", "this branch's server against PR #210's (static musl release, Go client, same runner)",
    tuple((listener, "fetch-stream", direction) for listener in ("http1", "http2", "http3") for direction in DIRECTIONS),
    ("gbps", "cpuNsPerByte", "peakMiB", "afterMiB", "idleMiB"), "idleMiB")
# This branch's server is the fastest on every transport, so each client is its run's bottleneck.
CLIENTS = Comparison(
    "client", "this branch's client against PR #210's (static musl release, this branch's server, same runner)",
    tuple((listener, transport, direction)
          for listener, transport in (("http1", "fetch-stream"), ("http2", "fetch-stream"),
                                      ("http3", "fetch-stream"), ("http3", "webtransport"))
          for direction in DIRECTIONS),
    ("gbps", "cpuNsPerByte", "peakMiB"), "peakMiB")


def binary(variant: str, package: str) -> Path:
    return STORE / variant / package


def build(variant: str, checkout: Path) -> None:
    for package in (SERVER, CLIENT):
        destination = binary(variant, package)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(build_static(checkout, "release", package), destination)


def baseline(commit: str) -> None:
    if not COMMIT.fullmatch(commit):
        raise SystemExit(f"not a full commit hash: {commit!r}")
    checkout = STORE / "baseline-checkout"
    subprocess.run(["git", "fetch", "--no-tags", "--depth=1", "origin", commit], cwd=ROOT, check=True)
    subprocess.run(["git", "worktree", "add", "--detach", str(checkout), commit], cwd=ROOT, check=True)
    build("baseline", checkout)


def status(pid: int, key: str) -> int:
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith(key + ":"):
            return int(line.split()[1]) * 1024
    raise KeyError(key)


def cpu(pid: int) -> float:
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def label(cell: Cell) -> str:
    return " ".join(cell)


def record(variant: str, cell: Cell, repeat: int, code: int, output: str, used: float,
           memory: dict[str, int]) -> Row:
    listener, transport, direction = cell
    rate = RATE.search(output)
    gbps = float(rate[1]) * UNITS[rate[2]] if rate and code == 0 else None
    return {"variant": variant, "listener": listener, "transport": transport, "direction": direction,
            "repeat": repeat, "exit": code, "gbps": gbps, "cpuSeconds": round(used, 3), "rss": memory}


def run_server(fixture: Fixture, client: Path, variant: str, cell: Cell, repeat: int) -> Row:
    """One Go client run against `variant`'s server, measuring the server."""
    listener, transport, direction = cell
    name = f"{variant}-{listener}-{transport}-{direction}-{repeat}"
    environment = fixture.environment | {"SSL_CERT_FILE": str(fixture.ca)}
    with fixture.server(binary(variant, SERVER), name) as server:
        assert server.process is not None
        pid = server.process.pid
        start, before = status(pid, "VmRSS"), cpu(pid)
        command = native(client, server, listener, transport, direction, SECONDS, f"--streams={STREAMS}",
                         "--loaded-latency=false")
        result = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=60)
        used = cpu(pid) - before
        after, peak = status(pid, "VmRSS"), status(pid, "VmHWM")
        time.sleep(IDLE)
        idle = status(pid, "VmRSS")
    output = result.stdout + result.stderr
    (fixture.directory / f"client-{name}.log").write_text(output)
    return record(variant, cell, repeat, result.returncode, output, used,
                  {"start": start, "peak": peak, "after": after, "idle": idle})


def watch(command: list[str], environment: dict[str, str], log: Path) -> tuple[int, float, int]:
    """Runs a client to its end: exit status, CPU seconds and peak RSS from /proc."""
    peak, deadline = 0, time.monotonic() + 60
    with log.open("w") as output, subprocess.Popen(command, env=environment, stdout=output,
                                                   stderr=subprocess.STDOUT) as process:
        while not os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT):
            # An exiting process loses its memory lines before the loop sees it exit.
            with contextlib.suppress(KeyError):
                peak = max(peak, status(process.pid, "VmHWM"))
            if time.monotonic() > deadline:
                process.kill()
            time.sleep(0.02)
        # Exited but not yet reaped: its CPU times include every thread's.
        used = cpu(process.pid)
    return process.returncode, used, peak


def run_client(fixture: Fixture, variant: str, cell: Cell, repeat: int) -> Row:
    """One run of `variant`'s client against this branch's server, measuring the client."""
    listener, transport, direction = cell
    name = f"{variant}-{listener}-{transport}-{direction}-{repeat}"
    environment = fixture.environment | {"SSL_CERT_FILE": str(fixture.ca)}
    log = fixture.directory / f"client-{name}.log"
    with fixture.server(binary("candidate", SERVER), name) as server:
        command = native(binary(variant, CLIENT), server, listener, transport, direction, SECONDS,
                         f"--streams={STREAMS}", "--loaded-latency=false")
        code, used, peak = watch(command, environment, log)
    return record(variant, cell, repeat, code, log.read_text(), used, {"peak": peak})


def median(values: list[float]) -> float | None:
    return statistics.median(values) if values else None


def summarize(rows: list[Row], comparison: Comparison) -> Table:
    table: Table = {}
    for cell in comparison.cells:
        for variant in VARIANTS:
            runs = [row for row in rows
                    if (row["listener"], row["transport"], row["direction"], row["variant"]) == (*cell, variant)]
            ok = [row for row in runs if row["gbps"]]
            entry: dict[str, Any] = {"ok": f"{len(ok)}/{len(runs)}", "failed": len(runs) - len(ok)}
            for key in comparison.metrics:
                values = [METRICS[key].value(row) for row in ok]
                entry[key] = median(values)
                entry[key + "Range"] = [min(values), max(values)] if values else None
            table.setdefault(label(cell), {})[variant] = entry
    return table


def regressions(table: Table, comparison: Comparison) -> list[str]:
    """Gross regressions only: failed runs, a quarter less HTTP/1 or HTTP/2 throughput, a third more HTTP/3 CPU per
    byte (fetch streams or WebTransport), or half again the guarded RSS and 8 MiB more."""
    found = []
    memory = comparison.memory
    for cell, variants in table.items():
        base, cand = variants["baseline"], variants["candidate"]
        if cand["failed"]:
            found.append(f"{cell}: {cand['failed']} candidate runs failed")
        if base["gbps"] is None or cand["gbps"] is None:
            continue
        if not cell.startswith("http3") and cand["gbps"] < 0.75 * base["gbps"]:
            found.append(f"{cell}: {cand['gbps']:.2f} Gbit/s against {base['gbps']:.2f}")
        if cell.startswith("http3") and cand["cpuNsPerByte"] > 1.33 * base["cpuNsPerByte"]:
            found.append(f"{cell}: {cand['cpuNsPerByte']:.2f} CPU ns/B against {base['cpuNsPerByte']:.2f}")
        if cand[memory] > 1.5 * base[memory] and cand[memory] - base[memory] > 8:
            found.append(f"{cell}: {METRICS[memory].label} {cand[memory]:.1f} against {base[memory]:.1f}")
    return found


def report(table: Table, found: list[str], comparison: Comparison) -> str:
    """Markdown: where the candidate is worse first, then every median with its spread."""
    def fmt(value: float | None, spread: list[float] | None, digits: int) -> str:
        if value is None:
            return "–"
        text = f"{value:.{digits}f}"
        return f"{text} ({spread[0]:.{digits}f}–{spread[1]:.{digits}f})" if spread else text

    worse, failed = [], []
    for cell, variants in table.items():
        base, cand = variants["baseline"], variants["candidate"]
        if base["failed"]:
            failed.append(f"{cell}: {base['failed']}")
        for key in comparison.metrics:
            metric = METRICS[key]
            if base[key] and cand[key]:
                change = cand[key] / base[key] - 1
                if (change < 0) == metric.lower_is_worse and abs(change) >= 0.02:
                    worse.append((abs(change), f"- {cell} {metric.label}: {cand[key]:.2f} against {base[key]:.2f} "
                                               f"({change:+.0%})"))
    lines = [f"### Rust perf smoke: {comparison.heading}", ""]
    lines += ["**Gross regressions:** " + ("; ".join(found) if found else "none"), ""]
    if failed:
        lines += ["**Failed baseline runs:** " + "; ".join(failed), ""]
    lines += ["**Where the candidate is worse (2 % or more):**", *([line for _, line in sorted(worse, reverse=True)]
                                                                 or ["- nowhere"]), ""]
    labels = [METRICS[key].label for key in comparison.metrics]
    lines += [f"Medians of {REPEATS} runs, {STREAMS} streams, {SECONDS} s each, min–max in brackets.", "",
              f"| Cell | {comparison.subject.capitalize()} | ok | " + " | ".join(labels) + " |",
              "|---|---|---|" + "---|" * len(labels)]
    for cell, variants in table.items():
        for variant, row in variants.items():
            figures = [fmt(row[key], row[key + "Range"], METRICS[key].digits) for key in comparison.metrics]
            lines.append(f"| {cell} | {variant} | {row['ok']} | " + " | ".join(figures) + " |")
    return "\n".join(lines) + "\n"


def compare(comparison: Comparison, fixture: Fixture, run: Callable[[str, Cell, int], Row]) -> None:
    """Runs every cell for both variants in alternating order, then reports and fails on gross regressions."""
    cells, rows = comparison.cells, []
    with (fixture.directory / "runs.jsonl").open("w") as sink:
        for repeat in range(REPEATS):
            for index in range(len(cells)):
                cell = cells[(index + repeat) % len(cells)]
                order = VARIANTS if (index + repeat) % 2 == 0 else VARIANTS[::-1]
                for variant in order:
                    row = run(variant, cell, repeat)
                    rows.append(row)
                    sink.write(json.dumps(row) + "\n")
                    print(json.dumps(row), flush=True)
    table = summarize(rows, comparison)
    (fixture.directory / "summary.json").write_text(json.dumps(table, indent=1) + "\n")
    found = regressions(table, comparison)
    text = report(table, found, comparison)
    print(text, flush=True)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with runner_path("GITHUB_STEP_SUMMARY").open("a") as output:
            output.write(text)
    if found:
        raise SystemExit(f"gross {comparison.subject} regression against PR #210: " + "; ".join(found))


def servers() -> None:
    fixture = Fixture("perf-servers-")
    client = fixture.go_build("client", "./cmd/graphite-meter-client")
    compare(SERVERS, fixture, lambda variant, cell, repeat: run_server(fixture, client, variant, cell, repeat))


def clients() -> None:
    fixture = Fixture("perf-clients-")
    compare(CLIENTS, fixture, lambda variant, cell, repeat: run_client(fixture, variant, cell, repeat))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("baseline").add_argument("commit")
    for command in ("candidate", "servers", "clients"):
        commands.add_parser(command)
    args = parser.parse_args()
    if args.command == "baseline":
        baseline(args.commit)
    elif args.command == "candidate":
        build("candidate", ROOT)
    elif args.command == "servers":
        servers()
    else:
        clients()


if __name__ == "__main__":
    main()
