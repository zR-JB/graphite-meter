"""Temporary paired Go/Rust campaign: throughput, CPU, peak, post-transfer and idle RSS for every transport.

build: release musl Rust binaries for BASE and the checkout, static Go binaries.
measure server|client: vary that side over go/base/cand against the candidate Rust counterpart.
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
BASE = 'f6307349daf0a1cac067ef69b8ecf81677f034c8'
MUSL = 'x86_64-unknown-linux-musl'
VARIANTS = ('go', 'base', 'cand')
TRANSFERS = [(protocol, direction, transport) for protocol, transport in
             (('http1', 'fetch-stream'), ('http2', 'fetch-stream'), ('http3', 'fetch-stream'),
              ('http3', 'webtransport')) for direction in ('download', 'upload')]
LATENCY = ('websocket', 'webtransport')
REPEATS, STREAMS, SECONDS, IDLE = 5, 4, 10, 3


def run(command: list[str], cwd: Path, environment: dict[str, str], log: Path, timeout: int = 3600) -> None:
    with log.open('w') as output:
        subprocess.run(command, cwd=cwd, env=environment, stdout=output, stderr=subprocess.STDOUT,
                       check=True, timeout=timeout)


def build() -> None:
    BIN.mkdir(parents=True, exist_ok=True)
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('GM_', 'CARGO_PROFILE_', 'MIMALLOC_')) and not key.endswith('RUSTFLAGS')}
    environment.update(CARGO_INCREMENTAL='0', CARGO_TARGET_DIR=str(OUT / 'build'),
                       CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER='x86_64-linux-gnu-gcc',
                       CC_x86_64_unknown_linux_musl='musl-gcc', GM_ENGINE_VERSION='0.0.0-campaign')
    source = OUT / 'base-src'
    if not source.exists():
        subprocess.run(['git', 'worktree', 'add', '--detach', str(source), BASE], cwd=ROOT, check=True)
    run(['rustup', 'target', 'add', MUSL], ROOT / 'rust', environment, OUT / 'target.log')
    for label, tree in (('base', source), ('cand', ROOT)):
        run(['cargo', 'build', '--locked', '--release', '--target', MUSL, '-p', 'graphite-meter-server',
             '-p', 'graphite-meter-client'], tree / 'rust', environment, OUT / f'build-{label}.log')
        for side in ('server', 'client'):
            shutil.copy2(OUT / 'build' / MUSL / 'release' / f'graphite-meter-{side}', BIN / f'{label}-{side}')
    go = {**environment, 'CGO_ENABLED': '0', 'GOTOOLCHAIN': 'local'}
    for side, package in (('server', './cmd/graphite-meter'), ('client', './cmd/graphite-meter-client')):
        run(['go', 'build', '-trimpath', '-o', str(BIN / f'go-{side}'), package], ROOT / 'go', go,
            OUT / f'build-go-{side}.log')


def status(pid: int, key: str) -> int:
    for line in Path(f'/proc/{pid}/status').read_text().splitlines():
        if line.startswith(key + ':'):
            return int(line.split()[1]) * 1024
    return 0


def cpu(pid: int) -> float:
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK')


def start(binary: Path, environment: dict[str, str], port: int, log: Path) -> subprocess.Popen:
    process = subprocess.Popen([str(binary)], env=environment, stdout=log.open('w'), stderr=subprocess.STDOUT,
                               start_new_session=True)
    deadline = time.monotonic() + 10
    while True:
        if process.poll() is not None:
            raise RuntimeError(f'server exited; see {log}')
        try:
            with urllib.request.urlopen(f'http://127.0.0.1:{port}/preflight', timeout=0.5):
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


def client_command(binary: Path, ports: dict[str, int], case: tuple) -> list[str]:
    command = [str(binary), '-report', '-insecure', '-url', f'http://127.0.0.1:{ports["http1"]}']
    if case[0] == 'latency':
        return command + ['-stages', 'latency', '-latency-transport', case[1], '-latency-duration', '4s']
    protocol, direction, transport = case
    scheme = 'http' if protocol == 'http1' else 'https'
    return command + ['-stages', direction, '-streams', str(STREAMS),
                      '-throughput-origin', f'{scheme}://127.0.0.1:{ports[protocol]}',
                      '-throughput-protocol', protocol, '-throughput-transport', transport,
                      '-loaded-latency=false', '-warmup', '500ms', f'-{direction}-duration', f'{SECONDS}s']


def parse(case: tuple, output: str) -> dict:
    if case[0] == 'latency':
        probes = re.search(r'Idle\s+(.+?)\s{2,}(.+?)\s{2,}(.+?)\s{2,}[\d,]+ / ([\d,]+)', output)
        if not probes:
            return {}
        return {'median': probes[1], 'p95': probes[2], 'jitter': probes[3],
                'probesPerSecond': int(probes[4].replace(',', '')) / 4}
    rate = re.search(r'(?:Download|Upload)\s+([\d.]+) (Gbit|Mbit)/s', output)
    if not rate:
        return {}
    return {'gbps': float(rate[1]) / (1 if rate[2] == 'Gbit' else 1000)}


def measure(study: str) -> None:
    results = OUT / f'results-{study}'
    results.mkdir(parents=True, exist_ok=True)
    fixture = Fixture('campaign-')
    _, cert, key = fixture.identity()
    ports = {'http1': unused_port(), 'http2': unused_port(), 'http3': unused_port(socket.SOCK_DGRAM)}
    environment = {key: value for key, value in os.environ.items() if not key.startswith(('GM_', 'MIMALLOC_'))}
    environment.update(GM_AUTH_MODE='off', GM_H1_ADDR=f'127.0.0.1:{ports["http1"]}',
                       GM_H2_ADDR=f'127.0.0.1:{ports["http2"]}', GM_H3_ADDR=f'127.0.0.1:{ports["http3"]}',
                       GM_TLS_CERT=str(cert), GM_TLS_KEY=str(key))
    cases = TRANSFERS + [('latency', transport) for transport in LATENCY]
    rows = []
    with (results / 'runs.jsonl').open('w') as sink:
        for repeat in range(REPEATS):
            order = VARIANTS[repeat % 3:] + VARIANTS[:repeat % 3]
            for index in range(len(cases)):
                case = cases[(index + repeat) % len(cases)]
                for variant in order:
                    server = BIN / (f'{variant}-server' if study == 'server' else 'cand-server')
                    client = BIN / (f'{variant}-client' if study == 'client' else 'cand-client')
                    cell = f'{study}-{variant}-{"-".join(case)}-{repeat}'
                    process = start(server, environment, ports['http1'], results / f'{cell}-server.log')
                    try:
                        before, begin = cpu(process.pid), status(process.pid, 'VmRSS')
                        timed = subprocess.run(['/usr/bin/time', '-f', 'GM_TIME %M %U %S', *client_command(
                            client, ports, case)], env=environment, capture_output=True, text=True, timeout=60)
                        after = cpu(process.pid)
                        end, peak = status(process.pid, 'VmRSS'), status(process.pid, 'VmHWM')
                        time.sleep(IDLE)
                        idle = status(process.pid, 'VmRSS')
                    finally:
                        stop(process)
                    output = timed.stdout + timed.stderr
                    (results / f'{cell}-client.log').write_text(output)
                    clock = re.search(r'GM_TIME (\d+) ([\d.]+) ([\d.]+)', output)
                    row = {'study': study, 'variant': variant, 'case': list(case), 'repeat': repeat,
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
    build() if sys.argv[1] == 'build' else measure(sys.argv[2])
