"""Clients, a router and real servers inside disposable user, network and PID namespaces.

Run through server-collection.sh. No host interface or qdisc is changed. The browser control
channel stays on unshaped client loopback. Results are written under the temporary directory;
the caller supplies already built servers, clients and pinned Chrome. Profiles server-cap,
differing-rtt and shared-cap drive the browser UI against four coordinated Go servers; matrix
compares the Go and Rust servers and clients on one shaped path.
"""

import hashlib
import itertools
import json
import math
import os
from pathlib import Path
import queue
import random
import re
import statistics
import sys
import threading
import subprocess
import tempfile
import time
import zlib

ROOT = Path(__file__).resolve().parents[2]


def command(*args, namespace=None, **kwargs):
    prefix = ["nsenter", "-t", str(namespace), "-n", "--"] if namespace else []
    return subprocess.run(prefix + list(args), check=True, **kwargs)


keepers = []
backends = []
# Linux's default congestion control and buffers for 1 Gbit/s at 300 ms, whatever the host tuned.
TCP = ["net.ipv4.tcp_congestion_control=cubic", "net.ipv4.tcp_rmem=4096 131072 134217728", "net.ipv4.tcp_wmem=4096 16384 134217728"]


def namespace():
    child = subprocess.Popen(["unshare", "--net", "sleep", "infinity"])
    keepers.append(child)
    for _ in range(100):
        if os.readlink(f"/proc/{child.pid}/ns/net") != os.readlink("/proc/self/ns/net"):
            command("ip", "link", "set", "lo", "up", namespace=child.pid)
            command("sysctl", "-qw", *TCP, namespace=child.pid)
            return child.pid
        time.sleep(0.01)
    raise RuntimeError("network namespace did not start")


def link(name, a_namespace, a_address, b_namespace, b_address):
    other = name + "p"
    command("ip", "link", "add", name, "type", "veth", "peer", "name", other)
    for iface, owner, address in [(name, a_namespace, a_address), (other, b_namespace, b_address)]:
        if owner:
            command("ip", "link", "set", iface, "netns", str(owner))
        # netem must see single packets; batches would be delayed and dropped whole.
        command("ethtool", "-K", iface, "tso", "off", "gso", "off", "gro", "off", "tx-udp-segmentation", "off",
                namespace=owner, stdout=subprocess.DEVNULL)
        if address:
            command("ip", "addr", "add", address, "dev", iface, namespace=owner)
        command("ip", "link", "set", iface, "up", namespace=owner)


def shape(owner, iface, delay_ms, mbps, loss=0.0, limit=200000, seed=None):
    args = ["tc", "qdisc", "replace", "dev", iface, "root", "netem", "limit", str(limit), "delay", f"{delay_ms:g}ms"]
    if mbps:
        args += ["rate", f"{mbps}mbit"]
    if loss:
        args += ["loss", f"{loss:g}%"]
    if seed is not None:
        args += ["seed", str(seed)]
    command(*args, namespace=owner)


def browser_processes(parent):
    """Process CPU includes all threads; RSS is summed and may count shared pages twice."""
    processes = {}
    for path in Path("/proc").glob("[0-9]*/stat"):
        try:
            raw = path.read_text()
            end = raw.rfind(")")
            fields = raw[end + 2:].split()
            processes[int(path.parent.name)] = (int(fields[1]), raw[raw.find("(") + 1:end], int(fields[11]) + int(fields[12]), int(fields[21]) * os.sysconf("SC_PAGE_SIZE"))
        except (OSError, ValueError, IndexError):
            continue  # The process exited while its stat line was read.
    descendants = {parent}
    while True:
        found = {pid for pid, (ppid, *_rest) in processes.items() if ppid in descendants}
        if found <= descendants:
            break
        descendants |= found
    return {pid: (cpu, rss) for pid, (_ppid, name, cpu, rss) in processes.items() if pid in descendants and name.startswith("chrom")}


def run_cell(environment, output, profile, count, repeat):
    cell = f"{profile}-{count}-r{repeat}"
    env = {**environment, "GM_MULTI_BENCH_COUNT": str(count)}
    with (output / f"{cell}.log").open("w") as error_log:
        process = subprocess.Popen(["bun", "test", "./bench/server-collection.bench.ts", "--no-orphans", "--timeout", "60000"], env=env, stdout=subprocess.PIPE, stderr=error_log, text=True, bufsize=1)
        lines = queue.SimpleQueue()
        def read_lines():
            for line in process.stdout:
                lines.put(line.strip())

        reader = threading.Thread(target=read_lines, daemon=True)
        reader.start()
        baseline = {}
        highwater = {}
        peak_rss = 0
        initial_rss = 0
        started = None
        result = None
        try:
            deadline = time.monotonic() + 75
            while process.poll() is None:
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"{cell} exceeded the bounded cell deadline")
                metrics = browser_processes(process.pid)
                if started and result is None:
                    highwater.update({pid: max(highwater.get(pid, 0), cpu) for pid, (cpu, _rss) in metrics.items()})
                    peak_rss = max(peak_rss, sum(rss for _cpu, rss in metrics.values()))
                time.sleep(0.05)
                while not lines.empty():
                    line = lines.get()
                    error_log.write(line + "\n")
                    error_log.flush()
                    if line.startswith("GM_BENCH_STEP "):
                        print(cell, line, flush=True)
                    if line == "GM_BENCH_BEGIN":
                        baseline = {pid: cpu for pid, (cpu, _rss) in metrics.items()}
                        initial_rss = sum(rss for _cpu, rss in metrics.values())
                        started = time.monotonic()
                    elif line.startswith("GM_BENCH_END "):
                        result = json.loads(line.removeprefix("GM_BENCH_END "))
                        result.update(profile=profile, repeat=repeat, wallSec=time.monotonic() - started)
            if process.returncode or result is None:
                raise RuntimeError(f"{cell} failed; inspect {output / (cell + '.log')}")
            result.update(browserCpuSec=sum(max(0, cpu - baseline.get(pid, 0)) for pid, cpu in highwater.items()) / os.sysconf("SC_CLK_TCK"), initialBrowserRssBytes=initial_rss, peakBrowserRssBytes=peak_rss)
            with (output / "results.ndjson").open("a") as rows:
                rows.write(json.dumps(result) + "\n")
            print(f"{cell}: {result['downloadMbps']:.1f} Mbit/s; browser CPU {result['browserCpuSec']:.2f}s; peak RSS {peak_rss / 2**20:.1f} MiB", flush=True)
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)


PROFILES = {  # name: (client link Mbit/s, delay ms and capacity Mbit/s per server)
    "server-cap": (0, [1, 1, 1, 1], [40, 60, 80, 100]),
    "differing-rtt": (0, [2.5, 10, 25, 50], [80, 80, 80, 80]),
    "shared-cap": (100, [1, 1, 1, 1], [200, 200, 200, 200]),
}


def collection(env, output, router, nodes, profiles, repeats):
    servers = [{"id": "self" if i == 1 else f"server-{i}", "url": f"https://10.81.{i}.2:7247", "name": f"Path {i}"} for i in range(1, 5)]
    env = {key: value for key, value in env.items() if not key.startswith("GM_AUTH_") and not key.startswith("GM_SERVER_CATALOG")}
    for i, node in enumerate(nodes, 1):
        backend_env = {**env, "GM_AUTH_MODE": "off", "GM_H1_ADDR": "0.0.0.0:7246", "GM_H1_TLS_ADDR": "0.0.0.0:7247", "GM_H2_ADDR": "", "GM_H3_ADDR": "", "GM_TLS_CERT": env["GM_E2E_TLS_CERT"], "GM_TLS_KEY": env["GM_E2E_TLS_KEY"], "GM_SERVER_NAME": f"Path {i}"}
        if i == 1:
            backend_env["GM_SERVER_CATALOG"] = json.dumps({"servers": servers[1:]})
        with (output / f"server-{i}.log").open("w") as log:
            backends.append(subprocess.Popen(["nsenter", "-t", str(node), "-n", "--", env["GM_E2E_SERVER_BIN"]], env=backend_env, stdout=log, stderr=log))
    env["GM_MULTI_BENCH_SERVERS"] = json.dumps(servers)
    time.sleep(0.5)
    command("curl", "--noproxy", "*", "--fail", "--silent", "--max-time", "3", "http://10.81.1.2:7246/preflight", stdout=subprocess.DEVNULL)
    if any(server.poll() is not None for server in backends):
        raise RuntimeError("a measurement server failed to start")
    for profile in profiles:
        client, delays, capacities = PROFILES[profile]
        shape(router, "gmclientp", 0.2, client)
        for i, (node, delay, capacity) in enumerate(zip(nodes, delays, capacities), 1):
            shape(node, f"gm{i}p", delay, capacity)
            shape(router, f"gm{i}", delay, 0)
        for repeat in range(1, repeats + 1):
            for count in ([4, 1, 2] if repeat % 2 else [2, 4, 1]):
                run_cell(env, output, profile, count, repeat)
    stop(backends)
    # The matrix server's own egress stays unshaped; netem there would see its segmentation batches.
    command("tc", "qdisc", "del", "dev", "gm1p", "root", namespace=nodes[0])


MATRIX = {
    "server": "go,rust",
    "client": "go,rust,browser",
    "transport": "h1,h2,h3,wt",
    "direction": "download,upload,bidirectional",
    "rtt": "0,20,100,300",
    "loss": "0,0.1,1",
    "count": "1,16",
    "rate": "1000",
}
WARMUP_S, MEASURE_S = 4, 8
HOST = "10.81.1.2"
THROUGHPUT = {  # transport: (native client flags, browser target, protocol the browser must negotiate)
    "h1": (["--throughput-origin", f"https://{HOST}:7247"], "protocol:http1", "http/1.1"),
    "h2": (["--throughput-origin", f"https://{HOST}:7248"], "protocol:http2", "h2"),
    "h3": (["--throughput-origin", f"https://{HOST}:7249"], "protocol:http3", "h3"),
    "wt": (["--throughput-origin", f"https://{HOST}:7249", "--throughput-transport", "webtransport"], "transport:webtransport", "h3"),
}
STAGE_LABELS = {"download": ("Download", "Loaded down"), "upload": ("Upload", "Loaded up"), "bidirectional": ("Bidirectional", "Loaded bi-dir")}
NUMBERS = {"rtt": r"\d+(\.\d+)?", "loss": r"\d+(\.\d+)?", "count": r"[1-9]\d*", "rate": r"[1-9]\d*"}


def matrix_cells(spec):
    axes = {axis: values.split(",") for axis, values in MATRIX.items()}
    for term in spec.split():
        axis, _, raw = term.partition("=")
        values = raw.split(",")
        if axis not in axes or not all(re.fullmatch(NUMBERS[axis], value) if axis in NUMBERS else value in axes[axis] for value in values):
            raise RuntimeError(f"GM_MULTI_BENCH_MATRIX term {term!r} names no matrix axis or value")
        axes[axis] = values
    cells = [dict(zip(axes, values)) for values in itertools.product(*axes.values())]
    # One browser drives one page, so browser cells have one client.
    return [cell for cell in cells if cell["client"] != "browser" or cell["count"] == "1"]


def output_of(*args, cwd=ROOT):
    return subprocess.run(args, cwd=cwd, capture_output=True, text=True, check=True).stdout.strip()


def build_identity(paths):
    profile = re.search(r"^\[profile\.release\]\n(?:[^\[\n].*\n)*", (ROOT / "rust/Cargo.toml").read_text(), re.M)
    return {
        "commit": output_of("git", "rev-parse", "HEAD"),
        "dirty": output_of("git", "status", "--porcelain", "--untracked-files=no") != "",
        "rustc": output_of("rustc", "-vV", cwd=ROOT / "rust"),
        "cargoRelease": profile[0].strip() if profile else None,
        "rustFlags": {key: value for key, value in os.environ.items() if "RUSTFLAGS" in key or key.startswith("CARGO_PROFILE_")},
        "binaries": {name: {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()} for name, path in paths.items()},
        "goBuild": {name: [line.strip() for line in output_of("go", "version", "-m", str(path)).splitlines() if not line.startswith(("\tdep", "\tmod", "\t=>"))]
                    for name, path in paths.items() if name.startswith("go-")},
        "kernel": os.uname().release,
        "cpu": next(line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")),
        "cpus": os.cpu_count(),
        "governors": sorted({path.read_text().strip() for path in Path("/sys/devices/system/cpu").glob("cpu*/cpufreq/scaling_governor")}),
        "method": {"warmupSec": WARMUP_S, "measureSec": MEASURE_S, "loadedPing": "fast", "latency": "websocket",
                   "offloads": "tso gso gro tx-udp-segmentation off", "queue": "one-way delay line plus one BDP, at least 1000 packets"},
    }


def usage(pid):
    """Server CPU ticks (all threads, user and system) and peak resident bytes."""
    stat = Path(f"/proc/{pid}/stat").read_text()
    fields = stat[stat.rfind(")") + 2:].split()
    peak = next(line for line in Path(f"/proc/{pid}/status").read_text().splitlines() if line.startswith("VmHWM:"))
    return int(fields[11]), int(fields[12]), int(peak.split()[1]) * 1024


def wire(router):
    """Bytes the router delivered toward the clients and toward the server, after netem loss."""
    counters = {line.split(":")[0].strip(): line.split(":")[1].split() for line in Path(f"/proc/{router}/net/dev").read_text().splitlines()[2:]}
    return int(counters["gmlan"][8]), int(counters["gm1"][8])


def listening(node):
    bound = set()
    for kind in ("tcp", "tcp6", "udp", "udp6"):
        for line in Path(f"/proc/{node}/net/{kind}").read_text().splitlines()[1:]:
            fields = line.split()
            if kind.startswith("udp") or fields[3] == "0A":
                bound.add((kind[:3], int(fields[1].rsplit(":", 1)[1], 16)))
    return {("tcp", 7246), ("tcp", 7247), ("tcp", 7248), ("tcp", 7249), ("udp", 7249)} <= bound


def mbps(text):
    match = re.fullmatch(r"([\d.]+) ([kMGT]?)bit/s", text.strip())
    return float(match[1]) * {"": 1e-6, "k": 1e-3, "M": 1, "G": 1e3, "T": 1e6}[match[2]] if match else None


def ms(text):
    match = re.fullmatch(r"(?:< )?([\d.]+) ms", text.strip())  # "< 0.1 ms" reads as its 0.1 ms bound.
    return float(match[1]) if match else None


def go_report(lines, direction):
    stage, population = STAGE_LABELS[direction]
    result = {}
    for line in lines:
        if match := re.match(rf"([↓↑]) {stage} +(\S+ \S*bit/s)", line):
            result["downMbps" if match[1] == "↓" else "upMbps"] = mbps(match[2])
    header = next(line for line in lines if re.match(r"Latency +Median", line))
    columns = list(re.finditer(r"\S+(?: \S+)*", header))
    row = next(line for line in lines if line.startswith(population + " "))
    cells = {column[0]: row[column.start():end].strip() for column, end in zip(columns, [c.start() for c in columns[1:]] + [None])}
    timeouts, resolved = (int(n.replace(",", "")) for n in re.findall(r"[\d,]+", cells["Probe timeouts"])[:2])
    return result | {"latencyP50Ms": ms(cells["Median"]), "latencyP95Ms": ms(cells["P95"]), "replies": resolved - timeouts, "timeouts": timeouts}


def rust_report(lines, direction):
    stage = STAGE_LABELS[direction][0]
    at = next(i for i, line in enumerate(lines) if re.match(rf"{stage}(?: · [^:]*)?: Download ", line))
    rates = re.match(r".*?: Download (.+?), Upload (.+)$", lines[at])
    latency = next(match for line in lines[at + 1:] if (match := re.match(r"  .+: Median (.+?), Added .+?, p95 (.+?), Jitter .+?, Probe timeouts ([\d,]+)/([\d,]+)", line)))
    timeouts, resolved = int(latency[3].replace(",", "")), int(latency[4].replace(",", ""))
    return {"downMbps": mbps(rates[1]), "upMbps": mbps(rates[2]),
            "latencyP50Ms": ms(latency[1]), "latencyP95Ms": ms(latency[2]), "replies": resolved - timeouts, "timeouts": timeouts}


def client_result(cell, text, status):
    """The client's own report of one stage; a report it cannot read makes the run invalid."""
    direction, result = cell["direction"], {"exit": status}
    try:
        if cell["client"] == "browser":
            end = json.loads(next(line for line in text.splitlines() if line.startswith("GM_BENCH_END ")).removeprefix("GM_BENCH_END "))
            result |= end["stages"][direction] | {"path": end["paths"][0], "browser": end["browser"]}
        else:
            lines = text.splitlines()
            result["outcome"] = re.match(r"Graphite Meter(?: · | {2,})(\S+)", lines[0])[1]
            result |= (rust_report if lines[0].startswith("Graphite Meter · ") else go_report)(lines, direction)
    except (StopIteration, IndexError, KeyError, TypeError, ValueError) as error:
        result["parseError"] = repr(error)
    needed = {"download": ["downMbps"], "upload": ["upMbps"], "bidirectional": ["downMbps", "upMbps"]}[direction]
    # Native clients fail rather than leave an explicit origin; a browser path is checked here.
    path = cell["client"] != "browser" or result.get("path", {}).get("browserProtocol") == THROUGHPUT[cell["transport"]][2]
    result["valid"] = status == 0 and path and result.get("outcome") == "Complete" and all(result.get(key) is not None for key in needed)
    return result


def client_command(cell, paths, env):
    flags, target, _ = THROUGHPUT[cell["transport"]]
    direction = cell["direction"]
    if cell["client"] == "browser":
        config = {
            "stages": {stage: stage == direction for stage in ("latency", "download", "upload", "bidirectional")},
            "skipLoadedLatencyWhenStageOff": False,
            "duration": {"warmupMs": WARMUP_S * 1000, "latencyMs": 1000, "downloadMs": MEASURE_S * 1000, "uploadMs": MEASURE_S * 1000, "bidirectionalMs": MEASURE_S * 1000},
            "loadedPingCadence": "fast",
            "adaptive": False,
            "transports": {"throughputTarget": target, "latencyTarget": "transport:websocket"},
        }
        return ["bun", "test", "./bench/server-collection.bench.ts", "--no-orphans", "--timeout", "120000"], {
            **env, "GM_MULTI_BENCH_COUNT": "1", "GM_MULTI_BENCH_CONFIG": json.dumps(config),
            "GM_MULTI_BENCH_SERVERS": json.dumps([{"id": "self", "url": f"https://{HOST}:7247", "name": "P8"}])}
    argv = [str(paths[cell["client"] + "-client"]), "--report", "--url", f"https://{HOST}:7247", "--stages", direction,
            "--warmup", f"{WARMUP_S}s", f"--{direction}-duration", f"{MEASURE_S}s", "--latency-transport", "websocket",
            "--latency-origin", f"https://{HOST}:7247", "--loaded-ping", "fast", *flags]
    return argv, {"PATH": env["PATH"], "HOME": env["HOME"], "SSL_CERT_FILE": env["SSL_CERT_FILE"], "NO_COLOR": "1"}


def matrix_run(env, output, router, node, hosts, paths, cell, run, seed):
    rtt, loss, rate, count, direction = float(cell["rtt"]), float(cell["loss"]), int(cell["rate"]), int(cell["count"]), cell["direction"]
    packets = lambda seconds: math.ceil(rate * 1e6 * seconds / (8 * 1514))
    for iface in ("gmlan", "gm1"):
        shape(router, iface, rtt / 2, rate, loss, packets(rtt / 2000) + max(1000, packets(rtt / 1000)), seed=zlib.crc32(f"{seed}/{iface}".encode()))
    directory = output / "matrix" / run
    directory.mkdir(parents=True)
    server_env = {
        "PATH": env["PATH"], "GM_AUTH_MODE": "off", "GM_SERVER_NAME": "P8", "GM_TLS_CERT": env["GM_E2E_TLS_CERT"], "GM_TLS_KEY": env["GM_E2E_TLS_KEY"],
        "GM_H1_ADDR": "0.0.0.0:7246", "GM_H1_TLS_ADDR": "0.0.0.0:7247", "GM_H2_ADDR": "0.0.0.0:7248", "GM_H3_ADDR": "0.0.0.0:7249",
        "GM_H1_PUBLIC_ORIGIN": f"http://{HOST}:7246", "GM_H1_TLS_PUBLIC_ORIGIN": f"https://{HOST}:7247",
        "GM_H2_PUBLIC_ORIGIN": f"https://{HOST}:7248", "GM_H3_PUBLIC_ORIGIN": f"https://{HOST}:7249",
    }
    with (directory / "server.log").open("w") as log:
        server = subprocess.Popen(["nsenter", "-t", str(node), "-n", "--", str(paths[cell["server"] + "-server"])], env=server_env, stdout=log, stderr=log)
    clients, error, measured, tcp = [], None, None, {}
    started = time.monotonic()
    try:
        while not listening(node):
            if server.poll() is not None or time.monotonic() > started + 10:
                raise RuntimeError("server did not start listening")
            time.sleep(0.02)
        before, started = (usage(server.pid), wire(router)), time.monotonic()
        argv, client_env = client_command(cell, paths, env)
        for i, host in enumerate(hosts[:count]):
            with (directory / f"client-{i}.out").open("w") as out, (directory / f"client-{i}.err").open("w") as err:
                clients.append(subprocess.Popen(["nsenter", "-t", str(host), "-n", "--", *argv], env=client_env, stdout=out, stderr=err))
        # Tuners such as bpftune still override congestion control per connection, so record what the server used.
        time.sleep(max(0.0, started + WARMUP_S + MEASURE_S / 2 - time.monotonic()))
        sockets = command("ss", "-tin", "state", "established", namespace=node, capture_output=True, text=True).stdout.split()
        tcp = {"congestion": sorted(set(sockets) & set(Path("/proc/sys/net/ipv4/tcp_available_congestion_control").read_text().split())),
               "buffers": command("sysctl", "-n", "net.ipv4.tcp_rmem", "net.ipv4.tcp_wmem", namespace=node, capture_output=True, text=True).stdout.split("\n")[:2]}
        for client in clients:
            client.wait(timeout=max(0.0, started + WARMUP_S + MEASURE_S + 100 - time.monotonic()))
        if server.poll() is not None:
            raise RuntimeError(f"server exited with {server.returncode}")
        measured = before, (usage(server.pid), wire(router)), time.monotonic() - started
    except (RuntimeError, subprocess.TimeoutExpired) as failure:
        error = str(failure)
    finally:
        stop(clients + [server])
    results = [client_result(cell, (directory / f"client-{i}.out").read_text(), client.returncode) for i, client in enumerate(clients)]
    row = {"run": run, **cell, "valid": error is None and len(results) == count and all(result["valid"] for result in results),
           "error": error, "serverExit": server.returncode, "serverTcp": tcp, "clients": results}
    if measured:
        ((user0, system0, _), (down0, up0)), ((user1, system1, peak), (down1, up1)), wall = measured
        tick = os.sysconf("SC_CLK_TCK")
        payload = {"download": down1 - down0, "upload": up1 - up0, "bidirectional": down1 - down0 + up1 - up0}[direction]
        cpu = (user1 - user0 + system1 - system0) / tick
        medians = {key: statistics.median(values) if (values := [r[key] for r in results if r.get(key) is not None]) else None
                   for key in ("latencyP50Ms", "latencyP95Ms")}
        row |= {"wallSec": wall, "serverCpuUserSec": (user1 - user0) / tick, "serverCpuSystemSec": (system1 - system0) / tick,
                "serverPeakRssBytes": peak, "wireDownBytes": down1 - down0, "wireUpBytes": up1 - up0,
                "cpuSecPerGbit": cpu / (payload * 8e-9) if payload else None,
                "throughputMbps": sum((r.get("downMbps") or 0) + (r.get("upMbps") or 0) for r in results), **medians}
    return row


def stop(processes):
    for process in processes:
        if process.poll() is None:
            process.terminate()
    for process in processes:
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def matrix(env, output, router, node, repeats):
    cells = matrix_cells(env.get("GM_MULTI_BENCH_MATRIX", ""))
    builds = {"go-server": ROOT / "go/graphite-meter", "rust-server": ROOT / "rust/target/release/graphite-meter-server",
              "go-client": ROOT / "go/graphite-meter-client", "rust-client": ROOT / "rust/target/release/graphite-meter-client"}
    paths = {name: path for name, path in builds.items() if any(name in (cell["server"] + "-server", cell["client"] + "-client") for cell in cells)}
    if missing := [str(path) for path in paths.values() if not path.is_file()]:
        raise RuntimeError(f"missing builds {missing}; mise run bench-matrix builds them")
    seed = int(env.get("GM_MULTI_BENCH_SEED", "1"))
    identity = build_identity(paths)
    # Each client has its own address behind one bridge, so the router's egress to it is the shared bottleneck.
    lan = namespace()
    command("ip", "link", "add", "br0", "type", "bridge", namespace=lan)
    command("ip", "link", "set", "br0", "up", namespace=lan)
    link("gmlan", router, "10.82.0.1/24", lan, None)
    hosts = [namespace() for _ in range(max(int(cell["count"]) for cell in cells))]
    for i, host in enumerate(hosts, 1):
        link(f"gmc{i}", host, f"10.82.0.{i + 1}/24", lan, None)
        command("ip", "route", "add", "default", "via", "10.82.0.1", namespace=host)
    for port in ["gmlanp", *(f"gmc{i}p" for i in range(1, len(hosts) + 1))]:
        command("ip", "link", "set", port, "master", "br0", namespace=lan)
    session = time.strftime("%Y%m%dT%H%M%S")
    total, order = len(cells) * repeats, 0
    with (output / "matrix.ndjson").open("a") as rows:
        for repeat in range(1, repeats + 1):
            ordered = cells[:]
            random.Random(f"{seed}/{repeat}").shuffle(ordered)
            for cell in ordered:
                order += 1
                run = f"{order:05d}"
                loadavg = [float(value) for value in Path("/proc/loadavg").read_text().split()[:3]]
                row = {"schema": 1, "session": session, "seed": seed, "repeat": repeat, "order": order, "loadavg": loadavg,
                       **matrix_run(env, output, router, node, hosts, paths, cell, run, f"{seed}/{repeat}"), "identity": identity}
                rows.write(json.dumps(row) + "\n")
                rows.flush()
                facts = (f"{row['throughputMbps']:.1f} Mbit/s, CPU {row['cpuSecPerGbit'] or 0:.2f} s/Gbit, "
                         f"peak RSS {row['serverPeakRssBytes'] / 2**20:.1f} MiB, p50 {row['latencyP50Ms']} ms") if "wallSec" in row else ""
                print(f"{order}/{total} {'-'.join(cell.values())}: {'valid' if row['valid'] else 'INVALID ' + str(row['error'] or 'client report')} {facts}", flush=True)
    with (output / "matrix.ndjson").open() as rows:
        summary = subprocess.run([sys.executable, str(Path(__file__).with_name("server-matrix-summary.py"))],
                                 stdin=rows, check=True, capture_output=True, text=True).stdout
    (output / "matrix-summary.txt").write_text(summary)
    print(summary, end="")


def output_directory():
    """GM_MULTI_BENCH_OUTPUT, resolved through links; it must lie inside the temporary directory."""
    path = os.path.realpath(os.environ["GM_MULTI_BENCH_OUTPUT"])
    if path.startswith(os.path.join(os.path.realpath(tempfile.gettempdir()), "")):
        return Path(path)
    raise RuntimeError("GM_MULTI_BENCH_OUTPUT must be inside the temporary directory")


def main():
    mapping = Path("/proc/self/uid_map").read_text().split()
    if os.getuid() != 0 or len(mapping) != 3 or mapping[0] != "0" or mapping[1] == "0" or mapping[2] != "1":
        raise RuntimeError("Run inside a disposable unprivileged user/network namespace")
    output = output_directory()
    output.mkdir(parents=True, exist_ok=True)
    command("ip", "link", "set", "lo", "up")
    command("sysctl", "-qw", *TCP)
    router = namespace()
    nodes = [namespace() for _ in range(4)]
    link("gmclient", None, "10.80.0.2/24", router, "10.80.0.1/24")
    command("ip", "route", "add", "10.81.0.0/16", "via", "10.80.0.1")
    command("sysctl", "-qw", "net.ipv4.ip_forward=1", namespace=router)
    for i, node in enumerate(nodes, 1):
        link(f"gm{i}", router, f"10.81.{i}.1/24", node, f"10.81.{i}.2/24")
        command("ip", "route", "add", "default", "via", f"10.81.{i}.1", namespace=node)
    # The browser gets its own configuration directory, never the desktop profile or its launcher flags.
    browser_config = tempfile.TemporaryDirectory()
    env = dict(os.environ, XDG_CONFIG_HOME=browser_config.name)
    env["BUN_CHROME_ARGS"] = f"--no-sandbox --no-proxy-server --ignore-certificate-errors-spki-list={env['GM_E2E_SPKI']} --origin-to-force-quic-on={HOST}:7249"
    repeats = int(env.get("GM_MULTI_BENCH_REPEATS", "2"))
    selected = env.get("GM_MULTI_BENCH_PROFILES", ",".join(PROFILES)).split(",")
    if unknown := set(selected) - {*PROFILES, "matrix"}:
        raise RuntimeError(f"GM_MULTI_BENCH_PROFILES names unknown profiles {sorted(unknown)}")
    if browser := [profile for profile in PROFILES if profile in selected]:
        collection(env, output, router, nodes, browser, repeats)
    if "matrix" in selected:
        matrix(env, output, router, nodes[0], repeats)


try:
    main()
finally:
    stop(backends + keepers)
