"""Temporary hosted mimalloc THP comparison; binaries and TLS keys are never uploaded."""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
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
OUTPUT = ROOT / 'rust/target/thp-study-results'
BUILD = ROOT / 'rust/target/thp-study-build'
MUSL = 'x86_64-unknown-linux-musl'
GNU = 'x86_64-unknown-linux-gnu'
VARIANTS = ('default', 'thp-off')
CASES = (('http1', 'upload', 4), ('http1', 'download', 4), ('http2', 'upload', 4),
         ('http3', 'upload', 4), ('http3', 'download', 4))
REPEATS = 12
ALLOCATOR = b'#[cfg(target_env = "musl")]\n#[global_allocator]\nstatic ALLOCATOR: rustfs_mimalloc::MiMalloc = rustfs_mimalloc::MiMalloc;\n'
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
    notices = BUILD / f'notices-{package}-{variant}'
    # The public CLI excludes cross-target development mode; the collector itself supports it.
    build(argparse.Namespace(repo=ROOT, package=package, target=target, profile='release',
                             out=notices, reviews=ROOT / 'legal/rust-reviewed-components.json',
                             supplement=None, browser_scan=None, version='0.0.0-thp-study',
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
        for label, package, variant, target in (('server', 'server', 'musl-mimalloc', MUSL),
                                               ('client', 'client', 'gnu-system', GNU)):
            run([sys.executable, '-m', 'scripts.ci.rust_thp_study', '--build', label],
                environment, OUTPUT / f'build-{package}-{variant}.log')
            binary = BUILD / 'binaries' / f'{package}-{variant}'
            binary.parent.mkdir(exist_ok=True)
            shutil.copy2(Path(environment['CARGO_TARGET_DIR']) / target / 'release' / f'graphite-meter-{package}', binary)
            assert b'UNREVIEWED DEVELOPMENT BUILD' in binary.read_bytes()
            binaries[package, variant] = binary
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
    protocol = kind
    scheme = 'http' if kind == 'http1' else 'https'
    origin = f'{scheme}://127.0.0.1:{ports[kind]}'
    resource = OUTPUT / f'{cell}-resources.json'
    command = ['/usr/bin/time', '-f', '{"userSeconds":%U,"systemSeconds":%S,"peakRssKiB":%M,'
               '"minorFaults":%R,"majorFaults":%F}', '-o', str(resource), str(binary), '-report', '-url',
               f'http://127.0.0.1:{ports["http1"]}', '-stages', direction, '-streams', str(streams),
               '-throughput-origin', origin, '-throughput-protocol', protocol,
               '-throughput-transport', 'fetch-stream', '-loaded-latency=false',
               '-warmup', '500ms', f'-{direction}-duration', f'{seconds}s', '-insecure']
    first_memory = memory(backend.pid)
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
    row['serverMemoryStartBytes'] = first_memory
    row['serverMemoryEndBytes'] = memory(backend.pid)
    gib = result['totalBytes'] / 2**30
    row['server']['cpuSecondsPerReportedGiB'] = row['server']['cpuSeconds'] / gib
    row['client']['cpuSecondsPerReportedGiB'] = (row['client']['userSeconds'] + row['client']['systemSeconds']) / gib
    return row


def publish(rows: list[dict], row: dict, name: str) -> None:
    rows.append(row)
    (OUTPUT / name).write_text(json.dumps(rows, indent=2) + '\n')
    print(json.dumps(row), flush=True)


def measure(binaries: dict[tuple[str, str], Path], environment: dict[str, str],
            ports: dict[str, int], server_hash: str) -> list[dict]:
    results: list[dict] = []
    for repeat in range(REPEATS):
        offset = repeat % len(CASES)
        order = VARIANTS if repeat % 2 == 0 else VARIANTS[::-1]
        for case_position, (kind, direction, streams) in enumerate(CASES[offset:] + CASES[:offset], 1):
            for variant_position, variant in enumerate(order, 1):
                cell = f'server-{repeat + 1}-{kind}-{direction}-{streams}-{variant}'
                own = environment if variant == 'default' else environment | {'MIMALLOC_ALLOW_THP': '0'}
                with server(binaries['server', 'musl-mimalloc'], own, ports['http1'],
                            OUTPUT / f'{cell}-server.log') as backend:
                    row = transfer(backend, binaries['client', 'gnu-system'], environment, ports,
                                   cell, kind, direction, streams, 10)
                row.update(experiment='server', variant=variant, repeat=repeat + 1,
                           casePosition=case_position, variantPosition=variant_position,
                           allocatorEnvironment={} if variant == 'default' else {'MIMALLOC_ALLOW_THP': '0'},
                           serverBinarySha256=server_hash, clientVariant='gnu-system')
                publish(results, row, 'results.json')
    return results


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
    workspace = tomllib.loads((ROOT / 'rust/Cargo.toml').read_text())
    profile = workspace['profile']['release']
    assert (profile['opt-level'], profile['lto'], profile['codegen-units']) == (3, 'fat', 1)
    assert workspace['workspace']['dependencies']['rustfs-mimalloc'] == '=0.5.6'
    thp = {name: Path('/sys/kernel/mm/transparent_hugepage', name).read_text().strip()
           for name in ('enabled', 'defrag', 'hpage_pmd_size')}
    if '[never]' in thp['enabled']:
        raise RuntimeError('host THP policy is never; a default-versus-disabled THP comparison is not meaningful')
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('GM_', 'CARGO_PROFILE_', 'MIMALLOC_', 'MALLOC_'))
                   and not key.endswith('RUSTFLAGS')}
    channel = tomllib.loads((ROOT / 'rust/rust-toolchain.toml').read_text())['toolchain']['channel']
    environment.update(LC_ALL='C', CARGO_INCREMENTAL='0', CARGO_TARGET_DIR=str(BUILD),
                       CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER='x86_64-linux-gnu-gcc',
                       CC_x86_64_unknown_linux_musl='musl-gcc', RUSTUP_TOOLCHAIN=channel)
    assert not any(environment.get(key) for key in ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER'))
    fixture = Fixture('thp-study-')
    try:
        run(['rustup', 'target', 'add', MUSL], environment, OUTPUT / 'target-setup.log')
        run(['cargo', 'fetch', '--locked'], environment, OUTPUT / 'cargo-fetch.log')
        for name, command in (('cpu', ['lscpu']), ('rustc', ['rustc', f'+{channel}', '-vV']),
                              ('musl-cc', ['musl-gcc', '-v']), ('gnu-cc', ['gcc', '-v'])):
            run(command, environment, OUTPUT / f'{name}.log')
        binaries = build_variants(environment)
        hashes = {f'{package}-{variant}': hashlib.sha256(binary.read_bytes()).hexdigest()
                  for (package, variant), binary in binaries.items()}
        _, cert, key = fixture.identity()
        ports = {protocol: unused_port() for protocol in ('http1', 'http2')}
        ports['http3'] = unused_port(kind=socket.SOCK_DGRAM)
        environment.update(GM_AUTH_MODE='off', GM_H1_ADDR=f'127.0.0.1:{ports["http1"]}',
                           GM_H2_ADDR=f'127.0.0.1:{ports["http2"]}', GM_H3_ADDR=f'127.0.0.1:{ports["http3"]}',
                           GM_TLS_CERT=str(cert), GM_TLS_KEY=str(key))
        study = {'profile': profile, 'mode': 'current-mimalloc-thp-paired',
            'variants': VARIANTS, 'variantLabels': {'default': 'shipping mimalloc default THP policy',
                                                  'thp-off': 'same binary with MIMALLOC_ALLOW_THP=0'},
            'candidateSource': {'wrapper': 'rustfs-mimalloc =0.5.6', 'sys': 'rustfs-mimalloc-sys =0.5.6',
                                'native': 'Microsoft mimalloc v3.5.3', 'features': []},
            'binarySha256': hashes, 'rustChannel': channel, 'rustc': (OUTPUT / 'rustc.log').read_text(),
            'compileEnvironment': {key: value for key, value in environment.items()
                                   if key.startswith(('CARGO_', 'CC_', 'RUSTUP_'))},
            'repeats': REPEATS, 'order': 'default/off then off/default, alternating each repetition',
            'caseOrder': 'rotate the five cases one position each repetition',
            'expectedTransfers': len(CASES) * len(VARIANTS) * REPEATS, 'builds': 2, 'families': CASES,
            'counterpart': 'fixed GNU Rust client; environment unchanged for both server settings',
            'platform': list(os.uname()), 'logicalCpus': os.cpu_count(), 'lscpu': (OUTPUT / 'cpu.log').read_text(),
            'transparentHugePages': thp, 'muslCCompiler': 'musl-gcc',
            'warmupMs': 500, 'measureMs': 10000, 'streams': [4],
            'cpuWindow': 'client GNU-time counters cover its entire process lifetime; server deltas bracket that same '
                         'client invocation, including preparation, warmup and drain; receiver bytes/duration belong '
                         'to the measurement stage, not an identical CPU window',
            'memory': 'server RSS sampled every 50 ms; RSS/PSS/Anonymous/AnonHugePages read at start and end. '
                      'Endpoints do not establish peak PSS or exclude transient huge pages',
            'limitations': ['shared hosted x86_64 loopback; fixed peer can cap throughput; no shaped link or ARM evidence',
                            'only allocator THP setting changes; this can affect allocator purge behavior too, '
                            'so it is not an isolated CPU/TLB experiment',
                            'no purge-delay tuning, global host policy change, idle retention test, or Go comparison']}
        (OUTPUT / 'study.json').write_text(json.dumps(study, indent=2) + '\n')
        rows = measure(binaries, environment, ports, hashes['server-musl-mimalloc'])
        study['defaultNoObservedHugePages'] = [f"{row['repeat']}-{row['kind']}-{row['direction']}"
            for row in rows if row['variant'] == 'default'
            and row['serverMemoryStartBytes']['AnonHugePages'] == row['serverMemoryEndBytes']['AnonHugePages'] == 0]
        (OUTPUT / 'study.json').write_text(json.dumps(study, indent=2) + '\n')
    finally:
        shutil.rmtree(fixture.directory, ignore_errors=True)
        shutil.rmtree(BUILD, ignore_errors=True)


if __name__ == '__main__':
    main()
