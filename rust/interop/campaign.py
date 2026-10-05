"""Temporary regression campaign: the rewrite against PR #210 and Go across shaped, lossy links, on runners only.

    python3 rust/interop/campaign.py build
    python3 rust/interop/campaign.py measure {servers,clients} PROFILE
    python3 rust/interop/campaign.py report

`build` adds Go's server and client to perf.py's store, beside the rewrite's (candidate) and PR #210's (baseline)
static musl release builds. `measure` runs every transport and direction for the three variants of one side against
a fixed counterpart: `servers` with Go's client, `clients` with the rewrite's server. Each run starts a fresh server;
variants alternate their order and cells rotate across repetitions. A shaped profile (root only) moves the client
into a network namespace behind a veth pair with netem in both directions, as PR #210's campaign did, and keeps the
loaded latency the client measures by default. Every run lands in runs.jsonl beside its logs; the summary lists
first where the rewrite is worse than PR #210 or Go beyond the run spread. `report` merges every job's runs into one
summary. Only harness errors fail: a job in which no run succeeded, or a report without runs.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import statistics
import subprocess
import tempfile
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any, NamedTuple

from fixture import ROOT, Fixture, Server
from github_api import runner_path
from perf import CLIENT, SERVER, UNITS, binary, cpu, status, watch

Row = dict[str, Any]
Cell = tuple[str, str, str]  # listener, throughput transport, direction


class Profile(NamedTuple):
    name: str
    rtt: float  # round trip, ms
    variation: float  # delay variation each way, ms
    mbit: float  # rate each way
    loss: float  # % each way


# PR #210's profiles; each bottleneck queue holds about 100 ms of full-size packets.
PROFILES = {profile.name: profile for profile in (
    Profile("loopback", 0, 0, 0, 0), Profile("cable", 30, 0, 300, 0), Profile("far", 200, 0, 1000, 0),
    Profile("wifi", 20, 5, 150, 0.5), Profile("mobile", 70, 20, 30, 2))}
# Looked up, so file names hold these constants rather than arguments.
STUDIES = {"servers": "servers", "clients": "clients"}
# Store directory of each variant in perf.py's layout.
VARIANTS = {"rewrite": "candidate", "pr210": "baseline", "go": "go"}
NAMES = tuple(VARIANTS)
CELLS: tuple[Cell, ...] = tuple(
    (listener, transport, direction)
    for listener, transport in (("http1", "fetch-stream"), ("http2", "fetch-stream"), ("http3", "fetch-stream"),
                                ("http3", "webtransport"))
    for direction in ("download", "upload", "bidirectional"))
REPEATS, SECONDS, STREAMS = 5, 10, 4
# The server releases freed memory two seconds after its last connection closes.
IDLE = 3.0
# No further repetition round starts once another would likely end past this many seconds of measuring.
BUDGET = 70 * 60
MIB = 1 << 20
LOOPBACK, SERVER_ADDRESS, CLIENT_ADDRESS, NAMESPACE = "127.0.0.1", "10.77.0.1", "10.77.0.2", "gm-client"
# Linux's default congestion control, with TCP buffers that never limit a 1 Gbit/s, 300 ms path.
TCP = ["net.ipv4.tcp_congestion_control=cubic", "net.ipv4.tcp_rmem=4096 131072 134217728",
       "net.ipv4.tcp_wmem=4096 16384 134217728"]
RATE = re.compile(r"(?m)^[↓↑] (?:Download|Upload|Bidirectional)\s+([\d.]+) (bit|kbit|Mbit|Gbit|Tbit)/s")
LOADED = re.compile(r"(?m)^Loaded (?:down|up|bi-dir)\s{2,}(.+)$")
LATENCY = re.compile(r"(?:< )?([\d.]+) (ms|s)")
OUTCOME = re.compile(r"(?m)^Graphite Meter\s+(\w+)")
# TMPDIR; the report job downloads every measuring job's runs here.
RESULTS = Path(tempfile.gettempdir()) / "graphite-meter-campaign"


class Shaped(Server):
    """A server on the shaped link's server address."""

    def __init__(self, fixture: Fixture, binary: Path, name: str) -> None:
        super().__init__(fixture, binary, name, {})
        self.environment |= {key: value.replace(LOOPBACK, SERVER_ADDRESS) for key, value in self.environment.items()
                             if key.startswith("GM_") and key.endswith("_ADDR")}

    def origin(self, listener: str) -> str:
        return super().origin(listener).replace(LOOPBACK, SERVER_ADDRESS)


def build() -> None:
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GM_")}
    for package, source in ((SERVER, "./cmd/graphite-meter"), (CLIENT, "./cmd/graphite-meter-client")):
        destination = binary("go", package)
        destination.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["go", "build", "-trimpath", "-o", str(destination), source], cwd=ROOT / "go",
                       env=environment | {"CGO_ENABLED": "0"}, check=True)


def shape(profile: Profile) -> list[str]:
    """Server in this namespace, client in NAMESPACE, one veth pair shaped in both directions."""
    inside = ["ip", "netns", "exec", NAMESPACE]

    def sh(*command: str) -> None:
        subprocess.run(command, check=True)

    sh("ip", "netns", "add", NAMESPACE)
    sh("ip", "link", "add", "gm-server", "type", "veth", "peer", "name", "gm-peer", "netns", NAMESPACE)
    limit = max(1000, int(profile.mbit * 1e6 / 8 * ((profile.rtt / 2 + profile.variation) / 1000 + 0.1) / 1500))
    netem = ["delay", f"{profile.rtt / 2:g}ms", *([f"{profile.variation:g}ms"] if profile.variation else []),
             "rate", f"{profile.mbit:g}mbit", *(["loss", f"{profile.loss:g}%"] if profile.loss else [])]
    for prefix, interface, address in (([], "gm-server", SERVER_ADDRESS), (inside, "gm-peer", CLIENT_ADDRESS)):
        # netem must see single packets; batches would be delayed and dropped whole.
        sh(*prefix, "ethtool", "-K", interface, "tso", "off", "gso", "off", "gro", "off", "tx-udp-segmentation", "off")
        sh(*prefix, "ip", "addr", "add", f"{address}/24", "dev", interface)
        sh(*prefix, "ip", "link", "set", interface, "up")
        sh(*prefix, "tc", "qdisc", "replace", "dev", interface, "root", "netem", "limit", str(limit), *netem)
        sh(*prefix, "sysctl", "-qw", *TCP)
    sh(*inside, "ping", "-c", "5", "-i", "0.2", SERVER_ADDRESS)
    return inside


def trust(fixture: Fixture) -> None:
    """Reissues the fixture's leaf for the shaped server address too."""
    directory = fixture.directory
    extensions = directory / "server.ext"
    extensions.write_text(extensions.read_text().replace("subjectAltName=", f"subjectAltName=IP:{SERVER_ADDRESS},"))
    subprocess.run(["openssl", "x509", "-req", "-in", str(directory / "server.csr"), "-CA", str(fixture.ca),
                    "-CAkey", str(directory / "ca.key"), "-set_serial", "2", "-days", "10", "-out", str(fixture.cert),
                    "-extfile", str(extensions)], check=True, capture_output=True)


def command(client: Path, server: Server, cell: Cell, shaped: bool) -> list[str]:
    """A report run of one stage; shaped runs keep the default warmup and loaded latency, as users run."""
    listener, transport, direction = cell
    return [str(client), "--report", "--url", server.origin("http1"), "--throughput-origin", server.origin(listener),
            "--throughput-protocol", listener, "--throughput-transport", transport, "--stages", direction,
            f"--{direction}-duration={SECONDS}s", f"--streams={STREAMS}",
            *([] if shaped else ["--warmup=500ms", "--loaded-latency=false"])]


def milliseconds(text: str) -> float | None:
    match = LATENCY.fullmatch(text)
    return float(match[1]) * (1e3 if match[2] == "s" else 1) if match else None


def parse(output: str, code: int) -> Row:
    """Rate over both directions, loaded latency, outcome and, for a failed run, its reason."""
    total = sum(float(value) * UNITS[unit] for value, unit in RATE.findall(output))
    gbps = total if total and code == 0 else None
    loaded = None
    if found := LOADED.search(output):
        columns = re.split(r"\s{2,}", found[1].strip())
        loaded = {"medianMs": milliseconds(columns[0]), "p95Ms": milliseconds(columns[2]) if len(columns) > 2 else None,
                  "timeouts": columns[4] if len(columns) > 4 else None}
    outcome = OUTCOME.search(output)
    reason = None
    if gbps is None:
        lines = [line.strip() for line in output.splitlines() if line.strip()]
        reason = "killed after 60 s" if code < 0 else (lines[-1][:200] if lines else f"exit {code}")
        reason = f"{outcome[1]}: {reason}" if outcome else reason
    return {"exit": code, "outcome": outcome[1] if outcome else None, "gbps": gbps, "loaded": loaded,
            "reason": reason}


def run(fixture: Fixture, study: str, prefix: list[str], variant: str, cell: Cell, repeat: int) -> Row:
    """One client run against a fresh server, measuring both sides."""
    listener, transport, direction = cell
    name = f"{variant}-{listener}-{transport}-{direction}-{repeat}"
    varied = VARIANTS[variant]
    server_binary = binary(varied if study == "servers" else "candidate", SERVER)
    client_binary = binary(varied if study == "clients" else "go", CLIENT)
    environment = fixture.environment | {"SSL_CERT_FILE": str(fixture.ca)}
    log = fixture.directory / f"client-{name}.log"
    server = Shaped(fixture, server_binary, name) if prefix else Server(fixture, server_binary, name, {})
    row: Row = {"study": study, "variant": variant, "listener": listener, "transport": transport,
                "direction": direction, "repeat": repeat, "server": {}, "client": {}}
    started, code, failure = time.monotonic(), 0, None
    try:
        with server:
            assert server.process is not None
            pid = server.process.pid
            before = cpu(pid)
            code, used, peak = watch([*prefix, *command(client_binary, server, cell, bool(prefix))], environment, log)
            row["client"] = {"cpuSeconds": round(used, 3), "peak": peak}
            row["server"] = {"cpuSeconds": round(cpu(pid) - before, 3), "peak": status(pid, "VmHWM"),
                             "after": status(pid, "VmRSS")}
            if study == "servers":
                time.sleep(IDLE)
                row["server"]["idle"] = status(pid, "VmRSS")
    except (RuntimeError, TimeoutError, KeyError) as error:
        # A missing status line means the server exited during the run.
        failure = "server: " + (str(error).splitlines() or ["exited during the run"])[0][:200]
        if server.process is not None and server.process.poll() is None:
            server.process.kill()
            server.process.wait()
    row |= parse(log.read_text() if log.exists() else "", code)
    if failure:
        row |= {"gbps": None, "reason": failure}
    row["seconds"] = round(time.monotonic() - started, 1)
    return row


def measure(study: str, profile: Profile) -> None:
    prefix = shape(profile) if profile.mbit else []
    fixture = Fixture(f"campaign-{study}-{profile.name}-")
    if prefix:
        trust(fixture)
    rows: list[Row] = []
    began, rounds = time.monotonic(), [0.0]
    with (fixture.directory / "runs.jsonl").open("w") as sink:
        for repeat in range(REPEATS):
            if time.monotonic() - began + max(rounds) > BUDGET:
                print(f"Stopping after {repeat} repetitions: another would pass the {BUDGET // 60} min budget")
                break
            start = time.monotonic()
            for index in range(len(CELLS)):
                position = (index + repeat) % len(CELLS)
                turn = (position + repeat) % len(NAMES)
                for variant in NAMES[turn:] + NAMES[:turn]:
                    row = {"profile": profile.name} | run(fixture, study, prefix, variant, CELLS[position], repeat)
                    rows.append(row)
                    sink.write(json.dumps(row) + "\n")
                    sink.flush()
                    print(json.dumps(row), flush=True)
            rounds.append(time.monotonic() - start)
    text = report(rows)
    (fixture.directory / "summary.md").write_text(text)
    print(text, flush=True)
    if not any(row["gbps"] for row in rows):
        raise SystemExit("no run succeeded; the harness is broken")


class Metric(NamedTuple):
    label: str
    value: Callable[[Row], float | None]
    higher_is_better: bool = False


def subject(row: Row) -> dict[str, Any]:
    return row["server"] if row["study"] == "servers" else row["client"]


def nanoseconds_per_byte(row: Row) -> float | None:
    used = subject(row).get("cpuSeconds")
    return used * 8 / (row["gbps"] * SECONDS) if used is not None else None


def mebibytes(key: str) -> Callable[[Row], float | None]:
    return lambda row: subject(row)[key] / MIB if key in subject(row) else None


# In the order the summary ranks findings: throughput, latency, CPU, then memory.
METRICS = (
    Metric("Mbit/s", lambda row: row["gbps"] * 1e3, higher_is_better=True),
    Metric("loaded median ms", lambda row: (row["loaded"] or {}).get("medianMs")),
    Metric("loaded p95 ms", lambda row: (row["loaded"] or {}).get("p95Ms")),
    Metric("CPU ns/B", nanoseconds_per_byte),
    Metric("peak MiB", mebibytes("peak")),
    Metric("idle MiB", mebibytes("idle")),
)


def figure(value: float) -> str:
    return f"{value:.0f}" if value >= 100 else f"{value:.1f}" if value >= 10 else f"{value:.2f}"


def spread(values: list[float]) -> str:
    return f"{figure(statistics.median(values))} ({figure(min(values))}–{figure(max(values))})" if values else "–"


def report(rows: list[Row]) -> str:
    """Markdown: failures and the rewrite's losses first, then every median with its spread."""
    groups: dict[tuple[str, str, str], dict[str, list[Row]]] = {}
    for row in rows:
        key = (row["study"], row["profile"], f"{row['listener']} {row['transport']} {row['direction']}")
        groups.setdefault(key, {}).setdefault(row["variant"], []).append(row)
    beyond: list[tuple[int, float, str]] = []
    within: list[tuple[int, float, str]] = []
    failures: list[str] = []
    tables: dict[tuple[str, str], list[str]] = {}
    for (study, profile, cell), variants in groups.items():
        where = f"{study} · {profile} · {cell}"
        values = {variant: [[value for row in runs if row["gbps"] and (value := metric.value(row)) is not None]
                            for metric in METRICS] for variant, runs in variants.items()}
        for variant, runs in variants.items():
            reasons: dict[str, int] = {}
            for row in runs:
                if not row["gbps"]:
                    reasons[row["reason"]] = reasons.get(row["reason"], 0) + 1
            failures += [f"- {where} · {variant}: {count}× {reason}" for reason, count in reasons.items()]
            ok = sum(1 for row in runs if row["gbps"])
            tables.setdefault((study, profile), []).append(
                f"| {cell} | {variant} | {ok}/{len(runs)} | " + " | ".join(map(spread, values[variant])) + " |")
        rewrite, empty = variants.get("rewrite", []), [[] for _ in METRICS]
        for other in ("pr210", "go"):
            runs = variants.get(other, [])
            lost, others = sum(not row["gbps"] for row in rewrite), sum(not row["gbps"] for row in runs)
            if lost > others:
                beyond.append((-1, lost - others, f"- {where}: rewrite failed {lost}/{len(rewrite)}, {other} "
                                                  f"{others}/{len(runs)}"))
            for rank, (metric, mine, theirs) in enumerate(zip(METRICS, values.get("rewrite", empty),
                                                               values.get(other, empty), strict=True)):
                if not mine or not theirs or not statistics.median(theirs):
                    continue
                change = statistics.median(mine) / statistics.median(theirs) - 1
                if (change < 0) != metric.higher_is_better or change == 0:
                    continue
                apart = max(mine) < min(theirs) if metric.higher_is_better else min(mine) > max(theirs)
                line = (f"- {where} · {metric.label}: rewrite {spread(mine)} against {other} {spread(theirs)} "
                        f"({change:+.0%})")
                if apart:
                    beyond.append((rank, abs(change), line))
                elif abs(change) >= 0.05:
                    within.append((rank, abs(change), line))
    labels = [metric.label for metric in METRICS]
    lines = ["### Regression campaign: the rewrite against PR #210 and Go", "",
             f"Medians (min–max) of up to {REPEATS} runs per variant; {STREAMS} streams, {SECONDS} s stages; servers "
             "vary with Go's client, clients with the rewrite's server; CPU and memory are the varied side's.", "",
             "**Where the rewrite is worse beyond the run spread:**",
             *([line for *_, line in sorted(beyond, key=lambda item: (item[0], -item[1]))] or ["- nowhere"]), "",
             "**Worse by 5 % or more in the median, within the spread:**",
             *([line for *_, line in sorted(within, key=lambda item: (item[0], -item[1]))] or ["- nowhere"]), "",
             "**Failed runs:**", *(failures or ["- none"]), ""]
    for (study, profile), table in tables.items():
        lines += [f"#### {study} · {profile}", "", "| Cell | Variant | ok | " + " | ".join(labels) + " |",
                  "|---|---|---|" + "---|" * len(labels), *table, ""]
    return "\n".join(lines)


def merge() -> None:
    rows = [json.loads(line) for path in sorted(RESULTS.rglob("runs.jsonl"))
            for line in path.read_text().splitlines() if line]
    if not rows:
        raise SystemExit(f"no runs under {RESULTS}")
    rows.sort(key=lambda row: (row["study"], list(PROFILES).index(row["profile"])))
    (RESULTS / "campaign-runs.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows))
    text = report(rows)
    (RESULTS / "summary.md").write_text(text)
    print(text, flush=True)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with runner_path("GITHUB_STEP_SUMMARY").open("a") as output:
            output.write(text)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("build")
    measuring = commands.add_parser("measure")
    measuring.add_argument("study", choices=STUDIES)
    measuring.add_argument("profile", choices=PROFILES)
    commands.add_parser("report")
    args = parser.parse_args()
    if args.command == "build":
        build()
    elif args.command == "measure":
        measure(STUDIES[args.study], PROFILES[args.profile])
    else:
        merge()


if __name__ == "__main__":
    main()
