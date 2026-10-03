"""Temporary hosted allocator comparison; every diagnostic binary is development-marked, never uploaded."""
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
VARIANTS = ('musl-system', 'musl-dlmalloc', 'gnu-system')
ALLOCATOR = b'#[cfg(target_env = "musl")]\n#[global_allocator]\nstatic ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;\n'
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
    paths = [ROOT / f'rust/{package}/{name}' for package, name in
             (('server', 'Cargo.toml'), ('server', 'src/main.rs'), ('client', 'src/main.rs'))]
    paths.append(ROOT / 'rust/Cargo.lock')
    original = {path: path.read_bytes() for path in paths}
    server_manifest, server_main, client_main, lock = paths
    locked_packages = [item for item in tomllib.loads(original[lock].decode())['package'] if item.get('source')]
    marker = b'    let last = finished.map(|snapshot| snapshot.phase);'
    assert original[client_main].count(ALLOCATOR) == original[client_main].count(marker) == 1
    base_client = original[client_main].replace(marker, TELEMETRY + marker)
    binaries = {}
    try:
        for variant in VARIANTS:
            for package in ('server', 'client'):
                for path, data in original.items():
                    path.write_bytes(data)
                client_main.write_bytes(base_client.replace(ALLOCATOR, b'') if variant == 'musl-system'
                                        else base_client)
                if package == 'server' and variant == 'musl-dlmalloc':
                    header = b'#![forbid(unsafe_code)]\n'
                    assert original[server_main].count(header) == 1
                    server_main.write_bytes(original[server_main].replace(header, header + ALLOCATOR, 1))
                    # Reuse an already locked/reviewed dependency; no permanent manifest or lock change.
                    server_manifest.write_bytes(original[server_manifest] + b'\n[target.\'cfg(target_env = "musl")\'.dependencies]\n'
                                                b'dlmalloc = { version = "0.2.14", features = ["global"] }\n')
                    run(['cargo', 'metadata', '--offline', '--format-version=1'], environment,
                        OUTPUT / 'allocator-lock-update.log')
                    current = tomllib.loads(lock.read_text())['package']
                    assert [item for item in current if item.get('source')] == locked_packages
                target = GNU if variant == 'gnu-system' else MUSL
                run([sys.executable, '-m', 'scripts.ci.rust_allocator_study', '--build', package, '--variant', variant],
                    environment, OUTPUT / f'build-{package}-{variant}.log')
                binary = ROOT / 'rust/target/allocator-study-build/binaries' / f'{package}-{variant}'
                binary.parent.mkdir(exist_ok=True)
                shutil.copy2(Path(environment['CARGO_TARGET_DIR']) / target / 'release' / f'graphite-meter-{package}', binary)
                assert b'UNREVIEWED DEVELOPMENT BUILD' in binary.read_bytes()
                binaries[package, variant] = binary
    finally:
        for path, data in original.items():
            path.write_bytes(data)
    return binaries


def stats(pid: int) -> dict[str, int | float]:
    raw = Path(f'/proc/{pid}/stat').read_text()
    fields = raw[raw.rfind(')') + 2:].split()
    return {'cpuSeconds': (int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK'),
            'rssBytes': int(fields[21]) * os.sysconf('SC_PAGE_SIZE'),
            'minorFaults': int(fields[7]), 'majorFaults': int(fields[9])}


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


def measure(binaries: dict[tuple[str, str], Path], environment: dict[str, str], ports: dict[str, int]) -> None:
    results = []
    for experiment in ('server', 'client'):
        for repeat in range(2):
            order = VARIANTS if repeat == 0 else VARIANTS[::-1]
            for protocol in ('http1', 'http2', 'http3'):
                for direction in ('download', 'upload'):
                    for streams in (1, 4):
                        for variant in order:
                            cell = f'{experiment}-{repeat + 1}-{protocol}-{direction}-{streams}-{variant}'
                            own_server = variant if experiment == 'server' else 'musl-system'
                            own_client = 'gnu-system' if experiment == 'server' else variant
                            with server(binaries['server', own_server], environment, ports['http1'],
                                        OUTPUT / f'{cell}-server.log') as backend:
                                resource = OUTPUT / f'{cell}-resources.json'
                                command = ['/usr/bin/time', '-f', '{"userSeconds":%U,"systemSeconds":%S,"peakRssKiB":%M,'
                                           '"minorFaults":%R,"majorFaults":%F}', '-o', str(resource),
                                           str(binaries['client', own_client]), '-report', '-url',
                                           f'http://127.0.0.1:{ports["http1"]}', '-stages', direction,
                                           '-streams', str(streams), '-throughput-protocol', protocol,
                                           '-throughput-transport', 'fetch-stream', '-loaded-latency=false',
                                           '-warmup', '500ms', f'-{direction}-duration', '3s', '-insecure']
                                first = stats(backend.pid)
                                peak = first['rssBytes']
                                with (OUTPUT / f'{cell}-client.log').open('w') as output:
                                    client = subprocess.Popen(command, env=environment, stdout=output, stderr=subprocess.STDOUT,
                                                              start_new_session=True)
                                    try:
                                        deadline = time.monotonic() + 25
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
                            transfer = json.loads(records[0])
                            assert transfer['direction'] == direction and transfer['meanBytesPerSec'] > 0, cell
                            assert transfer['totalBytes'] > 0 and transfer['elapsedNanos'] > 0, cell
                            paths = re.findall(r'^GM_ALLOCATOR_PATH (.+)$', log, re.MULTILINE)
                            assert len(paths) == 1 and protocol.title() in paths[0], paths
                            row: dict = {'experiment': experiment, 'variant': variant, 'repeat': repeat + 1,
                                   'protocol': protocol, 'direction': direction, 'streams': streams,
                                   'transfer': transfer, 'path': paths[0],
                                   'server': {key: last[key] - first[key] for key in ('cpuSeconds', 'minorFaults', 'majorFaults')},
                                   'client': json.loads(resource.read_text())}
                            row['server']['peakRssBytes'] = peak
                            gib = transfer['totalBytes'] / 2**30
                            row['server']['cpuSecondsPerReportedGiB'] = row['server']['cpuSeconds'] / gib
                            row['client']['cpuSecondsPerReportedGiB'] = (row['client']['userSeconds'] +
                                                                       row['client']['systemSeconds']) / gib
                            results.append(row)
                            (OUTPUT / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
                            print(json.dumps(row), flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build', choices=('server', 'client'))
    parser.add_argument('--variant', choices=VARIANTS)
    args = parser.parse_args()
    if args.build:
        if args.variant is None:
            parser.error('--build requires --variant')
        package = {'server': 'graphite-meter-server', 'client': 'graphite-meter-client'}[args.build]
        target, variant = {'musl-system': (MUSL, 'musl-system'), 'musl-dlmalloc': (MUSL, 'musl-dlmalloc'),
                           'gnu-system': (GNU, 'gnu-system')}[args.variant]
        build_child(package, target, variant)
        return
    if args.variant is not None:
        parser.error('--variant requires --build')
    OUTPUT.mkdir(parents=True, exist_ok=True)
    profile = tomllib.loads((ROOT / 'rust/Cargo.toml').read_text())['profile']['release']
    assert (profile['opt-level'], profile['lto'], profile['codegen-units']) == (3, 'fat', 1)
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('GM_', 'CARGO_PROFILE_')) and not key.endswith('RUSTFLAGS')}
    environment.update(LC_ALL='C', CARGO_INCREMENTAL='0', CARGO_TARGET_DIR=str(ROOT / 'rust/target/allocator-study-build'),
                       CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER='x86_64-linux-gnu-gcc')
    assert not any(environment.get(key) for key in ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER'))
    run(['rustup', 'target', 'add', MUSL], environment, OUTPUT / 'target-setup.log')
    run(['cargo', 'fetch', '--locked'], environment, OUTPUT / 'cargo-fetch.log')
    fixture = Fixture('allocator-study-')
    try:
        binaries = build_variants(environment)
        _, cert, key = fixture.identity()
        ports = {protocol: unused_port() for protocol in ('http1', 'http2', 'http3')}
        ports['http3'] = unused_port(kind=socket.SOCK_DGRAM)
        environment.update(GM_AUTH_MODE='off', GM_H1_ADDR=f'127.0.0.1:{ports["http1"]}', GM_H1_TLS_ADDR='',
                           GM_H2_ADDR=f'127.0.0.1:{ports["http2"]}', GM_H3_ADDR=f'127.0.0.1:{ports["http3"]}',
                           GM_TLS_CERT=str(cert), GM_TLS_KEY=str(key))
        (OUTPUT / 'study.json').write_text(json.dumps({'profile': profile, 'variants': VARIANTS, 'repeats': 2,
            'platform': list(os.uname()), 'logicalCpus': os.cpu_count(),
            'warmupMs': 500, 'measureMs': 3000, 'streams': [1, 4],
            'cpuWindow': 'entire client lifetime, including preparation, warmup and drain',
            'rss': 'server sampled every 50ms; client exact process high-water RSS via GNU time',
            'limitation': 'shared hosted x86_64 loopback runner; no shaped link or ARM evidence'}, indent=2) + '\n')
        measure(binaries, environment, ports)
    finally:
        shutil.rmtree(fixture.directory, ignore_errors=True)
        shutil.rmtree(ROOT / 'rust/target/allocator-study-build', ignore_errors=True)


if __name__ == '__main__':
    main()
