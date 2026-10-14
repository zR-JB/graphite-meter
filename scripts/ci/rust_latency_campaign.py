"""Temporary campaign for the Rust review pull request, sixth run: the last open questions.

build-net: ci-profile musl binaries of the head (one ACK per ten packets) and of ACK-frequency variants, a server whose
    ring ignores the CPU's AES and PMULL (arm64), and Go's server and TUI.
measure lossy|shaped: HTTP/3 over netem-shaped loopback against HTTP/2 and Go's HTTP/3: throughput and CPU per GiB.
measure cipher: server CPU per GiB serving HTTPS/1.1 to curl offering one TLS 1.3 suite, with and without AES.
build-pgo: release-profile binaries, instrumented ones trained on loopback transfers, and a build using that profile.
build-cross PROFILE: release-profile binaries using another architecture's profile.
measure cpu|client: server and TUI CPU per GiB of the release, PGO and cross-PGO builds.
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
SECONDS = int(os.environ.get("CAMPAIGN_SECONDS", "15"))
REPEATS = int(os.environ.get("CAMPAIGN_REPEATS", "6"))
SERVER_CPUS, CLIENT_CPUS = "0,1", "2,3"
THRESHOLD = "const ACK_ELICITING_THRESHOLD: u32 = 9;"
ASK = "    transport.ack_frequency_config(Some(ack_frequency()));\n"
QUIC = "rust/net/src/quic.rs"
# Each ci build: its edits (path, old, new, occurrences) on the head and its binaries.
Edit = tuple[str, str, str, int]
NET_BUILDS: dict[str, tuple[list[Edit], tuple[str, ...]]] = {
    "head": ([], ("server", "client")),
    "noack": ([(QUIC, ASK, "", 2)], ("server", "client")),
    "ack1": ([(QUIC, THRESHOLD, THRESHOLD.replace("9;", "1;"), 1)], ("server", "client")),
    "ack3": ([(QUIC, THRESHOLD, THRESHOLD.replace("9;", "3;"), 1)], ("server", "client")),
    "ack31": ([(QUIC, THRESHOLD, THRESHOLD.replace("9;", "31;"), 1)], ("server", "client")),
    "ack9r1": ([(QUIC, "    ack.ack_eliciting_threshold(VarInt::from_u32(ACK_ELICITING_THRESHOLD));\n",
                 "    ack.ack_eliciting_threshold(VarInt::from_u32(ACK_ELICITING_THRESHOLD));\n"
                 "    ack.reordering_threshold(VarInt::from_u32(1));\n", 1)], ("server", "client")),
}
# netem applies to loopback in both directions: each delay counts twice in a round trip.
SHAPES = {
    "lossy": ["delay", "20ms", "loss", "0.5%", "rate", "50mbit", "limit", "20000"],
    "shaped": ["delay", "10ms", "rate", "1gbit", "limit", "20000"],
}
CASES = [("download",), ("upload",)]
# Each study's variants: server build, client build, listener.
NET_STUDIES = {
    "lossy": {"head": ("head", "head", "http3"), "noack": ("noack", "noack", "http3"),
              "ack1": ("ack1", "ack1", "http3"), "ack3": ("ack3", "ack3", "http3"),
              "ack31": ("ack31", "ack31", "http3"), "ack9r1": ("ack9r1", "ack9r1", "http3"),
              "http2": ("head", "head", "http2"), "go": ("go", "go", "http3")},
    "shaped": {"head": ("head", "head", "http3"), "noack": ("noack", "noack", "http3"),
               "ack3": ("ack3", "ack3", "http3"), "ack31": ("ack31", "ack31", "http3")},
}
PGO_CASES = {"cpu": [("http3", "download"), ("http3", "upload"), ("http2", "download"), ("http1-tls", "upload")],
             "client": [("http3", "download"), ("http3", "upload"), ("http2", "download")]}
SUITES = ["TLS_AES_128_GCM_SHA256", "TLS_AES_256_GCM_SHA384", "TLS_CHACHA20_POLY1305_SHA256"]


def git(*arguments: str) -> str:
    return subprocess.run(["git", *arguments], cwd=ROOT, check=True, capture_output=True, text=True).stdout.strip()


def environment(rustflags: str = "") -> dict[str, str]:
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("GM_", "CARGO_PROFILE_")) and not key.endswith("RUSTFLAGS")}
    musl = musl_target()
    env |= {"CARGO_INCREMENTAL": "0", "CC_" + musl.replace("-", "_"): f"{musl.split('-')[0]}-linux-musl-gcc"}
    return env | ({"RUSTFLAGS": rustflags} if rustflags else {})


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


def compile_tree(label: str, tree: Path, sides: tuple[str, ...], profile: str, rustflags: str = "",
                 locked: bool = True) -> None:
    """Copies the tree's binaries to BIN as LABEL-SIDE; each label builds under its own target directory."""
    musl, built = musl_target(), tree / f"rust/target/campaign-{label}"
    packages = [argument for side in sides for argument in ("-p", f"graphite-meter-{side}")]
    command = ["cargo", "build", *(["--locked"] if locked else []), "--profile", profile, "--target", musl, *packages]
    with (OUT / f"build-{label}.log").open("w") as log:
        status = subprocess.run(command, cwd=tree / "rust", env=environment(rustflags) | {"CARGO_TARGET_DIR": str(built)},
                                stdout=log, stderr=subprocess.STDOUT).returncode
    if status:
        sys.exit(f"{label} build failed:\n" + (OUT / f"build-{label}.log").read_text()[-6000:])
    for side in sides:
        shutil.copy2(built / musl / profile / f"graphite-meter-{side}", BIN / f"{label}-{side}")


def setup() -> None:
    BIN.mkdir(parents=True, exist_ok=True)
    RESULTS.mkdir(parents=True, exist_ok=True)
    subprocess.run(["rustup", "target", "add", musl_target()], cwd=ROOT / "rust", check=True)


def build_net() -> None:
    setup()
    for label, (edits, sides) in NET_BUILDS.items():
        compile_tree(label, tree_for(label, edits), sides, "ci")
    # ring with the CPU's AES and PMULL ignored, as on a Raspberry Pi 4; x86 detection is untouched.
    subprocess.run(["cargo", "fetch", "--locked"], cwd=ROOT / "rust", check=True)
    home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    source = next(home.glob("registry/src/*/ring-0.17.14"))
    ring = OUT / "ring-masked"
    shutil.copytree(source, ring, dirs_exist_ok=True)
    linux = ring / "src/cpu/arm/linux.rs"
    text = linux.read_text()
    for cap in ("HWCAP_AES", "HWCAP_PMULL"):
        old = f"if caps & {cap} == {cap} {{"
        if text.count(old) != 1:
            sys.exit(f"ring: {old!r} is not in linux.rs once")
        text = text.replace(old, f"if false && caps & {cap} == {cap} {{")
    linux.write_text(text)
    masked = tree_for("masked", [("rust/Cargo.toml", "[patch.crates-io]\n",
                                  f"[patch.crates-io]\nring = {{ path = \"{ring}\" }}\n", 1)])
    # The path patch moves ring's lock entry, so Cargo relocks the copy.
    compile_tree("masked", masked, ("server",), "ci", locked=False)
    fixture = Fixture("campaign-go-")
    shutil.copy2(fixture.go_build("go-server", "./cmd/graphite-meter"), BIN / "go-server")
    shutil.copy2(fixture.go_build("go-client", "./cmd/graphite-meter-client"), BIN / "go-client")


def build_pgo() -> None:
    """Release binaries, a profile trained on loopback transfers, and release binaries using it."""
    setup()
    subprocess.run(["rustup", "component", "add", "llvm-tools"], cwd=ROOT / "rust", check=True)
    compile_tree("release", ROOT, ("server", "client"), "release")
    profiles = OUT / "pgo-raw"
    shutil.rmtree(profiles, ignore_errors=True)
    profiles.mkdir(parents=True)
    compile_tree("instrumented", ROOT, ("server", "client"), "release", f"-Cprofile-generate={profiles}")
    fixture = Fixture("campaign-train-")
    server = fixture.server(pinned(BIN / "instrumented-server", fixture.directory), "train",
                            {"TOKIO_WORKER_THREADS": "2"})
    with server:
        for listener, direction in [("http3", "download"), ("http3", "upload"), ("http2", "download"),
                                    ("http2", "upload"), ("http1-tls", "upload"), ("http1-tls", "download")]:
            subprocess.run(bulk(BIN / "instrumented-client", server, listener, direction, 8, "--loaded-latency=true"),
                           capture_output=True, timeout=180)
        subprocess.run(bulk(BIN / "instrumented-client", server, "http3", "download", 8, "--loaded-latency=true",
                            "--loaded-ping", "reply-driven", "--latency-transport", "webtransport",
                            "--latency-origin", server.origin("http3")), capture_output=True, timeout=180)
    raw = sorted(profiles.glob("*.profraw"))
    if not raw:
        sys.exit("PGO training wrote no profiles")
    merged = RESULTS / f"pgo-{os.uname().machine}.profdata"
    subprocess.run([str(llvm_profdata()), "merge", "-o", str(merged), *map(str, raw)], check=True)
    compile_tree("pgo", ROOT, ("server", "client"), "release", f"-Cprofile-use={merged}")
    export = OUT / "export"
    export.mkdir(exist_ok=True)
    for name in ("release-server", "release-client", "pgo-server", "pgo-client"):
        shutil.copy2(BIN / name, export / name)
    shutil.copy2(merged, export / merged.name)


def build_cross(profile: str) -> None:
    setup()
    subprocess.run(["rustup", "component", "add", "llvm-tools"], cwd=ROOT / "rust", check=True)
    compile_tree("pgox", ROOT, ("server", "client"), "release", f"-Cprofile-use={Path(profile).resolve()}")


def llvm_profdata() -> Path:
    rustc = lambda *arguments: subprocess.run(["rustc", *arguments], cwd=ROOT / "rust", check=True,
                                              capture_output=True, text=True).stdout
    host = re.search(r"host: (\S+)", rustc("-vV"))
    assert host is not None
    return Path(rustc("--print", "sysroot").strip()) / "lib/rustlib" / host[1] / "bin/llvm-profdata"


def cpu(pid: int) -> float:
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def pinned(binary: Path, directory: Path) -> Path:
    """`binary` behind `exec taskset`, which keeps the process ID the CPU accounting reads."""
    wrapper = directory / f"{binary.name}-pinned"
    wrapper.write_text(f"#!/bin/sh\nexec taskset -c {SERVER_CPUS} {binary} \"$@\"\n")
    wrapper.chmod(0o755)
    return wrapper


def bulk(client: Path, server: Server, listener: str, direction: str, seconds: int, *extra: str) -> list[str]:
    protocol = "http1" if listener.startswith("http1") else listener
    return ["taskset", "-c", CLIENT_CPUS, str(client), "--report", "--insecure", "--url", server.origin("http1"),
            "--stages", direction, "--streams", "4", "--throughput-origin", server.origin(listener),
            "--throughput-protocol", protocol, "--throughput-transport", "fetch-stream", "--warmup", "500ms",
            f"--{direction}-duration", f"{seconds}s", *extra]


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
        command = bulk(BIN / f"{client_bin}-client", server, listener, direction, SECONDS, "--loaded-latency=false")
        run = subprocess.run(["/usr/bin/time", "-f", "GM_TIME %U %S", *command], capture_output=True, text=True,
                             timeout=SECONDS * 8)
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
    if study == "cipher":
        return cipher(fixture)
    if study in NET_STUDIES:
        specs = {name: spec for name, spec in NET_STUDIES[study].items()
                 if (BIN / f"{spec[0]}-server").exists() and (BIN / f"{spec[1]}-client").exists()}
        subprocess.run(["sudo", "tc", "qdisc", "replace", "dev", "lo", "root", "netem", *SHAPES[study]], check=True)
        try:
            rows = run_rows(study, CASES, tuple(specs),
                            lambda variant, case: transfer(fixture, specs[variant][0], specs[variant][1],
                                                           specs[variant][2], case[0]))
        finally:
            subprocess.run(["sudo", "tc", "qdisc", "del", "dev", "lo", "root"], check=False)
        return summarize(study, tuple(specs), rows, ("mbps", "clientCpuPerGiB", "serverCpuPerGiB"))
    variants = tuple(name for name in ("release", "pgo", "pgox")
                     if (BIN / f"{name}-server").exists() and (BIN / f"{name}-client").exists())
    if study == "cpu":
        cell = lambda variant, case: transfer(fixture, variant, "release", *case)
        metrics = ("mbps", "serverCpuPerGiB")
    else:
        cell = lambda variant, case: transfer(fixture, "release", variant, *case)
        metrics = ("mbps", "clientCpuPerGiB", "serverCpuPerGiB")
    summarize(study, variants, run_rows(study, PGO_CASES[study], variants, cell), metrics)


def cipher(fixture: Fixture) -> None:
    """Server CPU per GiB for each suite curl offers alone, 4 GiB per download."""
    size = 4 << 30
    rows = []
    for build in ("head", "masked"):
        server = fixture.server(pinned(BIN / f"{build}-server", fixture.directory), f"cipher-{build}", {})
        with server:
            assert server.process is not None
            pid = server.process.pid
            for repeat in range(3):
                for suite in rotated(tuple(SUITES), repeat):
                    before = cpu(pid)
                    run = subprocess.run(["taskset", "-c", CLIENT_CPUS, "curl", "--silent", "--show-error",
                                          "--insecure", "--tls13-ciphers", suite, "--verbose", "--output", "/dev/null",
                                          "--write-out", "%{size_download} %{speed_download}",
                                          f"{server.origin('http1-tls')}/download?bytes={size}"],
                                         capture_output=True, text=True, timeout=600)
                    used = cpu(pid) - before
                    got, speed = (float(value) for value in run.stdout.split())
                    row = {"variant": build, "case": suite, "repeat": repeat, "negotiated": suite in run.stderr,
                           "mbps": round(speed * 8 / 1e6, 1), "serverCpuPerGiB": round(used / (got / 2**30), 3)}
                    rows.append(row)
                    print(json.dumps(row), flush=True)
    (RESULTS / "cipher-runs.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows))
    summarize("cipher", ("head", "masked"), rows, ("mbps", "serverCpuPerGiB"))


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
    mode = sys.argv[1]
    if mode == "build-net":
        build_net()
    elif mode == "build-pgo":
        build_pgo()
    elif mode == "build-cross":
        build_cross(sys.argv[2])
    else:
        measure(sys.argv[2])
