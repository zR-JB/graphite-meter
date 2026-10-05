"""Temporary perf smoke: this branch's server against PR #210's on one runner, never on a laptop.

    python3 rust/interop/perf.py build CHECKOUT DESTINATION
    python3 rust/interop/perf.py measure BASELINE CANDIDATE

`build` copies CHECKOUT's static musl release server into DESTINATION. `measure` drives both servers with the Go
native client, a fresh server per run, in alternating order: throughput, server CPU per byte, peak RSS, RSS right
after the transfer and after the idle release. It keeps every run in runs.jsonl, prints medians with their spread
and fails only on gross regressions.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import statistics
import subprocess
import time
from pathlib import Path
from typing import Any

from fixture import Fixture, build_server, native

CELLS = [(listener, direction) for listener in ("http1", "http2", "http3") for direction in ("download", "upload")]
VARIANTS = ("baseline", "candidate")
REPEATS, SECONDS, STREAMS = 5, 4, 4
# The server releases freed memory two seconds after its last connection closes.
IDLE = 3.0
MIB = 1 << 20
RATE = re.compile(r"(?:Download|Upload)\s+([\d.]+) (bit|kbit|Mbit|Gbit|Tbit)/s")
UNITS = {"bit": 1e-9, "kbit": 1e-6, "Mbit": 1e-3, "Gbit": 1.0, "Tbit": 1e3}


def status(pid: int, key: str) -> int:
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith(key + ":"):
            return int(line.split()[1]) * 1024
    raise KeyError(key)


def cpu(pid: int) -> float:
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def run(fixture: Fixture, binary: Path, client: Path, variant: str, cell: tuple[str, str], repeat: int) -> dict[str, Any]:
    listener, direction = cell
    name = f"{variant}-{listener}-{direction}-{repeat}"
    environment = fixture.environment | {"SSL_CERT_FILE": str(fixture.ca)}
    with fixture.server(binary, name) as server:
        assert server.process is not None
        pid = server.process.pid
        start, before = status(pid, "VmRSS"), cpu(pid)
        command = native(client, server, listener, "fetch-stream", direction, SECONDS, f"--streams={STREAMS}",
                         "--loaded-latency=false")
        result = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=60)
        used = cpu(pid) - before
        after, peak = status(pid, "VmRSS"), status(pid, "VmHWM")
        time.sleep(IDLE)
        idle = status(pid, "VmRSS")
    output = result.stdout + result.stderr
    (fixture.directory / f"client-{name}.log").write_text(output)
    rate = RATE.search(output)
    gbps = float(rate[1]) * UNITS[rate[2]] if rate and result.returncode == 0 else None
    return {"variant": variant, "listener": listener, "direction": direction, "repeat": repeat,
            "exit": result.returncode, "gbps": gbps, "cpuSeconds": round(used, 3),
            "rss": {"start": start, "peak": peak, "after": after, "idle": idle}}


def median(values: list[float]) -> float | None:
    return statistics.median(values) if values else None


def summarize(rows: list[dict[str, Any]]) -> dict[str, dict[str, dict[str, Any]]]:
    table: dict[str, dict[str, dict[str, Any]]] = {}
    for listener, direction in CELLS:
        for variant in VARIANTS:
            runs = [row for row in rows if (row["listener"], row["direction"], row["variant"])
                    == (listener, direction, variant)]
            ok = [row for row in runs if row["gbps"]]
            rates = [row["gbps"] for row in ok]
            # Bytes the timed stage moved; warmup and discovery cost both variants alike.
            cost = [row["cpuSeconds"] / (row["gbps"] * 1e9 / 8 * SECONDS) * 1e9 for row in ok]
            idle = [row["rss"]["idle"] / MIB for row in ok]
            table.setdefault(f"{listener} {direction}", {})[variant] = {
                "ok": f"{len(ok)}/{len(runs)}", "failed": len(runs) - len(ok),
                "gbps": median(rates), "gbpsRange": [min(rates), max(rates)] if rates else None,
                "cpuNsPerByte": median(cost), "cpuRange": [min(cost), max(cost)] if cost else None,
                "peakMiB": median([row["rss"]["peak"] / MIB for row in ok]),
                "afterMiB": median([row["rss"]["after"] / MIB for row in ok]),
                "idleMiB": median(idle), "idleRange": [min(idle), max(idle)] if idle else None,
            }
    return table


def regressions(table: dict[str, dict[str, dict[str, Any]]]) -> list[str]:
    """Gross regressions only: failed runs, a quarter less HTTP/1 or HTTP/2 throughput, a third more HTTP/3 CPU per
    byte, or half again the idle RSS and 8 MiB more."""
    found = []
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
        if cand["idleMiB"] > 1.5 * base["idleMiB"] and cand["idleMiB"] - base["idleMiB"] > 8:
            found.append(f"{cell}: idle RSS {cand['idleMiB']:.1f} MiB against {base['idleMiB']:.1f}")
    return found


def report(table: dict[str, dict[str, dict[str, Any]]], found: list[str]) -> str:
    """Markdown: where the candidate is worse first, then every median with its spread."""
    def fmt(value: float | None, spread: list[float] | None = None, digits: int = 2) -> str:
        if value is None:
            return "–"
        text = f"{value:.{digits}f}"
        return f"{text} ({spread[0]:.{digits}f}–{spread[1]:.{digits}f})" if spread else text

    worse = []
    for cell, variants in table.items():
        base, cand = variants["baseline"], variants["candidate"]
        for key, label, higher_is_worse in (("gbps", "Gbit/s", False), ("cpuNsPerByte", "CPU ns/B", True),
                                            ("idleMiB", "idle MiB", True)):
            if base[key] and cand[key]:
                change = cand[key] / base[key] - 1
                if (change > 0) == higher_is_worse and abs(change) >= 0.02:
                    worse.append((abs(change), f"- {cell} {label}: {cand[key]:.2f} against {base[key]:.2f} "
                                               f"({change:+.0%})"))
    lines = ["### Rust perf smoke: this branch against PR #210 (static musl release, Go client, same runner)", ""]
    lines += ["**Gross regressions:** " + ("; ".join(found) if found else "none"), ""]
    lines += ["**Where the candidate is worse (2 % or more):**", *([line for _, line in sorted(worse, reverse=True)]
                                                                 or ["- nowhere"]), ""]
    lines += [f"Medians of {REPEATS} runs, {STREAMS} streams, {SECONDS} s each, min–max in brackets.", "",
              "| Cell | Server | ok | Gbit/s | CPU ns/B | peak MiB | after MiB | idle MiB |",
              "|---|---|---|---|---|---|---|---|"]
    for cell, variants in table.items():
        for variant, row in variants.items():
            lines.append(f"| {cell} | {variant} | {row['ok']} | {fmt(row['gbps'], row['gbpsRange'])} | "
                         f"{fmt(row['cpuNsPerByte'], row['cpuRange'])} | {fmt(row['peakMiB'], digits=1)} | "
                         f"{fmt(row['afterMiB'], digits=1)} | {fmt(row['idleMiB'], row['idleRange'], 1)} |")
    return "\n".join(lines) + "\n"


def measure(baseline: Path, candidate: Path) -> None:
    fixture = Fixture("perf-")
    client = fixture.go_build("client", "./cmd/graphite-meter-client")
    binaries = {"baseline": baseline.resolve(), "candidate": candidate.resolve()}
    rows = []
    with (fixture.directory / "runs.jsonl").open("w") as sink:
        for repeat in range(REPEATS):
            for index in range(len(CELLS)):
                cell = CELLS[(index + repeat) % len(CELLS)]
                order = VARIANTS if (index + repeat) % 2 == 0 else VARIANTS[::-1]
                for variant in order:
                    row = run(fixture, binaries[variant], client, variant, cell, repeat)
                    rows.append(row)
                    sink.write(json.dumps(row) + "\n")
                    print(json.dumps(row), flush=True)
    table = summarize(rows)
    (fixture.directory / "summary.json").write_text(json.dumps(table, indent=1) + "\n")
    found = regressions(table)
    text = report(table, found)
    print(text, flush=True)
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a") as output:
            output.write(text)
    if found:
        raise SystemExit("gross regression against PR #210: " + "; ".join(found))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("build")
    build.add_argument("checkout", type=Path)
    build.add_argument("destination", type=Path)
    compare = commands.add_parser("measure")
    compare.add_argument("baseline", type=Path)
    compare.add_argument("candidate", type=Path)
    args = parser.parse_args()
    if args.command == "build":
        args.destination.mkdir(parents=True, exist_ok=True)
        shutil.copy2(build_server(args.checkout.resolve(), "release"), args.destination)
    else:
        measure(args.baseline, args.candidate)


if __name__ == "__main__":
    main()
