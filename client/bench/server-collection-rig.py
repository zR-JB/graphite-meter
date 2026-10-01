"""Clients, a router and real servers inside disposable user, network and PID namespaces.

Run through server-collection.sh. No host interface or qdisc is changed. The browser control
channel stays on unshaped client loopback. Results are written under the temporary directory;
the caller supplies an already built Go server and pinned Chrome. Profiles server-cap,
differing-rtt and shared-cap drive the browser UI against four coordinated Go servers.
"""

import contextlib
import json
import os
from pathlib import Path
import queue
import signal
import threading
import subprocess
import tempfile
import time


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


def shape(owner, iface, delay_ms, mbps):
    args = ["tc", "qdisc", "replace", "dev", iface, "root", "netem", "limit", "200000", "delay", f"{delay_ms:g}ms"]
    if mbps:
        args += ["rate", f"{mbps}mbit"]
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
            stop([process])


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
    # The PID namespace's init inherits the processes a browser leaves behind and must collect them.
    with contextlib.suppress(ChildProcessError):
        while os.waitpid(-1, os.WNOHANG)[0]:
            pass


def output_directory():
    """GM_MULTI_BENCH_OUTPUT, resolved through links; it must lie inside the temporary directory."""
    path = os.path.realpath(os.environ["GM_MULTI_BENCH_OUTPUT"])
    if path.startswith(os.path.join(os.path.realpath(tempfile.gettempdir()), "")):
        return Path(path)
    raise RuntimeError("GM_MULTI_BENCH_OUTPUT must be inside the temporary directory")


def main():
    # As its PID namespace's init the rig would otherwise ignore SIGTERM.
    signal.signal(signal.SIGTERM, signal.default_int_handler)
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
    # The browser gets its own configuration and temporary files, never the desktop profile or its launcher flags;
    # /dev/shm keeps its socket paths within the 108-byte limit.
    scratch = tempfile.TemporaryDirectory(dir="/dev/shm")
    env = dict(os.environ, XDG_CONFIG_HOME=scratch.name, TMPDIR=scratch.name)
    env["BUN_CHROME_ARGS"] = f"--no-sandbox --no-proxy-server --ignore-certificate-errors-spki-list={env['GM_E2E_SPKI']}"
    repeats = int(env.get("GM_MULTI_BENCH_REPEATS", "2"))
    selected = env.get("GM_MULTI_BENCH_PROFILES", ",".join(PROFILES)).split(",")
    if unknown := set(selected) - PROFILES.keys():
        raise RuntimeError(f"GM_MULTI_BENCH_PROFILES names unknown profiles {sorted(unknown)}")
    if browser := [profile for profile in PROFILES if profile in selected]:
        collection(env, output, router, nodes, browser, repeats)


try:
    main()
finally:
    stop(backends + keepers)
