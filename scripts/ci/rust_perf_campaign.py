"""Temporary paired Go/Rust campaign: throughput, CPU, peak, post-transfer and idle RSS for every transport.

build: release musl Rust binaries for BASE and the checkout, static Go binaries.
measure server|client: vary that side over go/base/cand against the candidate Rust counterpart on loopback.
measure wan-server|wan-client PROFILE: the same across one shaped link, with loaded latency (run as root).
profile: perf of the symbolized candidate and of Go on both sides of HTTP/1.1 transfers.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import time
import urllib.request

from rust.tests.process_fixture import Fixture, unused_port

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'rust/target/campaign'
BIN = OUT / 'bin'
BASE = '4b4ce453ea94dccdd46e6d83f7f6912c6abf76bd'
MUSL = 'x86_64-unknown-linux-musl'
VARIANTS = ('go', 'base', 'cand')
TRANSFERS = [(protocol, direction, transport) for protocol, transport in
             (('http1', 'fetch-stream'), ('http2', 'fetch-stream'), ('http3', 'fetch-stream'),
              ('http3', 'webtransport')) for direction in ('download', 'upload')]
LATENCY = ('websocket', 'webtransport')
REPEATS, STREAMS, SECONDS, IDLE = 5, 4, 10, 3
# Round trip ms, delay variation ms each way, Mbit/s each way, loss % each way; the queue limit is sized for
# 100 ms of full-size packets.
PROFILES = {'cable': (30, 0, 300, 0), 'far': (200, 0, 1000, 0), 'wifi': (20, 5, 150, 0.5), 'mobile': (70, 20, 30, 2)}
SERVER_ADDRESS, CLIENT_ADDRESS, CLIENT_NS = '10.77.0.1', '10.77.0.2', 'gm-client'
# Linux's default congestion control, with TCP buffers that never limit a 1 Gbit/s, 300 ms path.
TCP = ['net.ipv4.tcp_congestion_control=cubic', 'net.ipv4.tcp_rmem=4096 131072 134217728',
       'net.ipv4.tcp_wmem=4096 16384 134217728']


def run(command: list[str], cwd: Path, environment: dict[str, str], log: Path, timeout: int = 3600) -> None:
    with log.open('w') as output:
        status = subprocess.run(command, cwd=cwd, env=environment, stdout=output, stderr=subprocess.STDOUT,
                                timeout=timeout).returncode
    if status:
        sys.exit(f'{command} failed with {status}:\n' + '\n'.join(log.read_text().splitlines()[-80:]))


def build() -> None:
    BIN.mkdir(parents=True, exist_ok=True)
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('GM_', 'CARGO_PROFILE_', 'MIMALLOC_')) and not key.endswith('RUSTFLAGS')}
    environment.update(CARGO_INCREMENTAL='0',
                       CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER='x86_64-linux-gnu-gcc',
                       CC_x86_64_unknown_linux_musl='musl-gcc', GM_ENGINE_VERSION='0.0.0-campaign')
    source = OUT / 'base-src'
    if not source.exists():
        subprocess.run(['git', 'worktree', 'add', '--detach', str(source), BASE], cwd=ROOT, check=True)
    run(['rustup', 'target', 'add', MUSL], ROOT / 'rust', environment, OUT / 'target.log')
    symbols = {'CARGO_PROFILE_RELEASE_DEBUG': '2', 'CARGO_PROFILE_RELEASE_STRIP': 'none'}
    for label, tree, extra in (('base', source, {}), ('cand', ROOT, {}), ('prof', ROOT, symbols)):
        # Legal notice embedding requires each workspace's build outputs under its own target directory.
        target = tree / f'rust/target/campaign-build-{label}'
        run(['cargo', 'build', '--locked', '--release', '--target', MUSL, '-p', 'graphite-meter-server',
             '-p', 'graphite-meter-client'], tree / 'rust', {**environment, **extra, 'CARGO_TARGET_DIR': str(target)},
            OUT / f'build-{label}.log')
        for side in ('server', 'client'):
            shutil.copy2(target / MUSL / 'release' / f'graphite-meter-{side}', BIN / f'{label}-{side}')
    go = {**environment, 'CGO_ENABLED': '0', 'GOTOOLCHAIN': 'local'}
    for side, package in (('server', './cmd/graphite-meter'), ('client', './cmd/graphite-meter-client')):
        run(['go', 'build', '-trimpath', '-o', str(BIN / f'go-{side}'), package], ROOT / 'go', go,
            OUT / f'build-go-{side}.log')


def shape(profile: str) -> list[str]:
    """Server in this namespace, client in CLIENT_NS, one veth pair shaped in both directions."""
    rtt, variation, mbit, loss = PROFILES[profile]
    inside = ['ip', 'netns', 'exec', CLIENT_NS]
    sh = lambda *command: subprocess.run(command, check=True)
    sh('ip', 'netns', 'add', CLIENT_NS)
    sh('ip', 'link', 'add', 'gm-server', 'type', 'veth', 'peer', 'name', 'gm-peer', 'netns', CLIENT_NS)
    limit = max(1000, int(mbit * 1e6 / 8 * ((rtt / 2 + variation) / 1000 + 0.1) / 1500))
    for prefix, iface, address in (([], 'gm-server', SERVER_ADDRESS), (inside, 'gm-peer', CLIENT_ADDRESS)):
        # netem must see single packets; batches would be delayed and dropped whole.
        sh(*prefix, 'ethtool', '-K', iface, 'tso', 'off', 'gso', 'off', 'gro', 'off', 'tx-udp-segmentation', 'off')
        sh(*prefix, 'ip', 'addr', 'add', f'{address}/24', 'dev', iface)
        sh(*prefix, 'ip', 'link', 'set', iface, 'up')
        netem = ['delay', f'{rtt / 2:g}ms'] + ([f'{variation}ms'] if variation else []) + ['rate', f'{mbit}mbit']
        sh(*prefix, 'tc', 'qdisc', 'replace', 'dev', iface, 'root', 'netem', 'limit', str(limit), *netem,
           *(['loss', f'{loss}%'] if loss else []))
        sh(*prefix, 'sysctl', '-qw', *TCP)
    for key in ('net.core.rmem_max', 'net.core.wmem_max', 'net.ipv4.tcp_rmem'):
        sh(*inside, 'sysctl', key)
    sh(*inside, 'ping', '-c', '5', '-i', '0.2', SERVER_ADDRESS)
    return inside


def status(pid: int, key: str) -> int:
    for line in Path(f'/proc/{pid}/status').read_text().splitlines():
        if line.startswith(key + ':'):
            return int(line.split()[1]) * 1024
    return 0


def cpu(pid: int) -> float:
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK')


def start(binary: Path, environment: dict[str, str], address: str, log: Path) -> subprocess.Popen:
    process = subprocess.Popen([str(binary)], env=environment, stdout=log.open('w'), stderr=subprocess.STDOUT,
                               start_new_session=True)
    deadline = time.monotonic() + 10
    while True:
        if process.poll() is not None:
            raise RuntimeError(f'server exited; see {log}')
        try:
            with urllib.request.urlopen(f'http://{address}/preflight', timeout=0.5):
                return process
        except OSError:
            if time.monotonic() >= deadline:
                raise TimeoutError(f'server startup timed out; see {log}')
            time.sleep(0.05)


def stop(process: subprocess.Popen) -> None:
    process.send_signal(signal.SIGINT)
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)


def client_command(binary: Path, ports: dict[str, int], case: tuple, host: str) -> list[str]:
    command = [str(binary), '-report', '-insecure', '-url', f'http://{host}:{ports["http1"]}']
    if case[0] == 'latency':
        return command + ['-stages', 'latency', '-latency-transport', case[1], '-latency-duration', '4s']
    protocol, direction, transport = case
    scheme = 'http' if protocol == 'http1' else 'https'
    command += ['-stages', direction, '-streams', str(STREAMS), '-throughput-origin',
                f'{scheme}://{host}:{ports[protocol]}', '-throughput-protocol', protocol,
                '-throughput-transport', transport, f'-{direction}-duration', f'{SECONDS}s']
    # Shaped runs keep the default warmup and measure loaded latency, as users run.
    return command + (['-loaded-latency=false', '-warmup', '500ms'] if host == '127.0.0.1' else [])


def parse(case: tuple, output: str) -> dict:
    if case[0] == 'latency':
        probes = re.search(r'Idle\s+(.+?)\s{2,}(.+?)\s{2,}(.+?)\s{2,}[\d,]+ / ([\d,]+)', output)
        if not probes:
            return {}
        return {'median': probes[1], 'p95': probes[2], 'jitter': probes[3],
                'probesPerSecond': int(probes[4].replace(',', '')) / 4}
    rate = re.search(r'(?:Download|Upload)\s+([\d.]+) (Gbit|Mbit|kbit)/s', output)
    if not rate:
        return {}
    result = {'gbps': float(rate[1]) / {'Gbit': 1, 'Mbit': 1e3, 'kbit': 1e6}[rate[2]]}
    loaded = re.search(r'^Loaded (?:down|up)\s{2,}(.+)$', output, re.MULTILINE)
    if loaded:
        median, _added, p95, _jitter, timeouts = re.split(r'\s{2,}', loaded[1].strip())[:5]
        # A dash stands for a figure no probe answered.
        ms = lambda text: float(text.split()[0]) * (1e3 if text.endswith(' s') else 1) if text[0].isdigit() else None
        result.update(loadedMedianMs=ms(median), loadedP95Ms=ms(p95), probeTimeouts=timeouts)
    return result


def measure(study: str, profile: str | None) -> None:
    results = OUT / (f'results-{study}' + (f'-{profile}' if profile else ''))
    results.mkdir(parents=True, exist_ok=True)
    fixture = Fixture('campaign-')
    _, cert, key = fixture.identity()
    ports = {'http1': unused_port(), 'http2': unused_port(), 'http3': unused_port(socket.SOCK_DGRAM)}
    host, inside = (SERVER_ADDRESS, shape(profile)) if profile else ('127.0.0.1', [])
    environment = {key: value for key, value in os.environ.items() if not key.startswith(('GM_', 'MIMALLOC_'))}
    environment.update(GM_AUTH_MODE='off', GM_H1_ADDR=f'{host}:{ports["http1"]}',
                       GM_H2_ADDR=f'{host}:{ports["http2"]}', GM_H3_ADDR=f'{host}:{ports["http3"]}',
                       GM_TLS_CERT=str(cert), GM_TLS_KEY=str(key))
    side = study.removeprefix('wan-')
    cases = TRANSFERS + ([] if profile else [('latency', transport) for transport in LATENCY])
    rows = []
    with (results / 'runs.jsonl').open('w') as sink:
        for repeat in range(REPEATS):
            order = VARIANTS[repeat % 3:] + VARIANTS[:repeat % 3]
            for index in range(len(cases)):
                case = cases[(index + repeat) % len(cases)]
                for variant in order:
                    server = BIN / (f'{variant}-server' if side == 'server' else 'cand-server')
                    client = BIN / (f'{variant}-client' if side == 'client' else 'cand-client')
                    cell = f'{study}-{variant}-{"-".join(case)}-{repeat}'
                    process = start(server, environment, environment['GM_H1_ADDR'], results / f'{cell}-server.log')
                    try:
                        before, begin = cpu(process.pid), status(process.pid, 'VmRSS')
                        timed = subprocess.run([*inside, '/usr/bin/time', '-f', 'GM_TIME %M %U %S', *client_command(
                            client, ports, case, host)], env=environment, capture_output=True, text=True, timeout=120)
                        after = cpu(process.pid)
                        end, peak = status(process.pid, 'VmRSS'), status(process.pid, 'VmHWM')
                        time.sleep(IDLE)
                        idle = status(process.pid, 'VmRSS')
                    finally:
                        stop(process)
                    output = timed.stdout + timed.stderr
                    (results / f'{cell}-client.log').write_text(output)
                    clock = re.search(r'GM_TIME (\d+) ([\d.]+) ([\d.]+)', output)
                    row = {'study': study, 'profile': profile, 'variant': variant, 'case': list(case), 'repeat': repeat,
                           'exit': timed.returncode, **parse(case, output),
                           'server': {'startRss': begin, 'peakRss': peak, 'endRss': end, 'idleRss': idle,
                                      'cpuSeconds': round(after - before, 2)},
                           'client': {'peakRss': int(clock[1]) * 1024 if clock else None,
                                      'cpuSeconds': round(float(clock[2]) + float(clock[3]), 2) if clock else None}}
                    rows.append(row)
                    sink.write(json.dumps(row) + '\n')
                    sink.flush()
                    print(json.dumps(row), flush=True)
    summarize(rows, results / 'summary.json')
    shutil.rmtree(fixture.directory, ignore_errors=True)


def profile() -> None:
    results = OUT / 'results-profile'
    results.mkdir(parents=True, exist_ok=True)
    fixture = Fixture('campaign-')
    _, cert, key = fixture.identity()
    ports = {'http1': unused_port(), 'http2': unused_port(), 'http3': unused_port(socket.SOCK_DGRAM)}
    environment = {key: value for key, value in os.environ.items() if not key.startswith(('GM_', 'MIMALLOC_'))}
    environment.update(GM_AUTH_MODE='off', GM_H1_ADDR=f'127.0.0.1:{ports["http1"]}',
                       GM_H2_ADDR=f'127.0.0.1:{ports["http2"]}', GM_H3_ADDR=f'127.0.0.1:{ports["http3"]}',
                       GM_TLS_CERT=str(cert), GM_TLS_KEY=str(key))
    # The profiled side is varied; its counterpart is the candidate.
    cells = [('client', 'download', variant) for variant in ('prof', 'go')]
    cells += [('server', 'upload', variant) for variant in ('prof', 'go')]
    for side, direction, variant in cells:
        for repeat in range(2):
            name = f'profile-{side}-{direction}-{variant}-{repeat}'
            server = BIN / (f'{variant}-server' if side == 'server' else 'cand-server')
            client = BIN / (f'{variant}-client' if side == 'client' else 'cand-client')
            process = start(server, environment, environment['GM_H1_ADDR'], results / f'{name}-server.log')
            try:
                running = subprocess.Popen(client_command(client, ports, ('http1', direction, 'fetch-stream'),
                                                          '127.0.0.1'), env=environment, stdout=subprocess.PIPE,
                                           stderr=subprocess.STDOUT, text=True)
                time.sleep(2)
                pid = running.pid if side == 'client' else process.pid
                data = results / f'{name}.data'
                subprocess.run(['sudo', '-n', 'perf', 'record', '-F', '999', '-g', '-p', str(pid), '-o', str(data),
                                '--', 'sleep', '6'], capture_output=True, timeout=30, check=False)
                output = running.communicate(timeout=60)[0]
            finally:
                stop(process)
            report = subprocess.run(['sudo', '-n', 'perf', 'report', '-i', str(data), '--stdio', '--no-children',
                                     '--sort', 'dso,symbol', '--percent-limit', '0.4', '-g', 'none'],
                                    capture_output=True, text=True, timeout=300, check=False).stdout
            syscalls = subprocess.run(['sudo', '-n', 'perf', 'report', '-i', str(data), '--stdio', '--no-children',
                                       '--sort', 'symbol', '--percent-limit', '0.4', '-g', 'caller,0.5,callee',
                                       '--symbol-filter', 'entry_SYSCALL'],
                                      capture_output=True, text=True, timeout=300, check=False).stdout
            rate = re.search(r'(?:Download|Upload)\s+[^\n]*', output)
            (results / f'{name}.txt').write_text((rate[0] if rate else output[-500:]) + '\n' + report + syscalls)
            data.unlink(missing_ok=True)
    shutil.rmtree(fixture.directory, ignore_errors=True)


def summarize(rows: list[dict], path: Path) -> None:
    mib = 2 ** 20
    summary = {}
    for row in rows:
        summary.setdefault(' '.join(row['case']), {}).setdefault(row['variant'], []).append(row)
    table = {}
    for case, variants in summary.items():
        for variant, runs in variants.items():
            ok = [run for run in runs if run['exit'] == 0 and ('gbps' in run or 'probesPerSecond' in run)]
            median = lambda pick: round(statistics.median(pick(run) for run in ok), 2) if ok else None
            table.setdefault(case, {})[variant] = {
                'ok': f'{len(ok)}/{len(runs)}',
                'gbps': median(lambda run: run.get('gbps', 0)),
                'loadedMedianMs': median(lambda run: run.get('loadedMedianMs') or 0),
                'loadedP95Ms': median(lambda run: run.get('loadedP95Ms') or 0),
                'probesPerSecond': median(lambda run: run.get('probesPerSecond', 0)),
                'serverPeakMiB': median(lambda run: run['server']['peakRss'] / mib),
                'serverEndMiB': median(lambda run: run['server']['endRss'] / mib),
                'serverIdleMiB': median(lambda run: run['server']['idleRss'] / mib),
                'serverCpu': median(lambda run: run['server']['cpuSeconds']),
                'clientPeakMiB': median(lambda run: (run['client']['peakRss'] or 0) / mib),
                'clientCpu': median(lambda run: run['client']['cpuSeconds'] or 0)}
    path.write_text(json.dumps(table, indent=1) + '\n')
    for case, variants in table.items():
        print(case, json.dumps(variants))


if __name__ == '__main__':
    if sys.argv[1] == 'build':
        build()
    elif sys.argv[1] == 'profile':
        profile()
    else:
        measure(sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else None)
