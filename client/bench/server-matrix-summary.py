"""Go against Rust per matrix cell from the rig's matrix.ndjson rows on stdin: medians, ranges and flags.

A metric reads WORSE when Rust is worse in a one-sided exact Mann-Whitney test (p <= 0.05) and its
median is more than 2 % worse than Go's, so a flag needs at least three valid runs on each side.
"""

import collections
import json
import re
import statistics
import sys

CELL = ("transport", "direction", "rtt", "loss", "rate", "count")
METRICS = {  # row key: (label, scale, higher is better)
    "throughputMbps": ("throughput Mbit/s", 1, True),
    "cpuSecPerGbit": ("server CPU s/Gbit", 1, False),
    "serverPeakRssBytes": ("server peak RSS MiB", 2**-20, False),
    "latencyP50Ms": ("loaded p50 ms", 1, False),
    "latencyP95Ms": ("loaded p95 ms", 1, False),
}
CLIENT_METRICS = ("throughputMbps", "latencyP50Ms", "latencyP95Ms")
COMPARISONS = [  # label, Go side, Rust side as (server, client), metrics
    *[(f"server, {client} client", ("go", client), ("rust", client), METRICS) for client in ("go", "rust", "browser")],
    *[(f"client, {server} server", (server, "go"), (server, "rust"), CLIENT_METRICS) for server in ("go", "rust")],
    ("Rust stack vs Go stack", ("go", "go"), ("rust", "rust"), METRICS),
]


def p_worse(go, rust, higher_better):
    """Exact probability, under exchangeable runs, that Rust's rank sum is at least this far toward worse."""
    pooled = sorted(go + rust, key=lambda value: -value if higher_better else value)
    doubled = {}  # value -> twice its mid-rank, 1 being the best run
    start = 0
    while start < len(pooled):
        end = start
        while end < len(pooled) and pooled[end] == pooled[start]:
            end += 1
        doubled[pooled[start]] = start + end + 1
        start = end
    ways = [collections.Counter({0: 1})] + [collections.Counter() for _ in rust]
    for value in pooled:
        for chosen in range(len(rust), 0, -1):
            for total, count in ways[chosen - 1].items():
                ways[chosen][total + doubled[value]] += count
    observed = sum(doubled[value] for value in rust)
    return sum(count for total, count in ways[-1].items() if total >= observed) / sum(ways[-1].values())


def spread(values):
    return f"{statistics.median(values):.4g} [{min(values):.4g}-{max(values):.4g}]"


def main():
    rows = [json.loads(line) for line in sys.stdin if line.strip()]
    groups = collections.defaultdict(list)
    for row in rows:
        groups[row["server"], row["client"], *(row[axis] for axis in CELL)].append(row)
    identities = {json.dumps(row["identity"], sort_keys=True): row["identity"] for row in rows}
    for identity in identities.values():
        binaries = ", ".join(f"{name} {binary['sha256'][:12]}" for name, binary in identity["binaries"].items())
        print(f"commit {identity['commit'][:12]}{' (dirty)' if identity['dirty'] else ''} · {identity['rustc'].splitlines()[0]} · "
              f"kernel {identity['kernel']} · governor {'/'.join(identity['governors'])} · {binaries}")
    valid = sum(row["valid"] for row in rows)
    print(f"{valid} valid runs, {len(rows) - valid} invalid; median [range] of valid runs, delta is Rust against Go\n")
    table = [["comparison", *CELL, "metric", "Go", "Rust", "delta", "runs", "verdict"]]
    flagged = []
    number = lambda value: float(value) if re.fullmatch(r"[\d.]+", value) else value
    cells = sorted({key[2:] for key in groups}, key=lambda cell: [(isinstance(v, str), v) for v in map(number, cell)])
    for label, go_side, rust_side, metrics in COMPARISONS:
        for cell in cells:
            go_rows, rust_rows = groups.get((*go_side, *cell), []), groups.get((*rust_side, *cell), [])
            if not go_rows or not rust_rows:
                continue
            medians = {}
            for key in metrics:
                name, scale, higher = METRICS[key]
                go = [row[key] * scale for row in go_rows if row["valid"] and row.get(key) is not None]
                rust = [row[key] * scale for row in rust_rows if row["valid"] and row.get(key) is not None]
                runs = f"{len(go)}/{len(go_rows)}:{len(rust)}/{len(rust_rows)}"
                if not go or not rust:
                    table.append([label, *cell, name, spread(go) if go else "-", spread(rust) if rust else "-", "", runs, "no data"])
                    continue
                medians[key] = statistics.median(go), statistics.median(rust)
                delta = medians[key][1] / medians[key][0] - 1 if medians[key][0] else 0.0
                worse = -delta if higher else delta
                verdict = "WORSE" if worse > 0.02 and p_worse(go, rust, higher) <= 0.05 else ""
                load = medians.get("throughputMbps")
                if key == "cpuSecPerGbit" and (not load or not load[0] or abs(load[1] / load[0] - 1) > 0.05):
                    verdict = "unequal load"  # CPU per Gbit only ranks runs that delivered the same load.
                table.append([label, *cell, name, spread(go), spread(rust), f"{delta:+.1%}", runs, verdict])
                if verdict == "WORSE":
                    flagged.append(table[-1])
    widths = [max(len(row[i]) for row in table) for i in range(len(table[0]))]
    for row in table:
        print("  ".join(value.ljust(width) for value, width in zip(row, widths)).rstrip())
    print(f"\n{len(flagged)} metric(s) with Rust worse beyond noise" + (":" if flagged else "."))
    for row in flagged:
        print("  " + "  ".join(value for value in row if value))


if __name__ == "__main__":
    main()
