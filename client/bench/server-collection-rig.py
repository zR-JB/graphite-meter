"""One client, one router and four real servers inside disposable user/network namespaces.

Run through unshare --user --map-root-user --net. No host interface or qdisc is changed.
The browser control channel stays on unshaped client loopback. Results are written
outside the checkout; the caller supplies an already built server and pinned Chrome.
"""

import json
import os
from pathlib import Path
import queue
import threading
import subprocess
import sys
import time


def command(*args, namespace=None, **kwargs):
    prefix = ["nsenter", "-t", str(namespace), "-n", "--"] if namespace else []
    return subprocess.run(prefix + list(args), check=True, **kwargs)


keepers = []
backends = []


def namespace():
    child = subprocess.Popen(["unshare", "--net", "sleep", "1800"])
    keepers.append(child)
    for _ in range(100):
        if os.readlink(f"/proc/{child.pid}/ns/net") != os.readlink("/proc/self/ns/net"):
            command("ip", "link", "set", "lo", "up", namespace=child.pid)
            return child.pid
        time.sleep(0.01)
    raise RuntimeError("network namespace did not start")


def link(name, a_namespace, a_address, b_namespace, b_address):
    other = name + "p"
    command("ip", "link", "add", name, "type", "veth", "peer", "name", other)
    for iface, owner, address in [(name, a_namespace, a_address), (other, b_namespace, b_address)]:
        if owner:
            command("ip", "link", "set", iface, "netns", str(owner))
        command("ip", "addr", "add", address, "dev", iface, namespace=owner)
        command("ip", "link", "set", iface, "up", namespace=owner)


def shape(owner, iface, delay_ms, mbps):
    args = ["tc", "qdisc", "replace", "dev", iface, "root", "netem", "limit", "200000", "delay", f"{delay_ms}ms"]
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
            pass
    descendants = {parent}
    while True:
        found = {pid for pid, (ppid, *_rest) in processes.items() if ppid in descendants}
        if found <= descendants:
            break
        descendants |= found
    return {pid: (cpu, rss) for pid, (_ppid, name, cpu, rss) in processes.items() if pid in descendants and name.startswith("chrome")}


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


def main():
    mapping = Path("/proc/self/uid_map").read_text().split()
    if os.getuid() != 0 or len(mapping) != 3 or mapping[0] != "0" or mapping[1] == "0" or mapping[2] != "1":
        raise RuntimeError("Run inside a disposable unprivileged user/network namespace")
    output = Path(os.environ["GM_MULTI_BENCH_OUTPUT"]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    command("ip", "link", "set", "lo", "up")
    router = namespace()
    nodes = [namespace() for _ in range(4)]
    link("gmclient", None, "10.80.0.2/24", router, "10.80.0.1/24")
    command("ip", "route", "add", "10.81.0.0/16", "via", "10.80.0.1")
    command("sysctl", "-qw", "net.ipv4.ip_forward=1", namespace=router)
    servers = [{"id": "self" if i == 1 else f"server-{i}", "url": f"https://10.81.{i}.2:7247", "name": f"Path {i}"} for i in range(1, 5)]
    env = {key: value for key, value in os.environ.items() if not key.startswith("GM_AUTH_") and not key.startswith("GM_SERVER_CATALOG")}
    for i, node in enumerate(nodes, 1):
        link(f"gm{i}", router, f"10.81.{i}.1/24", node, f"10.81.{i}.2/24")
        command("ip", "route", "add", "default", "via", f"10.81.{i}.1", namespace=node)
        backend_env = {**env, "GM_AUTH_MODE": "off", "GM_H1_ADDR": "0.0.0.0:7246", "GM_H1_TLS_ADDR": "0.0.0.0:7247", "GM_H2_ADDR": "", "GM_H3_ADDR": "", "GM_TLS_CERT": env["GM_E2E_TLS_CERT"], "GM_TLS_KEY": env["GM_E2E_TLS_KEY"], "GM_SERVER_NAME": f"Path {i}"}
        if i == 1:
            backend_env["GM_SERVER_CATALOG"] = json.dumps({"servers": servers[1:]})
        log = (output / f"server-{i}.log").open("w")
        backend = subprocess.Popen(["nsenter", "-t", str(node), "-n", "--", env["GM_E2E_SERVER_BIN"]], env=backend_env, stdout=log, stderr=log)
        backends.append(backend)
        log.close()
    env.update(GM_MULTI_BENCH_SERVERS=json.dumps(servers), BUN_CHROME_ARGS=f"--no-sandbox --no-proxy-server --ignore-certificate-errors-spki-list={env['GM_E2E_SPKI']}")
    time.sleep(0.5)
    command("curl", "--noproxy", "*", "--fail", "--silent", "--max-time", "3", "http://10.81.1.2:7246/preflight", stdout=subprocess.DEVNULL)
    if any(server.poll() is not None for server in backends):
        raise RuntimeError("a measurement server failed to start")
    repeats = int(env.get("GM_MULTI_BENCH_REPEATS", "2"))
    profiles = env.get("GM_MULTI_BENCH_PROFILES", "server-cap,differing-rtt,shared-cap").split(",")
    for profile in profiles:
        shape(router, "gmclientp", 0.2, 100 if profile == "shared-cap" else 0)
        for i, node in enumerate(nodes, 1):
            delay = [2.5, 10, 25, 50][i - 1] if profile == "differing-rtt" else 1
            capacity = [40, 60, 80, 100][i - 1] if profile == "server-cap" else 80 if profile == "differing-rtt" else 200
            shape(node, f"gm{i}p", delay, capacity)
            shape(router, f"gm{i}", delay, 0)
        for repeat in range(1, repeats + 1):
            for count in ([4, 1, 2] if repeat % 2 else [2, 4, 1]):
                run_cell(env, output, profile, count, repeat)


try:
    main()
finally:
    for process in backends + keepers:
        if process.poll() is None:
            process.terminate()
    for process in backends + keepers:
        process.wait(timeout=5)
