"""Temporary hosted Go/Rust server comparison; binaries and TLS keys are never uploaded."""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import sys
import time
import tomllib
import urllib.request

from rust.tests.process_fixture import Fixture, unused_port

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / 'rust/target/allocator-study-results'
MUSL = 'x86_64-unknown-linux-musl'
GNU = 'x86_64-unknown-linux-gnu'
VARIANTS = ('go-static', 'musl-mimalloc')
CASES = (('http1', 'upload', 4), ('http1-tls', 'upload', 4),
         ('http2', 'upload', 4), ('http3', 'upload', 4))
ALLOCATOR = b'#[cfg(target_env = "musl")]\n#[global_allocator]\nstatic ALLOCATOR: rustfs_mimalloc::MiMalloc = rustfs_mimalloc::MiMalloc;\n'
GO_FLAGS = ['-trimpath', '-ldflags=-s -w -X github.com/zR-JB/graphite-meter/go/internal/config.EngineVersion=0.0.0-server-study']
TELEMETRY = b'''    if let Some(snapshot) = &finished {
        for stage in &snapshot.results {
            for (direction, result) in [("download", &stage.down), ("upload", &stage.up)] {
                if let Some(result) = result {
                    println!("GM_ALLOCATOR_RESULT {}", serde_json::json!({
                        "direction": direction, "totalBytes": result.total_bytes,
                        "meanBytesPerSec": result.mean_bytes_per_sec, "elapsedNanos": result.elapsed_nanos,
                    }));
                }
            }
        }
        for server in &snapshot.servers {
            println!("GM_ALLOCATOR_PATH {:?}", server.throughput);
        }
    }
'''


def run(command: list[str], environment: dict[str, str], log: Path, timeout: int = 1200) -> None:
    with log.open('w') as output:
        process = subprocess.Popen(command, cwd=ROOT / 'rust' if command[0] in ('cargo', 'rustup') else ROOT,
                                   env=environment, stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            process.wait(timeout=timeout)
            if process.returncode:
                raise subprocess.CalledProcessError(process.returncode, command)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)


def build_child(package: str, target: str, variant: str) -> None:
    from scripts.legal.rust import build
    notices = ROOT / 'rust/target/allocator-study-build' / f'notices-{package}-{variant}'
    # The public CLI excludes cross-target development mode; the collector itself supports it.
    build(argparse.Namespace(repo=ROOT, package=package, target=target, profile='release',
                             out=notices, reviews=ROOT / 'legal/rust-reviewed-components.json',
                             supplement=None, browser_scan=None, version='0.0.0-allocator-study',
                             review_template=False, local=True, development=True))


def build_variants(environment: dict[str, str]) -> dict[tuple[str, str], Path]:
    main = ROOT / 'rust/client/src/main.rs'
    original = main.read_bytes()
    marker = b'    let last = finished.map(|snapshot| snapshot.phase);'
    assert original.count(ALLOCATOR) == original.count(marker) == 1
    assert (ROOT / 'rust/server/src/main.rs').read_bytes().count(ALLOCATOR) == 1
    locked = tomllib.loads((ROOT / 'rust/Cargo.lock').read_text())['package']
    expected = {'rustfs-mimalloc': '95bb29aa7aee255a84d956b4f5309a3f98993ae6e10836ae5e4e8519e960780f',
                'rustfs-mimalloc-sys': '0e6571957abe93757f0c6dd99182589054ace7da4102af4296ffcc8297eb3524'}
    for name, checksum in expected.items():
        entries = [item for item in locked if item['name'] == name]
        assert len(entries) == 1 and entries[0]['version'] == '0.5.6' and entries[0]['checksum'] == checksum
    binaries = {}
    try:
        main.write_bytes(original.replace(ALLOCATOR, b'').replace(marker, TELEMETRY + marker))
        for package, variant, target in (('server', 'musl-mimalloc', MUSL), ('client', 'gnu-system', GNU)):
            run([sys.executable, '-m', 'scripts.ci.rust_allocator_study', '--build', package],
                environment, OUTPUT / f'build-{package}-{variant}.log')
            binary = ROOT / 'rust/target/allocator-study-build/binaries' / f'{package}-{variant}'
            binary.parent.mkdir(exist_ok=True)
            shutil.copy2(Path(environment['CARGO_TARGET_DIR']) / target / 'release' / f'graphite-meter-{package}', binary)
            assert b'UNREVIEWED DEVELOPMENT BUILD' in binary.read_bytes()
            binaries[package, variant] = binary
        binary = ROOT / 'rust/target/allocator-study-build/binaries/server-go-static'
        run(['go', '-C', str(ROOT / 'go'), 'build', *GO_FLAGS, '-o', str(binary), './cmd/graphite-meter'],
            environment, OUTPUT / 'build-server-go-static.log')
        run(['go', 'version', '-m', str(binary)], environment, OUTPUT / 'go-build-info.log')
        binaries['server', 'go-static'] = binary
    finally:
        main.write_bytes(original)
    return binaries


def stats(pid: int) -> dict[str, int | float]:
    raw = Path(f'/proc/{pid}/stat').read_text()
    fields = raw[raw.rfind(')') + 2:].split()
    return {'cpuSeconds': (int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK'),
            'rssBytes': int(fields[21]) * os.sysconf('SC_PAGE_SIZE'),
            'minorFaults': int(fields[7]), 'majorFaults': int(fields[9])}


def memory(pid: int) -> dict[str, int]:
    return {key: int(value.split()[0]) * 1024
            for line in Path(f'/proc/{pid}/smaps_rollup').read_text().splitlines() if ':' in line
            for key, value in [line.split(':', 1)]
            if key in ('Rss', 'Pss', 'Anonymous', 'AnonHugePages')}


@contextmanager
def server(binary: Path, environment: dict[str, str], port: int, log: Path):
    with log.open('w') as output:
        process = subprocess.Popen([str(binary)], env=environment, stdout=output, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 10
            while True:
                if process.poll() is not None:
                    raise RuntimeError(f'server exited; see {log}')
                try:
                    with urllib.request.urlopen(f'http://127.0.0.1:{port}/preflight', timeout=0.5):
                        break
                except OSError:
                    if time.monotonic() >= deadline:
                        raise TimeoutError(f'server startup timed out; see {log}')
                    time.sleep(0.05)
            yield process
        finally:
            failed = sys.exc_info()[0] is not None
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                    if not failed:
                        raise RuntimeError(f'server shutdown timed out; see {log}')
            if process.returncode != 0 and not failed:
                raise RuntimeError(f'server failed to shut down cleanly; see {log}')


def transfer(backend: subprocess.Popen, binary: Path, environment: dict[str, str], ports: dict[str, int],
             cell: str, kind: str, direction: str, streams: int, seconds: int) -> dict:
    protocol = 'http1' if kind == 'http1-tls' else kind
    scheme = 'http' if kind == 'http1' else 'https'
    origin = f'{scheme}://127.0.0.1:{ports[kind]}'
    resource = OUTPUT / f'{cell}-resources.json'
    command = ['/usr/bin/time', '-f', '{"userSeconds":%U,"systemSeconds":%S,"peakRssKiB":%M,'
               '"minorFaults":%R,"majorFaults":%F}', '-o', str(resource), str(binary), '-report', '-url',
               f'http://127.0.0.1:{ports["http1"]}', '-stages', direction, '-streams', str(streams),
               '-throughput-origin', origin, '-throughput-protocol', protocol,
               '-throughput-transport', 'fetch-stream', '-loaded-latency=false',
               '-warmup', '500ms', f'-{direction}-duration', f'{seconds}s', '-insecure']
    first = stats(backend.pid)
    peak = first['rssBytes']
    with (OUTPUT / f'{cell}-client.log').open('w') as output:
        client = subprocess.Popen(command, env=environment, stdout=output, stderr=subprocess.STDOUT,
                                  start_new_session=True)
        try:
            deadline = time.monotonic() + seconds + 22
            while client.poll() is None:
                peak = max(peak, stats(backend.pid)['rssBytes'])
                if time.monotonic() >= deadline:
                    raise TimeoutError(f'client cell timed out: {cell}')
                time.sleep(0.05)
            if client.returncode:
                raise RuntimeError(f'client cell failed: {cell}')
        finally:
            if client.poll() is None:
                os.killpg(client.pid, signal.SIGKILL)
                client.wait(timeout=5)
    last = stats(backend.pid)
    log = (OUTPUT / f'{cell}-client.log').read_text()
    records = re.findall(r'^GM_ALLOCATOR_RESULT (.+)$', log, re.MULTILINE)
    assert len(records) == 1, cell
    result = json.loads(records[0])
    assert result['direction'] == direction and result['meanBytesPerSec'] > 0, cell
    assert result['totalBytes'] > 0 and result['elapsedNanos'] > 0, cell
    paths = re.findall(r'^GM_ALLOCATOR_PATH (.+)$', log, re.MULTILINE)
    assert len(paths) == 1, paths
    assert re.findall(r'base_url: "([^"]+)"', paths[0]) == [origin], paths
    assert re.findall(r'transport: (\w+)', paths[0]) == ['FetchStream'], paths
    assert re.findall(r'protocol: (\w+)', paths[0]) == [protocol.title()], paths
    row: dict = {'kind': kind, 'protocol': protocol, 'origin': origin, 'direction': direction, 'streams': streams,
                 'transfer': result, 'path': paths[0],
                 'server': {key: last[key] - first[key] for key in ('cpuSeconds', 'minorFaults', 'majorFaults')},
                 'client': json.loads(resource.read_text())}
    row['server'].update(peakRssBytes=max(peak, last['rssBytes']), postStageRssBytes=last['rssBytes'])
    row['serverMemoryBytes'] = memory(backend.pid)
    gib = result['totalBytes'] / 2**30
    row['server']['cpuSecondsPerReportedGiB'] = row['server']['cpuSeconds'] / gib
    row['client']['cpuSecondsPerReportedGiB'] = (row['client']['userSeconds'] + row['client']['systemSeconds']) / gib
    return row


def publish(rows: list[dict], row: dict, name: str) -> None:
    rows.append(row)
    (OUTPUT / name).write_text(json.dumps(rows, indent=2) + '\n')
    print(json.dumps(row), flush=True)


def measure(binaries: dict[tuple[str, str], Path], environment: dict[str, str], ports: dict[str, int]) -> None:
    results: list[dict] = []
    for repeat in range(4):
        order = VARIANTS if repeat % 2 == 0 else VARIANTS[::-1]
        for kind, direction, streams in CASES:
            for variant in order:
                cell = f'server-{repeat + 1}-{kind}-{direction}-{streams}-{variant}'
                with server(binaries['server', variant], environment, ports['http1'],
                            OUTPUT / f'{cell}-server.log') as backend:
                    row = transfer(backend, binaries['client', 'gnu-system'], environment, ports,
                                   cell, kind, direction, streams, 10)
                row.update(experiment='server', variant=variant, repeat=repeat + 1,
                           serverVariant=variant, clientVariant='gnu-system')
                publish(results, row, 'results.json')


def retention(binaries: dict[tuple[str, str], Path], environment: dict[str, str], ports: dict[str, int]) -> None:
    results: list[dict] = []
    for variant in VARIANTS:
        with server(binaries['server', variant], environment, ports['http1'],
                    OUTPUT / f'retention-{variant}-server.log') as backend:
            time.sleep(3)
            initial = stats(backend.pid)
            peak = initial['rssBytes']
            for session in range(12):
                kind, direction = (('http2', 'upload'), ('http1', 'download'), ('http3', 'upload'))[session % 3]
                cell = f'retention-{variant}-{session + 1}-{kind}-{direction}'
                row = transfer(backend, binaries['client', 'gnu-system'], environment, ports,
                               cell, kind, direction, 4, 10)
                peak = max(peak, row['server']['peakRssBytes'])
                time.sleep(3)
                idle = stats(backend.pid)
                row['server']['idleRssBytes'] = idle['rssBytes']
                row.update(experiment='retention', variant=variant, allocatorEnvironment={}, session=session + 1,
                           serverVariant=variant, clientVariant='gnu-system', initialIdle=initial,
                           idleAfterSeconds=3, idle=idle, idleMemoryBytes=memory(backend.pid),
                           lifetimeSampledPeakRssBytes=peak)
                publish(results, row, 'retention.json')


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build', choices=('server', 'client'))
    args = parser.parse_args()
    if args.build:
        package, target, variant = {
            'server': ('graphite-meter-server', MUSL, 'musl-mimalloc'),
            'client': ('graphite-meter-client', GNU, 'gnu-system'),
        }[args.build]
        build_child(package, target, variant)
        return
    OUTPUT.mkdir(parents=True, exist_ok=True)
    profile = tomllib.loads((ROOT / 'rust/Cargo.toml').read_text())['profile']['release']
    assert (profile['opt-level'], profile['lto'], profile['codegen-units']) == (3, 'fat', 1)
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('GM_', 'CARGO_PROFILE_', 'MIMALLOC_', 'MALLOC_'))
                   and not key.endswith('RUSTFLAGS')
                   and key not in ('GOFLAGS', 'GOGC', 'GOMEMLIMIT', 'GOMAXPROCS', 'GODEBUG',
                                   'GOEXPERIMENT', 'GOAMD64', 'GOOS', 'GOARCH')}
    environment.update(LC_ALL='C', CARGO_INCREMENTAL='0', CARGO_TARGET_DIR=str(ROOT / 'rust/target/allocator-study-build'),
                       CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER='x86_64-linux-gnu-gcc',
                       CC_x86_64_unknown_linux_musl='musl-gcc', CGO_ENABLED='0', GOENV='off', GOTOOLCHAIN='local',
                       RUSTUP_TOOLCHAIN=tomllib.loads((ROOT / 'rust/rust-toolchain.toml').read_text())['toolchain']['channel'])
    assert not any(environment.get(key) for key in ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER'))
    fixture = Fixture('allocator-study-')
    try:
        run(['rustup', 'target', 'add', MUSL], environment, OUTPUT / 'target-setup.log')
        run(['cargo', 'fetch', '--locked'], environment, OUTPUT / 'cargo-fetch.log')
        for name, command in (('cpu', ['lscpu']), ('rustc', ['rustc', '-vV']), ('go-version', ['go', 'version']),
                              ('musl-cc', ['musl-gcc', '-v']), ('gnu-cc', ['gcc', '-v'])):
            run(command, environment, OUTPUT / f'{name}.log')
        run(['go', 'env', '-json', 'CGO_ENABLED', 'GOFLAGS', 'GOAMD64', 'GOEXPERIMENT', 'GOOS', 'GOARCH', 'GOTOOLCHAIN'],
            environment, OUTPUT / 'go-build-env.json')
        binaries = build_variants(environment)
        _, cert, key = fixture.identity()
        ports = {protocol: unused_port() for protocol in ('http1', 'http2', 'http1-tls')}
        ports['http3'] = unused_port(kind=socket.SOCK_DGRAM)
        environment.update(GM_AUTH_MODE='off', GM_H1_ADDR=f'127.0.0.1:{ports["http1"]}',
                           GM_H1_TLS_ADDR=f'127.0.0.1:{ports["http1-tls"]}',
                           GM_H2_ADDR=f'127.0.0.1:{ports["http2"]}', GM_H3_ADDR=f'127.0.0.1:{ports["http3"]}',
                           GM_TLS_CERT=str(cert), GM_TLS_KEY=str(key))
        (OUTPUT / 'study.json').write_text(json.dumps({'profile': profile, 'mode': 'go-rust-server',
            'variants': VARIANTS,
            'variantLabels': {'go-static': 'Go production static server / default GC',
                              'musl-mimalloc': 'Rust musl / RustFS wrapper0.5.6 / native mimalloc3.5.3',
                              'gnu-system': 'fixed GNU Rust client'},
            'baselines': {'server': 'go-static'}, 'candidate': 'musl-mimalloc',
            'candidateSource': {'wrapper': 'rustfs-mimalloc =0.5.6', 'sys': 'rustfs-mimalloc-sys =0.5.6',
                                'native': 'Microsoft mimalloc v3.5.3'},
            'goVersion': (OUTPUT / 'go-version.log').read_text().strip(), 'goBuildFlags': GO_FLAGS,
            'goBuildEnvironment': json.loads((OUTPUT / 'go-build-env.json').read_text()),
            'goRuntime': 'default GC, memory limit and GOMAXPROCS; inherited overrides removed; no forced GC',
            'source': 'unchanged Go production server; actual RustFS musl server; fixed GNU Rust client with telemetry only',
            'repeats': 4, 'order': 'Go/Rust, Rust/Go, Go/Rust, Rust/Go', 'expectedTransfers': 56,
            'builds': 3, 'families': CASES, 'experiments': ['server'],
            'counterparts': {'server': 'fixed GNU-system Rust client'},
            'platform': list(os.uname()), 'logicalCpus': os.cpu_count(),
            'transparentHugePages': {name: Path('/sys/kernel/mm/transparent_hugepage', name).read_text().strip()
                                     for name in ('enabled', 'defrag', 'hpage_pmd_size')},
            'muslCCompiler': 'musl-gcc', 'warmupMs': 500, 'measureMs': 10000, 'streams': [4],
            'cpuWindow': 'client GNU-time counters cover its entire process lifetime; server deltas bracket that same '
                         'client invocation, including preparation, warmup and drain; reported bytes/duration belong '
                         'to the receiver measurement, not an identical CPU window',
            'rss': 'server sampled every 50ms; Go RSS includes runtime and GC-retained heap by default; '
                   'client exact process high-water RSS via GNU time',
            'retention': 'one persistent server per implementation, four cycles of H2 upload/H1 clear download/'
                         'H3 upload at four streams; post-stage and three-second-idle RSS. No forced GC. '
                         'Short mixed retention check, not a long soak; retained measurement records, connection state, '
                         'allocator or GC heap can contribute to growth, which alone does not establish a leak',
            'limitation': 'shared hosted x86_64 loopback; fixed peer can cap throughput; compare same-run pairs only; '
                          'no shaped link, ARM, or H3 download GNU-server control'}, indent=2) + '\n')
        measure(binaries, environment, ports)
        retention(binaries, environment, ports)
    finally:
        shutil.rmtree(fixture.directory, ignore_errors=True)
        shutil.rmtree(ROOT / 'rust/target/allocator-study-build', ignore_errors=True)


if __name__ == '__main__':
    main()
