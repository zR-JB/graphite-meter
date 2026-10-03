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
VARIANTS = ('musl-system', 'musl-dlmalloc', 'musl-mimalloc', 'gnu-system')
CONFIRM_CASES = {
    'server': (('http1', 'upload', 4), ('http1-tls', 'upload', 4), ('http2', 'upload', 4),
               ('http3', 'upload', 4), ('http3', 'download', 4)),
    'client': (('http1', 'download', 4), ('http2', 'download', 1), ('http3', 'download', 4)),
}
ALLOCATOR = b'#[cfg(target_env = "musl")]\n#[global_allocator]\nstatic ALLOCATOR: rustfs_mimalloc::MiMalloc = rustfs_mimalloc::MiMalloc;\n'
DLMALLOC = b'#[cfg(target_env = "musl")]\n#[global_allocator]\nstatic ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;\n'
DLMALLOC_PACKAGE = {'name': 'dlmalloc', 'version': '0.2.14',
                    'source': 'registry+https://github.com/rust-lang/crates.io-index',
                    'checksum': 'ad5208a115eaba24916f7456929832e310a81518c641f93fee4f89aa93aa3675',
                    'dependencies': ['cfg-if', 'libc', 'windows-sys 0.61.2']}
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


def build_variants(environment: dict[str, str], confirm: bool = False) -> dict[tuple[str, str], Path]:
    paths = [ROOT / f'rust/{package}/{name}' for package, name in
             (('server', 'Cargo.toml'), ('server', 'src/main.rs'), ('client', 'src/main.rs'), ('client', 'Cargo.toml'))]
    paths.append(ROOT / 'rust/Cargo.lock')
    original = {path: path.read_bytes() for path in paths}
    server_manifest, server_main, client_main, client_manifest, lock = paths
    locked_packages = [item for item in tomllib.loads(original[lock].decode())['package'] if item.get('source')]
    marker = b'    let last = finished.map(|snapshot| snapshot.phase);'
    assert original[server_main].count(ALLOCATOR) == original[client_main].count(ALLOCATOR) == 1
    assert original[client_main].count(marker) == 1
    expected = {'rustfs-mimalloc': '95bb29aa7aee255a84d956b4f5309a3f98993ae6e10836ae5e4e8519e960780f',
                'rustfs-mimalloc-sys': '0e6571957abe93757f0c6dd99182589054ace7da4102af4296ffcc8297eb3524'}
    for name, checksum in expected.items():
        entries = [item for item in locked_packages if item['name'] == name]
        assert len(entries) == 1 and entries[0]['version'] == '0.5.6' and entries[0]['checksum'] == checksum
    base_client = original[client_main].replace(marker, TELEMETRY + marker)
    binaries = {}
    selections = (('server', 'musl-system'), ('server', 'musl-mimalloc'), ('server', 'gnu-system'),
                  ('client', 'musl-dlmalloc'), ('client', 'musl-mimalloc'), ('client', 'gnu-system')) if confirm else tuple(
        (package, variant) for variant in VARIANTS for package in ('server', 'client'))
    try:
        for package, variant in selections:
            for path, data in original.items():
                path.write_bytes(data)
            client_main.write_bytes(base_client)
            if variant != 'musl-mimalloc':
                main = server_main if package == 'server' else client_main
                manifest = server_manifest if package == 'server' else client_manifest
                declaration = original[main] if package == 'server' else base_client
                main.write_bytes(declaration.replace(ALLOCATOR, DLMALLOC if variant == 'musl-dlmalloc' else b''))
                dependency = b'rustfs-mimalloc.workspace = true\n'
                assert original[manifest].count(dependency) == 1
                replacement = b'dlmalloc = { version = "=0.2.14", features = ["global"] }\n' if variant == 'musl-dlmalloc' else b''
                # The other workspace consumer retains RustFS lock entries; only this allocator selection changes.
                manifest.write_bytes(original[manifest].replace(dependency, replacement))
                run(['cargo', 'metadata', '--format-version=1'], environment, OUTPUT / f'lock-{package}-{variant}.log')
                current = [item for item in tomllib.loads(lock.read_text())['package'] if item.get('source')]
                added = [item for item in current if item not in locked_packages]
                assert added == ([DLMALLOC_PACKAGE] if variant == 'musl-dlmalloc' and DLMALLOC_PACKAGE not in locked_packages else [])
                assert [item for item in current if item not in added] == locked_packages
            (OUTPUT / f'{package}-{variant}-Cargo.lock.txt').write_bytes(lock.read_bytes())
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


def measure(binaries: dict[tuple[str, str], Path], environment: dict[str, str], ports: dict[str, int],
            confirm: bool = False) -> None:
    results: list[dict] = []
    for experiment in ('server', 'client'):
        baseline = 'musl-system' if experiment == 'server' else 'musl-dlmalloc'
        variants = (baseline, 'musl-mimalloc') if confirm else VARIANTS
        cases = CONFIRM_CASES[experiment] if confirm else tuple(
            (protocol, direction, streams) for protocol in ('http1', 'http2', 'http3')
            for direction in ('download', 'upload') for streams in (1, 4))
        for repeat in range(4 if confirm else 2):
            order = variants if repeat % 2 == 0 else variants[::-1]
            for kind, direction, streams in cases:
                case_order = order
                if confirm and experiment == 'server' and (kind, direction, streams) == ('http3', 'download', 4):
                    three = (*variants, 'gnu-system')
                    case_order = three if repeat % 2 == 0 else three[::-1]
                for variant in case_order:
                    cell = f'{experiment}-{repeat + 1}-{kind}-{direction}-{streams}-{variant}'
                    own_server = variant if experiment == 'server' else ('gnu-system' if confirm else 'musl-system')
                    own_client = 'gnu-system' if experiment == 'server' else variant
                    with server(binaries['server', own_server], environment, ports['http1'],
                                OUTPUT / f'{cell}-server.log') as backend:
                        row = transfer(backend, binaries['client', own_client], environment, ports,
                                       cell, kind, direction, streams, 10 if confirm else 3)
                    row.update(experiment=experiment, variant=variant, repeat=repeat + 1,
                               serverVariant=own_server, clientVariant=own_client)
                    publish(results, row, 'results.json')


def retention(binaries: dict[tuple[str, str], Path], environment: dict[str, str], ports: dict[str, int]) -> None:
    results: list[dict] = []
    for variant, label, overrides in (('musl-system', 'musl-system', {}), ('musl-mimalloc', 'musl-mimalloc', {}),
                                      ('musl-mimalloc', 'musl-mimalloc-thp-off', {'MIMALLOC_ALLOW_THP': '0'})):
        with server(binaries['server', variant], environment | overrides, ports['http1'],
                    OUTPUT / f'retention-{label}-server.log') as backend:
            time.sleep(3)
            initial = stats(backend.pid)
            peak = initial['rssBytes']
            for session in range(6):
                kind, direction = (('http2', 'upload'), ('http1', 'download'), ('http3', 'upload'))[session % 3]
                cell = f'retention-{label}-{session + 1}-{kind}-{direction}'
                row = transfer(backend, binaries['client', 'gnu-system'], environment, ports,
                               cell, kind, direction, 4, 10)
                peak = max(peak, row['server']['peakRssBytes'])
                time.sleep(3)
                idle = stats(backend.pid)
                row['server']['idleRssBytes'] = idle['rssBytes']
                row.update(experiment='retention', variant=label, allocatorEnvironment=overrides, session=session + 1,
                           serverVariant=variant, clientVariant='gnu-system', initialIdle=initial,
                           idleAfterSeconds=3, idle=idle, idleMemoryBytes=memory(backend.pid),
                           lifetimeSampledPeakRssBytes=peak)
                publish(results, row, 'retention.json')


def thp_control(binaries: dict[tuple[str, str], Path], environment: dict[str, str], ports: dict[str, int]) -> None:
    results: list[dict] = []
    cases = (('server', 'http1', 'upload'), ('server', 'http2', 'upload'),
             ('server', 'http3', 'download'), ('client', 'http1', 'download'))
    settings = (('musl-mimalloc', {}), ('musl-mimalloc-thp-off', {'MIMALLOC_ALLOW_THP': '0'}))
    for repeat in range(2):
        order = settings if repeat == 0 else settings[::-1]
        for experiment, kind, direction in cases:
            for label, overrides in order:
                cell = f'thp-{experiment}-{repeat + 1}-{kind}-{direction}-4-{label}'
                own_server = 'musl-mimalloc' if experiment == 'server' else 'gnu-system'
                own_client = 'gnu-system' if experiment == 'server' else 'musl-mimalloc'
                with server(binaries['server', own_server], environment | (overrides if experiment == 'server' else {}),
                            ports['http1'], OUTPUT / f'{cell}-server.log') as backend:
                    row = transfer(backend, binaries['client', own_client],
                                   environment | (overrides if experiment == 'client' else {}),
                                   ports, cell, kind, direction, 4, 10)
                row.update(experiment=experiment, variant=label, allocatorEnvironment=overrides,
                           repeat=repeat + 1, serverVariant=own_server, clientVariant=own_client)
                publish(results, row, 'thp.json')


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build', choices=('server', 'client'))
    parser.add_argument('--variant', choices=VARIANTS)
    parser.add_argument('--confirm', action='store_true', help='focused mimalloc confirmation with GNU peers')
    args = parser.parse_args()
    if args.build:
        if args.confirm:
            parser.error('--confirm cannot be combined with --build')
        if args.variant is None:
            parser.error('--build requires --variant')
        package = {'server': 'graphite-meter-server', 'client': 'graphite-meter-client'}[args.build]
        target, variant = {'musl-system': (MUSL, 'musl-system'), 'musl-dlmalloc': (MUSL, 'musl-dlmalloc'),
                           'musl-mimalloc': (MUSL, 'musl-mimalloc'), 'gnu-system': (GNU, 'gnu-system')}[args.variant]
        build_child(package, target, variant)
        return
    if args.variant is not None:
        parser.error('--variant requires --build')
    OUTPUT.mkdir(parents=True, exist_ok=True)
    profile = tomllib.loads((ROOT / 'rust/Cargo.toml').read_text())['profile']['release']
    assert (profile['opt-level'], profile['lto'], profile['codegen-units']) == (3, 'fat', 1)
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('GM_', 'CARGO_PROFILE_', 'MIMALLOC_', 'MALLOC_'))
                   and not key.endswith('RUSTFLAGS')}
    environment.update(LC_ALL='C', CARGO_INCREMENTAL='0', CARGO_TARGET_DIR=str(ROOT / 'rust/target/allocator-study-build'),
                       CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER='x86_64-linux-gnu-gcc',
                       CC_x86_64_unknown_linux_musl='musl-gcc')
    assert not any(environment.get(key) for key in ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER'))
    run(['rustup', 'target', 'add', MUSL], environment, OUTPUT / 'target-setup.log')
    run(['cargo', 'fetch', '--locked'], environment, OUTPUT / 'cargo-fetch.log')
    for name, command in (('cpu', ['lscpu']), ('rustc', ['rustc', '-vV']),
                          ('musl-cc', ['musl-gcc', '-v']), ('gnu-cc', ['gcc', '-v'])):
        run(command, environment, OUTPUT / f'{name}.log')
    fixture = Fixture('allocator-study-')
    try:
        binaries = build_variants(environment, args.confirm)
        _, cert, key = fixture.identity()
        ports = {protocol: unused_port() for protocol in ('http1', 'http2', 'http3')}
        ports['http3'] = unused_port(kind=socket.SOCK_DGRAM)
        ports['http1-tls'] = unused_port()
        environment.update(GM_AUTH_MODE='off', GM_H1_ADDR=f'127.0.0.1:{ports["http1"]}',
                           GM_H1_TLS_ADDR=f'127.0.0.1:{ports["http1-tls"]}' if args.confirm else '',
                           GM_H2_ADDR=f'127.0.0.1:{ports["http2"]}', GM_H3_ADDR=f'127.0.0.1:{ports["http3"]}',
                           GM_TLS_CERT=str(cert), GM_TLS_KEY=str(key))
        (OUTPUT / 'study.json').write_text(json.dumps({'profile': profile,
            'mode': 'confirmation' if args.confirm else 'screening',
            'variants': ['musl-system', 'musl-dlmalloc', 'musl-mimalloc'] if args.confirm else VARIANTS,
            'baselines': {'server': 'musl-system', 'client': 'musl-dlmalloc'}, 'candidate': 'musl-mimalloc',
            'candidateSource': {'wrapper': 'rustfs-mimalloc =0.5.6', 'sys': 'rustfs-mimalloc-sys =0.5.6',
                'native': 'Microsoft mimalloc v3.5.3',
                'wrapperArchiveSha256': '95bb29aa7aee255a84d956b4f5309a3f98993ae6e10836ae5e4e8519e960780f',
                'sysArchiveSha256': '0e6571957abe93757f0c6dd99182589054ace7da4102af4296ffcc8297eb3524'},
            'baselineSource': 'same application/toolchain/current lock; only selected musl allocator declaration '
                              'and dependency differ. dlmalloc0.2.14 uses its verified original lock record',
            'repeats': 4 if args.confirm else 2, 'order': 'AB/BA/AB/BA' if args.confirm else 'forward/reverse',
            'orderException': 'server H3 download4 includes GNU-system as a third variant, forward/reverse/'
                              'forward/reverse order' if args.confirm else None,
            'thpControl': 'same mimalloc binary, default versus MIMALLOC_ALLOW_THP=0; two forward/reverse 10s '
                          'repetitions of server H1 upload4/H2 upload4/H3 download4 and client H1 download4, '
                          'GNU peer environment unchanged; extra six-session server retention pass with THP off'
                          if args.confirm else None,
            'families': CONFIRM_CASES if args.confirm else 'all http1/http2/http3 download/upload at 1/4 streams',
            'experiments': ['server', 'client'], 'counterparts': {'server': 'fixed GNU-system client',
                'client': 'fixed GNU-system server' if args.confirm else 'fixed musl-system server'},
            'platform': list(os.uname()), 'logicalCpus': os.cpu_count(),
            'transparentHugePages': {name: Path('/sys/kernel/mm/transparent_hugepage', name).read_text().strip()
                                     for name in ('enabled', 'defrag', 'hpage_pmd_size')},
            'muslCCompiler': 'musl-gcc for every musl variant; earlier study used host GCC',
            'warmupMs': 500, 'measureMs': 10000 if args.confirm else 3000, 'streams': [1, 4],
            'cpuWindow': 'client GNU-time counters cover its entire process lifetime; server deltas bracket that same '
                         'client invocation, including preparation, warmup and drain; reported bytes/duration belong '
                         'to the receiver measurement, not an identical CPU window',
            'rss': 'server sampled every 50ms; client exact process high-water RSS via GNU time',
            'retention': 'one persistent server per baseline/candidate, two cycles of H2 upload/H1 clear download/'
                         'H3 upload at four streams; post-stage and three-second-idle RSS. Short mixed retention '
                         'check, not a long soak; retained measurement records or connection state can contribute '
                         'to growth, which alone does not establish a leak' if args.confirm else None,
            'limitation': 'shared hosted x86_64 loopback; fixed peer can cap throughput; compare same-run pairs only; no shaped link or ARM evidence'}, indent=2) + '\n')
        measure(binaries, environment, ports, args.confirm)
        if args.confirm:
            thp_control(binaries, environment, ports)
            retention(binaries, environment, ports)
    finally:
        shutil.rmtree(fixture.directory, ignore_errors=True)
        shutil.rmtree(ROOT / 'rust/target/allocator-study-build', ignore_errors=True)


if __name__ == '__main__':
    main()
