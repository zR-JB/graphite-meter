"""Temporary paired QUIC receive comparison; diagnostic binaries and TLS keys are never uploaded."""
from __future__ import annotations

from contextlib import contextmanager, nullcontext
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
OUTPUT = ROOT / 'rust/target/quic-receive-study-results'
BUILD = ROOT / 'rust/target/quic-receive-study-build'
MUSL = 'x86_64-unknown-linux-musl'
GNU = 'x86_64-unknown-linux-gnu'
BASELINE = 'b34035dcfc96789f8269a5e080a6513e981f4904'
VERSION = '0.0.0-quic-receive-study'
VARIANTS = ('baseline', 'candidate')
CASES = (('http3', 'upload', 4), ('http3', 'download', 4), ('http2', 'upload', 4),
         ('http3', 'upload', 1))
REPEATS = 8
CGROUP = Path('/sys/fs/cgroup/gm-quic-study')
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



# Temporary hosted-only source edits. Never apply to clean comparison builds.
EDITS = {
    'noq-proto/src/connection/buffer_budget.rs': [
        (
            'pub(super) const PACKET_QUEUE_BYTES: u64 = 16 * 1024 * 1024;\n',
            'pub(super) const PACKET_QUEUE_BYTES: u64 = 16 * 1024 * 1024;\n\n'
            'static NEXT_BUDGET_ID: AtomicUsize = AtomicUsize::new(1);\n',
        ),
        (
            '    refused: AtomicBool,\n',
            '    refused: AtomicBool,\n'
            '    peak: AtomicUsize,\n'
            '    id: usize,\n'
            "    kind: &'static str,\n",
        ),
        (
            '            refused: AtomicBool::new(false),\n',
            '            refused: AtomicBool::new(false),\n'
            '            peak: AtomicUsize::new(0),\n'
            '            id: NEXT_BUDGET_ID.fetch_add(1, AtomicOrdering::Relaxed),\n'
            '            kind: "other",\n',
        ),
        (
            '    pub(super) fn for_receive(window: u64, shared: Option<&Arc<dyn SharedBudget>>) -> Arc<Self> {\n'
            '        Self::new(window.saturating_mul(3), shared)\n'
            '    }\n',
            "    pub(super) fn named(kind: &'static str, limit: u64, shared: Option<&Arc<dyn SharedBudget>>) -> Arc<Self> {\n"
            '        let mut budget = Self::new(limit, shared);\n'
            '        Arc::get_mut(&mut budget).expect("new budget has one owner").kind = kind;\n'
            '        budget\n'
            '    }\n\n'
            '    pub(super) fn for_receive(window: u64, shared: Option<&Arc<dyn SharedBudget>>) -> Arc<Self> {\n'
            '        Self::named("reassembly", window.saturating_mul(3), shared)\n'
            '    }\n',
        ),
        (
            '                Ok(_) => {\n'
            '                    return Ok(Allocation {\n',
            '                Ok(_) => {\n'
            '                    self.peak.fetch_max(next, AtomicOrdering::Relaxed);\n'
            '                    return Ok(Allocation {\n',
        ),
        (
            '    fn drop(&mut self) {\n'
            '        self.refund(self.floor);\n',
            '    fn drop(&mut self) {\n'
            '        if self.kind != "other" {\n'
            '            eprintln!(\n'
            '                r#"GM_QUIC_BUFFER_BUDGET {{"kind":"{}","id":{},"peak_used_bytes":{},"used_at_drop_bytes":{},"limit_at_drop_bytes":{},"floor_bytes":{}}}"#,\n'
            '                self.kind, self.id, self.peak.load(AtomicOrdering::Relaxed), self.used(),\n'
            '                self.limit.load(AtomicOrdering::Relaxed), self.floor,\n'
            '            );\n'
            '        }\n'
            '        self.refund(self.floor);\n',
        ),
    ],
    'noq-proto/src/connection/mod.rs': [
        (
            '            buffer_budget::BufferBudget::new(buffer_budget::PACKET_QUEUE_BYTES, shared);\n',
            '            buffer_budget::BufferBudget::named("packet_queue", buffer_budget::PACKET_QUEUE_BYTES, shared);\n',
        ),
    ],
}


def source_revision(revision: str) -> str:
    return f'git+https://github.com/zR-JB/noq?rev={revision}#{revision}'


def build_variants(environment: dict[str, str], candidate: str) -> dict[str, Path]:
    main, manifest, lock = (ROOT / path for path in
                            ('rust/client/src/main.rs', 'rust/Cargo.toml', 'rust/Cargo.lock'))
    original = {path: path.read_bytes() for path in (main, manifest, lock)}
    workspace = tomllib.loads(original[manifest].decode())
    assert workspace['patch']['crates-io']['noq']['git'] == 'https://github.com/zR-JB/noq'
    initial = workspace['patch']['crates-io']['noq']['rev']
    assert re.fullmatch(r'[0-9a-f]{40}', initial)
    assert original[manifest].count(f'rev = "{initial}"'.encode()) == 1
    marker = b'    let last = finished.map(|snapshot| snapshot.phase);'
    assert original[main].count(ALLOCATOR) == original[main].count(marker) == 1
    assert (ROOT / 'rust/server/src/main.rs').read_bytes().count(ALLOCATOR) == 1
    packages = tomllib.loads(original[lock].decode())['package']
    for name in ('rustfs-mimalloc', 'rustfs-mimalloc-sys'):
        entry, = [item for item in packages if item['name'] == name]
        assert entry['version'] == '0.5.6'
    external = [item for item in packages if item.get('source') == source_revision(initial)]
    assert {item['name'] for item in external} == {'noq', 'noq-proto', 'noq-udp'} and len(external) == 3
    binaries: dict[str, Path] = {}
    try:
        main.write_bytes(original[main].replace(marker, TELEMETRY + marker))
        for label, package, target, revision in (
                ('client', 'client', GNU, BASELINE), ('baseline', 'server', MUSL, BASELINE),
                ('candidate', 'server', MUSL, candidate), ('diagnostic', 'server', MUSL, candidate)):
            manifest.write_bytes(original[manifest].replace(f'rev = "{initial}"'.encode(),
                                                            f'rev = "{revision}"'.encode()))
            lock.write_bytes(original[lock].replace(source_revision(initial).encode(),
                                                    source_revision(revision).encode()))
            changed = tomllib.loads(lock.read_text())['package']
            assert [{**item, 'source': source_revision(initial)}
                    if item.get('source') == source_revision(revision) else item for item in changed] == packages
            with diagnostic_patch(environment) if label == 'diagnostic' else nullcontext():
                if label == 'diagnostic':
                    run(['cargo', 'clean', '--release', '--target', MUSL, '-p', 'noq-proto',
                         '-p', 'noq', '-p', 'graphite-meter-server'], environment, OUTPUT / 'diagnostic-clean.log')
                run(['cargo', 'build', '--locked', '--release', '--target', target,
                     '-p', f'graphite-meter-{package}'], environment, OUTPUT / f'build-{label}.log')
                binary = BUILD / 'binaries' / label
                binary.parent.mkdir(exist_ok=True)
                shutil.copy2(BUILD / target / 'release' / f'graphite-meter-{package}', binary)
                run([str(binary), '-version' if package == 'client' else '--version'],
                    environment, OUTPUT / f'version-{label}.log', timeout=10)
                assert VERSION in (OUTPUT / f'version-{label}.log').read_text()
                assert (b'GM_QUIC_BUFFER_BUDGET' in binary.read_bytes()) == (label == 'diagnostic')
                binaries[label] = binary
    finally:
        for path, content in original.items():
            path.write_bytes(content)
    assert binaries['diagnostic'].read_bytes() != binaries['candidate'].read_bytes()
    return binaries


@contextmanager
def diagnostic_patch(environment: dict[str, str]):
    log = OUTPUT / 'candidate-metadata.log'
    run(['cargo', 'metadata', '--locked', '--format-version', '1'], environment, log, timeout=120)
    records = [line for line in log.read_text().splitlines() if line.startswith('{')]
    assert len(records) == 1
    packages = json.loads(records[0])['packages']
    entry, = [item for item in packages if item['name'] == 'noq-proto']
    revision = tomllib.loads((ROOT / 'rust/Cargo.toml').read_text())['patch']['crates-io']['noq']['rev']
    assert entry['source'] == source_revision(revision)
    checkout = Path(entry['manifest_path']).resolve().parents[1]
    cargo_home = Path(environment.get('CARGO_HOME', str(Path.home() / '.cargo'))).resolve()
    assert checkout.is_relative_to(cargo_home / 'git/checkouts')
    originals: dict[Path, bytes] = {}
    try:
        for relative, changes in EDITS.items():
            path = checkout / relative
            originals[path] = path.read_bytes()
            content = originals[path].decode()
            for before, after in changes:
                assert content.count(before) == 1, relative
                content = content.replace(before, after)
            path.write_text(content)
        yield
    finally:
        for path, content in originals.items():
            path.write_bytes(content)


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
def server(binary: Path, environment: dict[str, str], port: int, log: Path, group: Path | None):
    def enter_group() -> None:
        assert group is not None
        (group / 'cgroup.procs').write_text('0')
    with log.open('w') as output:
        process = subprocess.Popen([str(binary)], env=environment, stdout=output, stderr=subprocess.STDOUT,
                                   start_new_session=True, preexec_fn=enter_group if group else None)
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
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
                    if not failed:
                        raise RuntimeError(f'server shutdown timed out; see {log}')
            if process.returncode != 0 and not failed:
                raise RuntimeError(f'server failed to shut down cleanly; see {log}')


def transfer(backend: subprocess.Popen, binary: Path, environment: dict[str, str], ports: dict[str, int],
             cell: str, kind: str, direction: str, streams: int, seconds: int, group: Path | None) -> dict:
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
    peaks = first_memory.copy()
    group_start = cgroup_memory(group)
    group_peaks = group_start.copy()
    with (OUTPUT / f'{cell}-client.log').open('w') as output:
        client = subprocess.Popen(command, env=environment, stdout=output, stderr=subprocess.STDOUT,
                                  start_new_session=True)
        try:
            deadline = time.monotonic() + seconds + 22
            while client.poll() is None:
                for key, value in memory(backend.pid).items():
                    peaks[key] = max(peaks.get(key, 0), value)
                for key, value in cgroup_memory(group).items():
                    group_peaks[key] = max(group_peaks.get(key, 0), value)
                if time.monotonic() >= deadline:
                    raise TimeoutError(f'client cell timed out: {cell}')
                time.sleep(0.1)
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
    row['server'].update(peakRssBytes=max(peaks['Rss'], last['rssBytes']), postStageRssBytes=last['rssBytes'])
    row['serverMemoryStartBytes'] = first_memory
    row['serverMemoryEndBytes'] = memory(backend.pid)
    for key, value in row['serverMemoryEndBytes'].items():
        peaks[key] = max(peaks.get(key, 0), value)
    row['serverMemorySampledPeakBytes'] = peaks
    row['serverCgroupStartBytes'] = group_start
    row['serverCgroupEndBytes'] = cgroup_memory(group)
    row['serverCgroupSampledPeakBytes'] = group_peaks
    gib = result['totalBytes'] / 2**30
    row['server']['cpuSecondsPerReportedGiB'] = row['server']['cpuSeconds'] / gib
    row['client']['cpuSecondsPerReportedGiB'] = (row['client']['userSeconds'] + row['client']['systemSeconds']) / gib
    return row


def publish(rows: list[dict], row: dict, name: str) -> None:
    rows.append(row)
    (OUTPUT / name).write_text(json.dumps(rows, indent=2) + '\n')
    print(json.dumps(row), flush=True)




def setup_cgroup(environment: dict[str, str]) -> tuple[bool, str]:
    if not Path('/sys/fs/cgroup/cgroup.controllers').exists():
        return False, 'cgroup v2 is unavailable'
    if CGROUP.exists():
        return False, 'fixed study cgroup already exists; refusing to reuse it'
    try:
        run(['sudo', '-n', 'mkdir', str(CGROUP)], environment, OUTPUT / 'cgroup-mkdir.log', timeout=10)
        run(['sudo', '-n', 'chown', f'{os.getuid()}:{os.getgid()}', str(CGROUP),
             str(CGROUP / 'cgroup.procs'), str(CGROUP / 'cgroup.subtree_control')],
            environment, OUTPUT / 'cgroup-chown.log', timeout=10)
        if 'memory' not in (CGROUP / 'cgroup.controllers').read_text().split():
            raise RuntimeError('memory controller is not delegated to the study subtree')
        (CGROUP / 'cgroup.subtree_control').write_text('+memory')
        probe = CGROUP / 'migration-probe'
        probe.mkdir()
        try:
            with (OUTPUT / 'cgroup-probe.log').open('w') as log:
                subprocess.run(['/bin/true'], env=environment, stdout=log, stderr=subprocess.STDOUT,
                               preexec_fn=lambda: (probe / 'cgroup.procs').write_text('0'),
                               check=True, timeout=10)
        finally:
            probe.rmdir()
        return True, 'server-only cgroup entered before exec; client remains outside'
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        if CGROUP.exists():
            try:
                run(['sudo', '-n', 'rmdir', str(CGROUP)], environment, OUTPUT / 'cgroup-cleanup.log', timeout=10)
            except (OSError, subprocess.SubprocessError) as cleanup:
                return False, f'{error}; cleanup also failed: {cleanup}'
        return False, str(error)


@contextmanager
def cell_cgroup(enabled: bool, cell: str):
    group = CGROUP / cell if enabled else None
    if group:
        group.mkdir()
    try:
        yield group
    finally:
        if group:
            group.rmdir()


def cgroup_memory(group: Path | None) -> dict[str, int]:
    if group is None:
        return {}
    values = {key: int(value) for line in (group / 'memory.stat').read_text().splitlines()
              for key, value in [line.split()] if key in ('sock', 'kernel', 'anon', 'file')}
    values.update(current=int((group / 'memory.current').read_text()),
                  peak=int((group / 'memory.peak').read_text()))
    return values


def budget_reports(log: Path) -> list[dict]:
    reports = [json.loads(raw) for raw in re.findall(r'^GM_QUIC_BUFFER_BUDGET (.+)$', log.read_text(), re.MULTILINE)]
    assert reports and {item['kind'] for item in reports} == {'packet_queue', 'reassembly'}
    assert len({item['id'] for item in reports}) == len(reports)
    for item in reports:
        assert item['used_at_drop_bytes'] == 0
        assert all(isinstance(item[key], int) and item[key] >= 0 for key in
                   ('id', 'peak_used_bytes', 'used_at_drop_bytes', 'limit_at_drop_bytes', 'floor_bytes'))
    return reports


def measure(binaries: dict[str, Path], environment: dict[str, str], ports: dict[str, int],
            hashes: dict[str, str], cgroup_enabled: bool) -> None:
    results: list[dict] = []
    diagnostic: list[dict] = []
    server_environment = environment | {'MIMALLOC_ALLOW_THP': '0'}
    for repeat in range(REPEATS):
        offset = repeat % len(CASES)
        order = VARIANTS if repeat % 2 == 0 else VARIANTS[::-1]
        for case_position, (kind, direction, streams) in enumerate(CASES[offset:] + CASES[:offset], 1):
            for variant_position, variant in enumerate(order, 1):
                cell = f'{repeat + 1}-{kind}-{direction}-{streams}-{variant}'
                log = OUTPUT / f'{cell}-server.log'
                with cell_cgroup(cgroup_enabled, cell) as group:
                    with server(binaries[variant], server_environment, ports['http1'], log, group) as backend:
                        row = transfer(backend, binaries['client'], environment, ports,
                                       cell, kind, direction, streams, 10, group)
                    row['serverCgroupAfterShutdownBytes'] = cgroup_memory(group)
                row.update(variant=variant, repeat=repeat + 1, casePosition=case_position,
                           variantPosition=variant_position, instrumented=False, speedComparison=True,
                           serverBinarySha256=hashes[variant], clientBinarySha256=hashes['client'])
                publish(results, row, 'results.json')
    for repeat in range(3):
        cell = f'diagnostic-{repeat + 1}-http3-upload-4'
        log = OUTPUT / f'{cell}-server.log'
        own = server_environment | {'MIMALLOC_SHOW_STATS': '1'}
        with cell_cgroup(cgroup_enabled, cell) as group:
            with server(binaries['diagnostic'], own, ports['http1'], log, group) as backend:
                row = transfer(backend, binaries['client'], environment, ports, cell, 'http3', 'upload', 4, 10, group)
            row['serverCgroupAfterShutdownBytes'] = cgroup_memory(group)
        row.update(variant='diagnostic', repeat=repeat + 1, instrumented=True, speedComparison=False,
                   serverBinarySha256=hashes['diagnostic'], clientBinarySha256=hashes['client'],
                   budgetReportsAtShutdown=budget_reports(log), allocatorShutdownSummaryLog=log.name)
        publish(diagnostic, row, 'diagnostic.json')


def main() -> None:
    OUTPUT.mkdir(parents=True, exist_ok=True)
    workspace = tomllib.loads((ROOT / 'rust/Cargo.toml').read_text())
    profile = workspace['profile']['release']
    assert (profile['opt-level'], profile['lto'], profile['codegen-units']) == (3, 'fat', 1)
    assert workspace['workspace']['dependencies']['rustfs-mimalloc'] == '=0.5.6'
    candidate = os.environ.get('GM_QUIC_RECEIVE_CANDIDATE_REV', workspace['patch']['crates-io']['noq']['rev'])
    assert re.fullmatch(r'[0-9a-f]{40}', candidate) and candidate != BASELINE
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('GM_', 'CARGO_PROFILE_', 'MIMALLOC_', 'MALLOC_'))
                   and not key.endswith('RUSTFLAGS')}
    channel = tomllib.loads((ROOT / 'rust/rust-toolchain.toml').read_text())['toolchain']['channel']
    environment.update(LC_ALL='C', CARGO_INCREMENTAL='0', CARGO_TARGET_DIR=str(BUILD),
                       CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER='x86_64-linux-gnu-gcc',
                       CC_x86_64_unknown_linux_musl='musl-gcc', RUSTUP_TOOLCHAIN=channel,
                       GM_ENGINE_VERSION=VERSION)
    assert not any(environment.get(key) for key in ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER'))
    fixture = Fixture('quic-receive-study-')
    cgroup_enabled = False
    try:
        run(['rustup', 'target', 'add', MUSL], environment, OUTPUT / 'target-setup.log')
        run(['cargo', 'fetch', '--locked'], environment, OUTPUT / 'cargo-fetch.log')
        for name, command in (('cpu', ['lscpu']), ('rustc', ['rustc', f'+{channel}', '-vV']),
                              ('musl-cc', ['musl-gcc', '-v'])):
            run(command, environment, OUTPUT / f'{name}.log')
        binaries = build_variants(environment, candidate)
        hashes = {label: hashlib.sha256(binary.read_bytes()).hexdigest() for label, binary in binaries.items()}
        cgroup_enabled, cgroup_reason = setup_cgroup(environment)
        _, cert, key = fixture.identity()
        ports = {protocol: unused_port() for protocol in ('http1', 'http2')}
        ports['http3'] = unused_port(kind=socket.SOCK_DGRAM)
        environment.update(GM_AUTH_MODE='off', GM_H1_ADDR=f'127.0.0.1:{ports["http1"]}',
                           GM_H2_ADDR=f'127.0.0.1:{ports["http2"]}', GM_H3_ADDR=f'127.0.0.1:{ports["http3"]}',
                           GM_TLS_CERT=str(cert), GM_TLS_KEY=str(key))
        study = {
            'mode': 'quic-receive-paired', 'baselineRevision': BASELINE, 'candidateRevision': candidate,
            'profile': profile, 'builds': 4, 'binarySha256': hashes, 'rustChannel': channel,
            'rustc': (OUTPUT / 'rustc.log').read_text(), 'platform': list(os.uname()),
            'logicalCpus': os.cpu_count(), 'lscpu': (OUTPUT / 'cpu.log').read_text(),
            'nativeAllocator': 'rustfs-mimalloc 0.5.6 / Microsoft mimalloc 3.5.3',
            'cargoConfig': (ROOT / 'rust/.cargo/config.toml').read_text(),
            'compileEnvironment': {key: value for key, value in environment.items()
                                   if key.startswith(('CARGO_', 'CC_', 'RUSTUP_'))},
            'developmentBuild': {'engineVersion': VERSION, 'dependencyNotices': 'absent',
                                 'distribution': 'diagnostic-only; binaries and assets are not uploaded'},
            'repeats': REPEATS, 'families': CASES, 'expectedCleanTransfers': 64, 'expectedDiagnosticTransfers': 3,
            'order': 'baseline/candidate then candidate/baseline alternating; rotate cases each repetition',
            'counterpart': 'fixed GNU Rust client built with baseline Noq; unchanged across all server variants',
            'serverAllocatorEnvironment': {'MIMALLOC_ALLOW_THP': '0'},
            'diagnosticOnlyEnvironment': {'MIMALLOC_SHOW_STATS': '1'},
            'warmupMs': 500, 'measureMs': 10000,
            'transparentHugePages': {name: Path('/sys/kernel/mm/transparent_hugepage', name).read_text().strip()
                                     for name in ('enabled', 'defrag', 'hpage_pmd_size')},
            'serverCgroup': {'enabled': cgroup_enabled, 'reason': cgroup_reason},
            'cpuWindow': 'server deltas and client GNU time bracket the entire client invocation, including '
                         'preparation, warmup and drain; receiver bytes/duration refer only to the measurement stage',
            'memory': 'RSS/PSS/Anonymous/AnonHugePages sampled every 100 ms plus endpoints. Cgroup entered before '
                      'server exec, client outside; current/peak and sock/kernel/anon/file are separate charges. '
                      'Cgroup peak covers startup through shutdown; sampled process peaks cover the client invocation. '
                      'Shared file page ownership and kernel/socket charges differ from RSS; peaks are not additive.',
            'diagnostic': 'Only diagnostic candidate has relaxed atomic per-budget high-water instrumentation. '
                          'Packet queue and reassembly peaks belong to independent budgets, not simultaneous total RSS. '
                          'used_at_drop_bytes is zero after the final Arc owner, not transfer-end live bytes. '
                          'floor_bytes is a separate reservation; allocator summary is shutdown state if emitted. '
                          'Instrumented throughput is context only, excluded from speed comparison.',
            'limitations': ['shared hosted x86_64 loopback; fixed peer and host contention may cap throughput',
                            'sampled memory can miss short-lived peaks; process RSS excludes socket memory; '
                            'the same periodic smaps/cgroup observer runs in both clean variants',
                            'no shaped-link, ARM, idle-retention, browser, or production distribution evidence'],
        }
        (OUTPUT / 'study.json').write_text(json.dumps(study, indent=2) + '\n')
        measure(binaries, environment, ports, hashes, cgroup_enabled)
    finally:
        try:
            if cgroup_enabled:
                run(['sudo', '-n', 'rmdir', str(CGROUP)], environment, OUTPUT / 'cgroup-cleanup.log', timeout=10)
        finally:
            shutil.rmtree(fixture.directory, ignore_errors=True)
            shutil.rmtree(BUILD, ignore_errors=True)


if __name__ == '__main__':
    main()
