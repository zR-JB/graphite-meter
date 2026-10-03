"""Temporary hosted CPU profiles; binaries, TLS keys and raw stack-memory records stay private."""
from __future__ import annotations

import argparse
import gzip
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import tomllib

from .rust_allocator_study import ALLOCATOR, GNU, MUSL, ROOT, TELEMETRY, Fixture, run, server, unused_port

OUTPUT = ROOT / 'rust/target/server-profile-results'
BUILD = ROOT / 'rust/target/server-profile-build'
CASES = (('h1-clear', 'http1'), ('h1-tls', 'http1'), ('h2-tls', 'http2'), ('h3-quic', 'http3'))


def command_output(command: list[str], environment: dict[str, str], *, input_text: str | None = None) -> str:
    return subprocess.run(command, env=environment, input=input_text, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, check=True, timeout=120).stdout


def build_child(package: str, target: str) -> None:
    from scripts.legal.rust import build
    build(argparse.Namespace(repo=ROOT, package=package, target=target, profile='release',
                             out=BUILD / f'notices-{package}-{target}', reviews=ROOT / 'legal/rust-reviewed-components.json',
                             supplement=None, browser_scan=None, version='0.0.0-server-profile',
                             review_template=False, local=True, development=True))


def build_binaries(environment: dict[str, str]) -> dict[str, Path]:
    main = ROOT / 'rust/client/src/main.rs'
    original = main.read_bytes()
    marker = b'    let last = finished.map(|snapshot| snapshot.phase);'
    assert original.count(ALLOCATOR) == original.count(marker) == 1
    assert (ROOT / 'rust/server/src/main.rs').read_bytes().count(ALLOCATOR) == 1
    locked = tomllib.loads((ROOT / 'rust/Cargo.lock').read_text())['package']
    for name in ('rustfs-mimalloc', 'rustfs-mimalloc-sys'):
        assert [entry['version'] for entry in locked if entry['name'] == name] == ['0.5.6']
    binaries = {}
    try:
        main.write_bytes(original.replace(ALLOCATOR, b'').replace(marker, TELEMETRY + marker))
        for label, package, target in (('server', 'server', MUSL), ('client', 'client', GNU),
                                       ('server-gnu', 'server', GNU)):
            own = environment | ({'CARGO_PROFILE_RELEASE_DEBUG': '2', 'CARGO_PROFILE_RELEASE_STRIP': 'none'}
                                 if package == 'server' else {})
            run([sys.executable, '-m', 'scripts.ci.rust_server_profile', '--build', label], own,
                OUTPUT / f'build-{label}.log')
            binary = BUILD / f'profile-{label}'
            shutil.copy2(BUILD / target / 'release' / f'graphite-meter-{package}', binary)
            assert b'UNREVIEWED DEVELOPMENT BUILD' in binary.read_bytes()
            binaries[label] = binary
    finally:
        main.write_bytes(original)
    return binaries


def tools(environment: dict[str, str]) -> tuple[str, str, str]:
    candidates = [shutil.which('perf'), *map(str, sorted(Path('/usr/lib/linux-tools').glob('*/perf')))]
    for candidate in candidates:
        if candidate is None:
            continue
        try:
            features = command_output([candidate, 'version', '--build-options'], environment)
        except (OSError, subprocess.CalledProcessError):
            continue
        if re.search(r'(?:dwarf|libdw|libunwind)[^\n:]*:\s*\[\s*on\s*\]', features, re.IGNORECASE):
            perf = candidate
            break
    else:
        raise RuntimeError('install a compatible Linux perf with DWARF/libdw or libunwind support on the hosted runner')
    demangler = next((path for name in ('llvm-cxxfilt', 'llvm-cxxfilt-18', 'llvm-cxxfilt-17', 'llvm-cxxfilt-16')
                      if (path := shutil.which(name))), None)
    if demangler is None or command_output([demangler, '--format=auto', '--no-strip-underscore'], environment,
                                          input_text='_RNvC1a4main\n').strip() != 'a::main':
        raise RuntimeError('the hosted runner needs llvm-cxxfilt with working Rust v0 demangling')
    try:
        run(['sudo', '-n', perf, 'stat', '-e', 'cpu-clock', '--', '/bin/sleep', '0.05'], environment,
            OUTPUT / 'perf-capability.log', timeout=15)
    except subprocess.CalledProcessError as error:
        raise RuntimeError('hosted perf cannot collect server-context user/kernel CPU; see perf-capability.log') from error
    return perf, demangler, features


def stats(pid: int) -> dict[str, int | float]:
    raw = Path(f'/proc/{pid}/stat').read_text()
    fields = raw[raw.rfind(')') + 2:].split()
    ticks = os.sysconf('SC_CLK_TCK')
    return {'userSeconds': int(fields[11]) / ticks, 'systemSeconds': int(fields[12]) / ticks,
            'rssBytes': int(fields[21]) * os.sysconf('SC_PAGE_SIZE'),
            'minorFaults': int(fields[7]), 'majorFaults': int(fields[9])}


def classify(symbol: str, source: str, dso: str) -> tuple[str, str]:
    if symbol.startswith('[k]') or 'kernel.kallsyms' in dso:
        return 'kernel', 'execution context'
    source_rules = (
        ('allocator', ('/dlmalloc-', '/malloc/', '/mimalloc-', '/libmimalloc-sys-',
                       '/rustfs-mimalloc-', '/rustfs-mimalloc-sys-', '/c_src/mimalloc/', '/mimalloc/v2/', '/mimalloc/v3/')),
        ('crypto', ('/ring-', '/argon2-', '/sha2-', '/hmac-')),
        ('tls', ('/rustls-', '/rustls/', '/tokio-rustls-')),
        ('transport', ('/noq-', '/h2-', '/hyper-', '/hyper-util-',
                       '/tokio-tungstenite-', '/tungstenite-')),
        ('application', ('/rust/server/', '/rust/core/', '/rust/net/', '/rust/http3/')),
        ('rust-standard-library', ('/library/std/', '/library/core/', '/library/alloc/', '/compiler_builtins-')),
        ('runtime', ('/tokio-', '/mio-', '/socket2-', '/rustix-')),
        ('libc', ('/musl/', '/glibc/')),
    )
    for bucket, fragments in source_rules:
        if any(fragment in source for fragment in fragments):
            return bucket, 'source location'
    if '/.cargo/registry/src/' in source or '/.cargo/git/checkouts/' in source:
        return 'other-dependencies', 'source location'
    clean = re.sub(r'^\[[^]]+\]\s*', '', symbol)
    if clean in {'malloc', 'calloc', 'realloc', 'free', 'aligned_alloc', 'posix_memalign',
                 '__libc_malloc', '__libc_free', '__malloc_alloc_meta', 'alloc_slot'}:
        return 'allocator', 'native allocator symbol'
    if clean.startswith(('mi_', '_mi_')):
        return 'allocator', 'mimalloc native symbol'
    if re.match(r'(?:ring_core_\w+_|(?:aes|chacha|poly1305|sha256|sha512|gcm)_)(?:[\w]+)', clean):
        return 'crypto', 'native crypto symbol'
    for bucket, prefixes in (
        ('allocator', ('dlmalloc::', 'mimalloc::', 'libmimalloc_sys::', 'rustfs_mimalloc::', 'rustfs_mimalloc_sys::')),
        ('crypto', ('ring::', 'argon2::', 'sha2::', 'hmac::')), ('tls', ('rustls::', 'tokio_rustls::')),
        ('transport', ('noq::', 'noq_proto::', 'h2::', 'hyper::')),
        ('application', ('graphite_meter_server::', 'graphite_meter_core::', 'graphite_meter_net::', 'graphite_meter_http3::')),
        ('rust-standard-library', ('std::', 'core::', 'alloc::', 'compiler_builtins::')),
        ('runtime', ('tokio::', 'mio::', 'rustix::', 'socket2::')),
    ):
        if clean.removeprefix('<').startswith(prefixes):
            return bucket, 'symbol only; inline ownership unavailable'
    if 'libc.so' in dso:
        return 'libc', 'shared library'
    # A C ABI memory symbol can belong to musl or compiler-builtins; do not guess its source owner.
    if clean in {'memcpy', 'memmove', 'memset', 'memcmp', 'bcmp', 'strlen'}:
        return 'native-runtime-unattributed', 'C ABI symbol; implementation owner unavailable'
    return 'unknown', 'source/symbol owner unavailable'


def symbolize(perf: str, demangler: str, raw: Path, cell: str, environment: dict[str, str],
              start: int, stop: int) -> dict:
    window = ','.join(f'{value // 1_000_000_000}.{value % 1_000_000_000:09d}' for value in (start, stop))
    base = ['sudo', '-n', perf, 'report', '-i', str(raw), '--stdio', '--stdio-color=never', '--inline',
            '--full-source-path', '--percent-limit', '0', '--time', window]
    table = command_output([*base, '--no-children', '--no-demangle', '-g', 'none', '-t', '\t',
                            '-F', 'period,sample,dso,symbol,srcline', '-s', 'dso,symbol,srcline'], environment)
    with gzip.open(OUTPUT / f'{cell}-self-table.txt.gz', 'wt') as output:
        output.write(table)
    rows: list[dict] = []
    for line in table.splitlines():
        if not line.strip() or line.lstrip().startswith('#'):
            continue
        fields = [field.strip() for field in line.split('\t')]
        if len(fields) != 5 or not fields[0].isdigit() or not fields[1].isdigit():
            raise RuntimeError(f'unexpected perf self histogram row: {line!r}')
        period, count, dso, symbol, source = fields
        rows.append({'period': int(period), 'samples': int(count), 'dso': dso, 'rawSymbol': symbol, 'source': source})
    if not rows or not sum(row['period'] for row in rows):
        raise RuntimeError(f'no CPU samples collected for {cell}')
    names = [re.sub(r'^\[[^]]+\]\s*', '', row['rawSymbol']) for row in rows]
    demangled = command_output([demangler, '--format=auto', '--no-strip-underscore'], environment,
                               input_text='\n'.join(names) + '\n').splitlines()
    if len(demangled) != len(rows):
        raise RuntimeError('Rust demangler changed the number of histogram rows')
    buckets: dict[str, int] = {}
    evidence: dict[str, dict[str, int]] = {}
    for row, name in zip(rows, demangled, strict=True):
        context = '[k] ' if row['rawSymbol'].startswith('[k]') else ''
        row['symbol'] = context + name
        row['bucket'], row['attribution'] = classify(row['symbol'], row['source'], row['dso'])
        buckets[row['bucket']] = buckets.get(row['bucket'], 0) + row['period']
        own = evidence.setdefault(row['bucket'], {})
        own[row['attribution']] = own.get(row['attribution'], 0) + row['period']
    total = sum(buckets.values())
    for row in rows:
        row['percent'] = row['period'] * 100 / total
    for label, command in (
        ('self', [*base, '--no-children', '-g', 'none', '-F', 'overhead,period,sample,dso,symbol,srcline']),
        ('inclusive', [*base, '--children', '-g', 'graph,0.5,caller']),
        ('script', ['sudo', '-n', perf, 'script', '-i', str(raw), '--inline', '--full-source-path', '--demangle',
                    '--time', window, '--show-lost-events',
                    '-F', 'comm,pid,tid,time,event,period,ip,sym,dso,srcline,misc']),
    ):
        data = command_output(command, environment)
        with gzip.open(OUTPUT / f'{cell}-{label}.txt.gz', 'wt') as output:
            output.write(data)
    lost = re.search(r'^#\s*Total Lost Samples:\s*(\S+)', table, re.MULTILINE)
    return {'totalPeriod': total, 'samples': sum(row['samples'] for row in rows),
            'reportedLostSamples': lost[1] if lost else None,
            'sourceAttributedPeriod': sum(row['period'] for row in rows if row['attribution'] == 'source location'),
            'symbolOnlyPeriod': sum(row['period'] for row in rows if 'symbol' in row['attribution']),
            'buckets': {key: {'period': value, 'percent': value * 100 / total, 'attributionPeriods': evidence[key]}
                        for key, value in buckets.items()},
            'self': sorted(rows, key=lambda row: row['period'], reverse=True),
            'method': 'exclusive self ownership at execution IP; application includes all owned Rust crates; each period once'}


def calibrate(perf: str, demangler: str, environment: dict[str, str]) -> None:
    # Exercise the complete report/parser before fat builds; calibration never enters case results.
    with tempfile.TemporaryDirectory(prefix='perf-calibration-', dir=ROOT / 'rust/target') as directory:
        raw = Path(directory) / 'perf.data'
        start = time.monotonic_ns()
        run(['sudo', '-n', perf, 'record', '-e', 'cpu-clock', '-F', '99', '--call-graph', 'dwarf,16384',
             '--clockid', 'mono', '--no-buildid-cache', '-o', str(raw), '--', sys.executable, '-c',
             'import time\nstop = time.monotonic() + 1\nwhile time.monotonic() < stop:\n    pass\n'],
            environment, OUTPUT / 'perf-calibration.log', timeout=15)
        summary = symbolize(perf, demangler, raw, 'calibration', environment, start, time.monotonic_ns())
        print(f'perf calibration: {summary["samples"]} samples; report parser and symbolizer passed', flush=True)


def profile_case(perf: str, demangler: str, binaries: dict[str, Path], environment: dict[str, str],
                 ports: dict[str, int], kind: str, protocol: str, stage: str, streams: int,
                 *, server_key: str = 'server') -> dict:
    cell = f'{kind}-{stage}-{streams}' + ('-gnu-reference' if server_key == 'server-gnu' else '')
    origin = f'{"http" if kind == "h1-clear" else "https"}://127.0.0.1:{ports[kind]}'
    raw = BUILD / f'{cell}.perf.data'
    try:
        with server(binaries[server_key], environment, ports['h1-clear'], OUTPUT / f'{cell}-server.log') as backend:
            with (OUTPUT / f'{cell}-perf.log').open('w') as log:
                recorder = subprocess.Popen(['sudo', '-n', perf, 'record', '-e', 'cpu-clock', '-F', '99',
                    '--call-graph', 'dwarf,16384', '--clockid', 'mono', '--no-buildid-cache',
                    '-o', str(raw), '-p', str(backend.pid)], env=environment, stdout=log,
                    stderr=subprocess.STDOUT, start_new_session=True)
                try:
                    time.sleep(0.2)
                    if recorder.poll() is not None:
                        raise RuntimeError(f'perf attachment failed; see {cell}-perf.log')
                    first = stats(backend.pid)
                    peak = first['rssBytes']
                    start = time.monotonic_ns()
                    command = [str(binaries['client']), '-report', '-url', f'http://127.0.0.1:{ports["h1-clear"]}',
                        '-throughput-origin', origin, '-throughput-protocol', protocol,
                        '-throughput-transport', 'fetch-stream', '-stages', stage, '-streams', str(streams),
                        '-loaded-latency=false', '-warmup', '500ms', f'-{stage}-duration', '10s', '-insecure']
                    with (OUTPUT / f'{cell}-client.log').open('w') as output:
                        client = subprocess.Popen(command, env=environment, stdout=output, stderr=subprocess.STDOUT,
                                                  start_new_session=True)
                        try:
                            deadline = time.monotonic() + 35
                            while client.poll() is None:
                                peak = max(peak, stats(backend.pid)['rssBytes'])
                                if time.monotonic() >= deadline:
                                    raise TimeoutError(f'profile client timed out: {cell}')
                                if raw.exists() and raw.stat().st_size > 256 * 1024**2:
                                    raise RuntimeError(f'profile exceeded 256 MiB: {cell}')
                                time.sleep(0.05)
                            if client.returncode:
                                raise RuntimeError(f'profile client failed: {cell}')
                        finally:
                            if client.poll() is None:
                                os.killpg(client.pid, signal.SIGKILL)
                                client.wait(timeout=5)
                    stop = time.monotonic_ns()
                    last = stats(backend.pid)
                finally:
                    if recorder.poll() is None:
                        subprocess.run(['sudo', '-n', '/bin/kill', '-INT', str(recorder.pid)], check=True, timeout=10)
                        try:
                            recorder.wait(timeout=15)
                        except subprocess.TimeoutExpired:
                            subprocess.run(['sudo', '-n', '/bin/kill', '-KILL', '--', f'-{recorder.pid}'],
                                           check=True, timeout=10)
                            recorder.wait(timeout=5)
                    if recorder.returncode not in (0, 130, -signal.SIGINT):
                        raise RuntimeError(f'perf capture failed: {cell}')
            profile = symbolize(perf, demangler, raw, cell, environment, start, stop)
        text = (OUTPUT / f'{cell}-client.log').read_text()
        transfer = [json.loads(record) for record in re.findall(r'^GM_ALLOCATOR_RESULT (.+)$', text, re.MULTILINE)]
        expected = {'download', 'upload'} if stage == 'bidirectional' else {stage}
        if {record['direction'] for record in transfer} != expected or len(transfer) != len(expected):
            raise RuntimeError(f'missing final receiver results: {cell}')
        if not all(record['meanBytesPerSec'] > 0 and record['totalBytes'] > 0 and record['elapsedNanos'] > 0
                   for record in transfer):
            raise RuntimeError(f'invalid final receiver results: {cell}')
        selected = re.findall(r'^GM_ALLOCATOR_PATH (.+)$', text, re.MULTILINE)
        if len(selected) != 1 or not all(value in selected[0] for value in (protocol.title(), 'FetchStream', origin)):
            raise RuntimeError(f'wrong negotiated profile path: {selected}')
        return {'case': cell, 'serverVariant': 'gnu-system' if server_key == 'server-gnu' else 'musl-mimalloc',
                'serverTarget': GNU if server_key == 'server-gnu' else MUSL, 'kind': kind, 'protocol': protocol, 'stage': stage, 'streams': streams,
                'origin': origin, 'selected': selected[0], 'transfer': transfer, 'profile': profile,
                'window': {'startMonotonicNanos': start, 'stopMonotonicNanos': stop,
                           'scope': 'entire client lifetime: preparation, warmup, measurement and drain'},
                'server': {key: last[key] - first[key] for key in ('userSeconds', 'systemSeconds', 'minorFaults', 'majorFaults')}
                          | {'peakRssBytes': peak}}
    finally:
        raw.unlink(missing_ok=True)
        raw.with_suffix(raw.suffix + '.old').unlink(missing_ok=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build', choices=('server', 'client', 'server-gnu'))
    args = parser.parse_args()
    if args.build:
        package, target = {'server': ('graphite-meter-server', MUSL), 'client': ('graphite-meter-client', GNU),
                           'server-gnu': ('graphite-meter-server', GNU)}[args.build]
        build_child(package, target)
        return
    OUTPUT.mkdir(parents=True, exist_ok=True)
    workspace = tomllib.loads((ROOT / 'rust/Cargo.toml').read_text())
    profile = workspace['profile']['release']
    channel = tomllib.loads((ROOT / 'rust/rust-toolchain.toml').read_text())['toolchain']['channel']
    allocator = workspace['workspace']['dependencies']['rustfs-mimalloc']
    allocator_features = allocator.get('features', []) if isinstance(allocator, dict) else []
    allocator_packages = [{key: entry[key] for key in ('name', 'version', 'source', 'checksum')}
                          for entry in tomllib.loads((ROOT / 'rust/Cargo.lock').read_text())['package']
                          if entry['name'] in ('rustfs-mimalloc', 'rustfs-mimalloc-sys')]
    assert (profile['opt-level'], profile['lto'], profile['codegen-units']) == (3, 'fat', 1)
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('GM_', 'CARGO_PROFILE_', 'MIMALLOC_', 'MALLOC_')) and not key.endswith('RUSTFLAGS')}
    environment.update(LC_ALL='C', RUSTUP_TOOLCHAIN=channel, CARGO_INCREMENTAL='0', CARGO_TARGET_DIR=str(BUILD),
                       CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER='x86_64-linux-gnu-gcc',
                       CC_x86_64_unknown_linux_musl='musl-gcc')
    assert not any(environment.get(key) for key in ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER'))
    perf, demangler, features = tools(environment)
    calibrate(perf, demangler, environment)
    fixture = Fixture('server-profile-')
    try:
        run(['rustup', 'target', 'add', MUSL], environment, OUTPUT / 'target-setup.log')
        run(['cargo', 'fetch', '--locked'], environment, OUTPUT / 'cargo-fetch.log')
        binaries = build_binaries(environment)
        _, cert, key = fixture.identity()
        ports = {kind: unused_port() for kind, _ in CASES}
        ports['h3-quic'] = unused_port(kind=socket.SOCK_DGRAM)
        environment.update(GM_AUTH_MODE='off', GM_H1_ADDR=f'127.0.0.1:{ports["h1-clear"]}',
            GM_H1_TLS_ADDR=f'127.0.0.1:{ports["h1-tls"]}', GM_H2_ADDR=f'127.0.0.1:{ports["h2-tls"]}',
            GM_H3_ADDR=f'127.0.0.1:{ports["h3-quic"]}', GM_TLS_CERT=str(cert), GM_TLS_KEY=str(key))
        metadata = {'profile': profile, 'serverOverrides': {'debug': 2, 'strip': 'none'},
            'allocator': {'wrapper': 'rustfs-mimalloc 0.5.6', 'native': 'mimalloc 3.5.3',
                          'features': allocator_features, 'packages': allocator_packages},
            'serverTarget': MUSL, 'clientTarget': GNU, 'expectedProfiles': 25,
            'gnuReference': {'serverTarget': GNU, 'allocator': 'glibc system', 'case': 'h3-quic-download-4-gnu-reference'},
            'sampleEvent': 'cpu-clock', 'sampleHz': 99,
            'perfFeatures': features, 'demangler': demangler, 'platform': list(os.uname()),
            'lscpu': command_output(['lscpu'], environment), 'rustc': command_output(['rustc', f'+{channel}', '-vV'], environment),
            'buildFlags': {key: value for key, value in environment.items() if key.startswith(('CARGO_', 'CC_'))},
            'buildIds': {package: command_output(['readelf', '-n', str(binary)], environment)
                         for package, binary in binaries.items()},
            'limits': ['full fat LTO can cross-inline owners; missing precompiled std/libc/assembly DWARF stays unresolved',
                       'symbol-only attribution does not recover inlined owners',
                       'tls means rustls/tokio-rustls protocol, framing and provider glue; '
                       'crypto means ring/hash/native crypto owners',
                       'cpu-clock measures on-CPU execution; sleeping and lock-wait latency are excluded',
                       'kernel samples are server-context CPU, excluding unrelated ksoftirqd/client work',
                       'inclusive callchains overlap; exclusive categories count each sampled period once',
                       'one profile per case on shared hosted loopback; no link shaping or ARM evidence']}
        (OUTPUT / 'metadata.json').write_text(json.dumps(metadata, indent=2) + '\n')
        rows = []
        for kind, protocol in CASES:
            for stage in ('download', 'upload', 'bidirectional'):
                for streams in (1, 4):
                    row = profile_case(perf, demangler, binaries, environment, ports, kind, protocol, stage, streams)
                    rows.append(row)
                    (OUTPUT / 'results.json').write_text(json.dumps(rows, indent=2) + '\n')
                    print(json.dumps({key: row[key] for key in ('case', 'transfer', 'server')}), flush=True)
        row = profile_case(perf, demangler, binaries, environment, ports, 'h3-quic', 'http3', 'download', 4,
                           server_key='server-gnu')
        rows.append(row)
        (OUTPUT / 'results.json').write_text(json.dumps(rows, indent=2) + '\n')
        print(json.dumps({key: row[key] for key in ('case', 'transfer', 'server')}), flush=True)
    finally:
        shutil.rmtree(fixture.directory, ignore_errors=True)
        shutil.rmtree(BUILD, ignore_errors=True)


if __name__ == '__main__':
    main()
