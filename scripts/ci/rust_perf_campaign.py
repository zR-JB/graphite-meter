"""Temporary paired campaign of candidate knobs, each run full-stack against the candidate, plus a CPU profile.

build: release musl binaries of the checkout and of each variant (VARIANTS edits applied to a copy).
measure: each variant's server and client together over the transports it changes.
profile: perf of a symbolized candidate server during WebTransport and HTTP/3 uploads.
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
MUSL = 'x86_64-unknown-linux-musl'
NOQ_COPY = '5c12aca28'
QUIC = [('http3', d, t) for t in ('fetch-stream', 'webtransport') for d in ('download', 'upload')]
H2 = [('http2', d, 'fetch-stream') for d in ('download', 'upload')]
JUMBO = """{ let mut c = noq::EndpointConfig::default(); c.max_udp_payload_size(8952).expect("payload in range"); c }"""
MTU = """transport.mtu_discovery_config(Some({ let mut m = noq::MtuDiscoveryConfig::default(); m.upper_bound(8952); m }));"""
# Each variant: (cases it is measured on, [(file, old, new)] source edits).
VARIANTS = {
    'cand': (H2 + QUIC, []),
    'h2': (H2, [
        ('rust/server/src/http/http2.rs', 'const FRAME_BYTES: usize = 16 * 1024;', 'const FRAME_BYTES: usize = 64 * 1024;'),
        ('rust/server/src/http/http2.rs', '.max_send_buffer_size(FRAME_BYTES)', '.max_send_buffer_size(4 * FRAME_BYTES)'),
        ('rust/client/src/net.rs', '.timer(TokioTimer::new())', '.timer(TokioTimer::new())\n                .max_frame_size(64 * 1024)'),
    ]),
    'mtu': (QUIC, [
        ('rust/server/src/http/quic.rs', 'noq::EndpointConfig::default()', JUMBO),
        ('rust/server/src/budget.rs', '    let mut transport = noq::TransportConfig::default();',
         '    let mut transport = noq::TransportConfig::default();\n    ' + MTU),
        ('rust/client/src/quic.rs', 'quinn::EndpointConfig::default()', JUMBO.replace('noq::', 'quinn::')),
        ('rust/client/src/quic.rs', '        let mut transport = quinn::TransportConfig::default();',
         '        let mut transport = quinn::TransportConfig::default();\n        ' + MTU.replace('noq::', 'quinn::')),
    ]),
    'copy': (QUIC, 'noq'),
}
REPEATS, STREAMS, SECONDS, IDLE = 5, 4, 10, 3


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
    environment.update(CARGO_INCREMENTAL='0', CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER='x86_64-linux-gnu-gcc',
                       CC_x86_64_unknown_linux_musl='musl-gcc', GM_ENGINE_VERSION='0.0.0-campaign')
    run(['rustup', 'target', 'add', MUSL], ROOT / 'rust', environment, OUT / 'target.log')
    builds = [(name, edits, {}) for name, (_, edits) in VARIANTS.items()]
    builds.append(('prof', [], {'CARGO_PROFILE_RELEASE_DEBUG': '2', 'CARGO_PROFILE_RELEASE_STRIP': 'none'}))
    for label, edits, extra in builds:
        tree = OUT / f'src-{label}'
        if not tree.exists():
            subprocess.run(['git', 'worktree', 'add', '--detach', str(tree), 'HEAD'], cwd=ROOT, check=True)
        if edits == 'noq':
            pin = re.search(r'zR-JB/noq", rev = "([0-9a-f]{40})"', (tree / 'rust/Cargo.toml').read_text())
            assert pin, 'noq pin'
            pinned = pin[1]
            full = subprocess.run(['git', 'ls-remote', 'https://github.com/zR-JB/noq',
                                   'refs/heads/graphite-meter/experiment-receive-copy'],
                                  capture_output=True, text=True, check=True).stdout.split()[0]
            assert full.startswith(NOQ_COPY)
            for path in ('rust/Cargo.toml', 'rust/Cargo.lock'):
                (tree / path).write_text((tree / path).read_text().replace(pinned, full))
        else:
            for path, old, new in edits:
                text = (tree / path).read_text()
                assert old in text, (label, path, old)
                (tree / path).write_text(text.replace(old, new))
        # Legal notice embedding requires each workspace's build outputs under its own target directory.
        target = tree / 'rust/target/campaign-build'
        run(['cargo', 'build', '--locked', '--release', '--target', MUSL, '-p', 'graphite-meter-server',
             '-p', 'graphite-meter-client'], tree / 'rust', {**environment, **extra, 'CARGO_TARGET_DIR': str(target)},
            OUT / f'build-{label}.log')
        for side in ('server', 'client'):
            shutil.copy2(target / MUSL / 'release' / f'graphite-meter-{side}', BIN / f'{label}-{side}')


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


def environment_for(fixture: Fixture) -> tuple[dict[str, str], dict[str, int]]:
    _, cert, key = fixture.identity()
    ports = {'http1': unused_port(), 'http2': unused_port(), 'http3': unused_port(socket.SOCK_DGRAM)}
    environment = {key: value for key, value in os.environ.items() if not key.startswith(('GM_', 'MIMALLOC_'))}
    environment.update(GM_AUTH_MODE='off', GM_H1_ADDR=f'127.0.0.1:{ports["http1"]}',
                       GM_H2_ADDR=f'127.0.0.1:{ports["http2"]}', GM_H3_ADDR=f'127.0.0.1:{ports["http3"]}',
                       GM_TLS_CERT=str(cert), GM_TLS_KEY=str(key))
    return environment, ports


def transfer(name: str, server: Path, client: Path, environment: dict[str, str], ports: dict[str, int],
             case: tuple, results: Path, during=None) -> dict:
    process = start(server, environment, ports['http1'], results / f'{name}-server.log')
    try:
        before, begin = cpu(process.pid), status(process.pid, 'VmRSS')
        command = ['/usr/bin/time', '-f', 'GM_TIME %M %U %S', *client_command(client, ports, case)]
        running = subprocess.Popen(command, env=environment, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        if during:
            during(process.pid)
        try:
            output, _ = running.communicate(timeout=90)
        except subprocess.TimeoutExpired:
            running.kill()
            output = running.communicate()[0] + '\nGM_TIMEOUT'
        after = cpu(process.pid)
        end, peak = status(process.pid, 'VmRSS'), status(process.pid, 'VmHWM')
        time.sleep(IDLE)
        idle = status(process.pid, 'VmRSS')
    finally:
        stop(process)
    (results / f'{name}-client.log').write_text(output)
    clock = re.search(r'GM_TIME (\d+) ([\d.]+) ([\d.]+)', output)
    return {'case': list(case), 'exit': running.returncode, **parse(case, output),
            'server': {'startRss': begin, 'peakRss': peak, 'endRss': end, 'idleRss': idle,
                       'cpuSeconds': round(after - before, 2)},
            'client': {'peakRss': int(clock[1]) * 1024 if clock else None,
                       'cpuSeconds': round(float(clock[2]) + float(clock[3]), 2) if clock else None}}


def measure() -> None:
    results = OUT / 'results-variants'
    results.mkdir(parents=True, exist_ok=True)
    fixture = Fixture('campaign-')
    environment, ports = environment_for(fixture)
    cases = list(dict.fromkeys(case for cases, _ in VARIANTS.values() for case in cases))
    names = list(VARIANTS)
    rows = []
    with (results / 'runs.jsonl').open('w') as sink:
        for repeat in range(REPEATS):
            order = names[repeat % len(names):] + names[:repeat % len(names)]
            for index in range(len(cases)):
                case = cases[(index + repeat) % len(cases)]
                for variant in (name for name in order if case in VARIANTS[name][0]):
                    row = {'study': 'variants', 'variant': variant, 'repeat': repeat, **transfer(
                        f'{variant}-{"-".join(case)}-{repeat}', BIN / f'{variant}-server', BIN / f'{variant}-client',
                        environment, ports, case, results)}
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
    environment, ports = environment_for(fixture)
    for case in (('http3', 'upload', 'webtransport'), ('http3', 'upload', 'fetch-stream')):
        for repeat in range(2):
            name = f'profile-{"-".join(case)}-{repeat}'
            data = results / f'{name}.data'
            def record(pid: int) -> None:
                time.sleep(1.5)
                subprocess.run(['sudo', '-n', 'perf', 'record', '-F', '999', '-p', str(pid), '-o', str(data),
                                '--', 'sleep', '8'], capture_output=True, timeout=30)
            row = transfer(name, BIN / 'prof-server', BIN / 'cand-client', environment, ports, case, results, record)
            print(json.dumps(row), flush=True)
            report = subprocess.run(['sudo', '-n', 'perf', 'report', '-i', str(data), '--stdio', '--no-children',
                                     '--sort', 'dso,symbol', '--percent-limit', '0.4'],
                                    capture_output=True, text=True, timeout=300).stdout
            (results / f'{name}.txt').write_text(json.dumps(row) + '\n' + report)
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
    {'build': build, 'measure': measure, 'profile': profile}[sys.argv[1]]()
