"""Temporary campaign for the Rust review pull request, seventh run: per-packet loss and build profiles.

build-loss: ci-profile musl binaries whose noq sends one datagram per system call, at ACK thresholds 9 (the head), off
    and 31, and Go's server and TUI.
measure loss05|loss01: HTTP/3 over loopback at MTU 1500 with offloads off, so netem drops single packets (40 ms;
    0.5% at 50 Mbit/s, 0.1% at 100 Mbit/s), against HTTP/2 over Linux TCP and Go's HTTP/3 without GSO.
build-profiles: musl binaries of the head with the ci profile, the release profile (fat LTO) and thin LTO.
measure cpu|client: server and TUI CPU per GiB of each profile over loopback.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import statistics
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path

from rust.interop.fixture import Fixture, Server, musl_target

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "rust/target/campaign"
BIN = OUT / "bin"
RESULTS = OUT / "results"
SECONDS = 15
REPEATS = 6
SERVER_CPUS, CLIENT_CPUS = "0,1", "2,3"
QUIC = "rust/net/src/quic.rs"
THRESHOLD = "const ACK_ELICITING_THRESHOLD: u32 = 9;"
ASK = "    transport.ack_frequency_config(Some(ack_frequency()));\n"
NOQ_PIN = 'noq = { git = "https://github.com/zR-JB/noq", rev = "a459ad8525063ddae3bc9fa1385826d6848a0ce3" }'
Edit = tuple[str, str, str, int]
LOSS_BUILDS: dict[str, list[Edit]] = {
    "head": [],
    "noack": [(QUIC, ASK, "", 2)],
    "ack31": [(QUIC, THRESHOLD, THRESHOLD.replace("9;", "31;"), 1)],
}
# Each profile: the Cargo profile and its overrides.
PROFILES: dict[str, tuple[str, dict[str, str]]] = {
    "ci": ("ci", {}),
    "release": ("release", {}),
    "thin": ("release", {"CARGO_PROFILE_RELEASE_LTO": "thin"}),
}
# netem applies to loopback in both directions: each delay counts twice in a round trip.
SHAPES = {
    "loss05": ["delay", "20ms", "loss", "0.5%", "rate", "50mbit", "limit", "20000"],
    "loss01": ["delay", "20ms", "loss", "0.1%", "rate", "100mbit", "limit", "20000"],
}
# Each loss variant: server build, client build, listener, extra environment.
GO_ONE_DATAGRAM = {"QUIC_GO_DISABLE_GSO": "true"}
LOSS_VARIANTS: dict[str, tuple[str, str, str, dict[str, str]]] = {
    "head": ("head", "head", "http3", {}), "noack": ("noack", "noack", "http3", {}),
    "ack31": ("ack31", "ack31", "http3", {}), "http2": ("head", "head", "http2", {}),
    "go": ("go", "go", "http3", GO_ONE_DATAGRAM),
}
PROFILE_CASES = {"cpu": [("http3", "download"), ("http3", "upload"), ("http2", "download"), ("http1-tls", "upload")],
                 "client": [("http3", "download"), ("http3", "upload"), ("http2", "download")]}


def git(*arguments: str) -> str:
    return subprocess.run(["git", *arguments], cwd=ROOT, check=True, capture_output=True, text=True).stdout.strip()


def environment(overrides: dict[str, str]) -> dict[str, str]:
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("GM_", "CARGO_PROFILE_")) and not key.endswith("RUSTFLAGS")}
    musl = musl_target()
    env |= {"CARGO_INCREMENTAL": "0", "CC_" + musl.replace("-", "_"): f"{musl.split('-')[0]}-linux-musl-gcc"}
    return env | overrides


def tree_for(label: str, edits: list[Edit]) -> Path:
    if not edits:
        return ROOT
    tree = OUT / f"{label}-src"
    if not tree.exists():
        git("worktree", "add", "--detach", str(tree), git("rev-parse", "HEAD"))
        for path, old, new, count in edits:
            text = (tree / path).read_text()
            if text.count(old) != count:
                sys.exit(f"{label}: {old!r} is not in {path} {count} times")
            (tree / path).write_text(text.replace(old, new))
    return tree


def compile_tree(label: str, tree: Path, profile: str, overrides: dict[str, str], locked: bool = True) -> None:
    """Copies the tree's server and TUI to BIN as LABEL-SIDE; each label builds under its own target directory."""
    musl, built = musl_target(), tree / f"rust/target/campaign-{label}"
    command = ["cargo", "build", *(["--locked"] if locked else []), "--profile", profile, "--target", musl,
               "-p", "graphite-meter-server", "-p", "graphite-meter-client"]
    env = environment(overrides | {"CARGO_TARGET_DIR": str(built)})
    with (OUT / f"build-{label}.log").open("w") as log:
        status = subprocess.run(command, cwd=tree / "rust", env=env, stdout=log, stderr=subprocess.STDOUT).returncode
    if status:
        sys.exit(f"{label} build failed:\n" + (OUT / f"build-{label}.log").read_text()[-6000:])
    for side in ("server", "client"):
        shutil.copy2(built / musl / profile / f"graphite-meter-{side}", BIN / f"{label}-{side}")


def setup() -> None:
    BIN.mkdir(parents=True, exist_ok=True)
    RESULTS.mkdir(parents=True, exist_ok=True)
    subprocess.run(["rustup", "target", "add", musl_target()], cwd=ROOT / "rust", check=True)


def build_loss() -> None:
    """noq copies that send one datagram per call, so netem's loss falls on single packets as on a real link."""
    setup()
    metadata = json.loads(subprocess.run(["cargo", "metadata", "--format-version", "1", "--locked"], cwd=ROOT / "rust",
                                         check=True, capture_output=True, text=True).stdout)
    manifest = next(Path(package["manifest_path"]) for package in metadata["packages"] if package["name"] == "noq")
    checkout = OUT / "noq-one-datagram"
    shutil.copytree(manifest.parent.parent, checkout, dirs_exist_ok=True)
    connection = checkout / "noq/src/connection.rs"
    old = "const MAX_TRANSMIT_SEGMENTS: NonZeroUsize = NonZeroUsize::new(32).expect(\"known\");"
    text = connection.read_text()
    if text.count(old) != 1:
        sys.exit("noq: the transmit segment bound is not in connection.rs once")
    connection.write_text(text.replace(old, old.replace("new(32)", "new(1)")))
    pin = ("rust/Cargo.toml", NOQ_PIN, f'noq = {{ path = "{checkout / "noq"}" }}', 1)
    for label, edits in LOSS_BUILDS.items():
        # The path pin moves noq's lock entries, so Cargo relocks each copy.
        compile_tree(label, tree_for(label, [*edits, pin]), "ci", {}, locked=False)
    fixture = Fixture("campaign-go-")
    shutil.copy2(fixture.go_build("go-server", "./cmd/graphite-meter"), BIN / "go-server")
    shutil.copy2(fixture.go_build("go-client", "./cmd/graphite-meter-client"), BIN / "go-client")


def build_profiles() -> None:
    setup()
    for label, (profile, overrides) in PROFILES.items():
        compile_tree(label, ROOT, profile, overrides)


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


def transfer(fixture: Fixture, server_bin: str, client_bin: str, listener: str, direction: str,
             extra: dict[str, str]) -> dict[str, object]:
    server = fixture.server(pinned(BIN / f"{server_bin}-server", fixture.directory), server_bin,
                            {"TOKIO_WORKER_THREADS": "2"} | extra)
    with server:
        assert server.process is not None
        pid = server.process.pid
        before = cpu(pid)
        run = subprocess.run(["/usr/bin/time", "-f", "GM_TIME %U %S",
                              *bulk(BIN / f"{client_bin}-client", server, listener, direction)],
                             capture_output=True, text=True, timeout=SECONDS * 8, env=os.environ | extra)
        seconds = cpu(pid) - before
    output = run.stdout + run.stderr
    clock = re.search(r"GM_TIME ([\d.]+) ([\d.]+)", output)
    used = float(clock[1]) + float(clock[2]) if clock else None
    gbps = rate(run.stdout)
    return {"mbps": gbps * 1000 if gbps else None, "clientCpuPerGiB": per_gib(used, gbps),
            "serverCpuPerGiB": per_gib(seconds, gbps), "report": run.stdout[-1500:], "status": run.returncode}


def rotated(items: tuple[str, ...], shift: int) -> tuple[str, ...]:
    shift %= len(items)
    return items[shift:] + items[:shift]


def run_rows(study: str, cases: Sequence[tuple[str, ...]], variants: tuple[str, ...], measure_cell) -> list[dict]:
    rows = []
    with (RESULTS / f"{study}-runs.jsonl").open("w") as sink:
        for repeat in range(REPEATS):
            for index in range(len(cases)):
                case = cases[(index + repeat) % len(cases)]
                for variant in rotated(variants, repeat + index):
                    row = {"variant": variant, "case": " ".join(case), "repeat": repeat} | measure_cell(variant, case)
                    rows.append(row)
                    sink.write(json.dumps(row) + "\n")
                    sink.flush()
                    print(json.dumps({key: value for key, value in row.items() if key != "report"}), flush=True)
    return rows


def measure(study: str) -> None:
    RESULTS.mkdir(parents=True, exist_ok=True)
    fixture = Fixture(f"campaign-{study}-")
    if study in SHAPES:
        variants = tuple(LOSS_VARIANTS)
        for command in (["ip", "link", "set", "lo", "mtu", "1500"], ["ethtool", "-K", "lo", "tso", "off", "gso", "off"],
                        ["tc", "qdisc", "replace", "dev", "lo", "root", "netem", *SHAPES[study]]):
            subprocess.run(["sudo", *command], check=True)
        try:
            rows = run_rows(study, [("download",), ("upload",)], variants,
                            lambda variant, case: transfer(fixture, *LOSS_VARIANTS[variant][:3], case[0],
                                                           LOSS_VARIANTS[variant][3]))
        finally:
            subprocess.run(["sudo", "tc", "qdisc", "del", "dev", "lo", "root"], check=False)
            subprocess.run(["sudo", "ethtool", "-K", "lo", "tso", "on", "gso", "on"], check=False)
            subprocess.run(["sudo", "ip", "link", "set", "lo", "mtu", "65536"], check=False)
        return summarize(study, variants, rows, ("mbps",))
    variants = tuple(PROFILES)
    if study == "cpu":
        cell = lambda variant, case: transfer(fixture, variant, "release", case[0], case[1], {})
        metrics = ("mbps", "serverCpuPerGiB")
    else:
        cell = lambda variant, case: transfer(fixture, "release", variant, case[0], case[1], {})
        metrics = ("mbps", "clientCpuPerGiB", "serverCpuPerGiB")
    summarize(study, variants, run_rows(study, PROFILE_CASES[study], variants, cell), metrics)


def summarize(study: str, variants: tuple[str, ...], rows: list[dict], metrics: tuple[str, ...]) -> None:
    """Medians over repeats with their range, variants side by side."""
    lines = [f"### Campaign · {study} study · {os.uname().machine} · median (range)", "",
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
    if sys.argv[1] == "build-loss":
        build_loss()
    elif sys.argv[1] == "build-profiles":
        build_profiles()
    else:
        # The study name comes from this module's constants, never from the command line.
        measure(next(name for name in (*SHAPES, *PROFILE_CASES) if name == sys.argv[2]))
