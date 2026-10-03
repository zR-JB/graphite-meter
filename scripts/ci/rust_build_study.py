"""Temporary hosted comparison of legal build topology with identical release inputs."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import tomllib


def main() -> None:
    root = Path(__file__).resolve().parents[2]
    output = root / 'rust/target/build-study-results'
    output.mkdir(parents=True, exist_ok=True)
    collector = root / 'scripts/legal/rust.py'
    source = root / 'rust/server/src/config/load.rs'
    provenance = root / 'legal/rust-provenance.json'
    originals = {path: path.read_bytes() for path in (collector, source, provenance)}
    baseline = (root / 'rust/target/build-study-baseline.py').read_bytes()
    invocation = b'    result = subprocess.run(command, cwd=repo / \'rust\', env=environment,'
    assert baseline.count(invocation) == 1
    baseline = baseline.replace(invocation, b"    command.insert(3, '--timings')\n" + invocation)
    profile = tomllib.loads((root / 'rust/Cargo.toml').read_text())['profile']['release']
    assert (profile['opt-level'], profile['lto'], profile['codegen-units']) == (3, 'fat', 1)
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('CARGO_PROFILE_', 'GM_RUST_'))}
    environment.update(CARGO_INCREMENTAL='0', GM_RUST_BUILD_TIMINGS='1')
    assert not any(environment.get(name) for name in
                   ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER'))
    # Downloads are shared setup, outside both cold compilation measurements.
    subprocess.run(['rustup', 'target', 'add', 'x86_64-unknown-linux-musl'], cwd=root / 'rust', check=True)
    subprocess.run(['cargo', 'fetch', '--locked'],
                   cwd=root / 'rust', env=environment, check=True)
    results = []
    candidate_mtimes = {}
    try:
        for label, implementation in [('candidate', originals[collector]), ('baseline', baseline)]:
            collector.write_bytes(implementation)
            for bytecode in collector.parent.glob('__pycache__/rust.*.pyc'):
                bytecode.unlink()
            source.write_bytes(originals[source])
            provenance.write_bytes(originals[provenance])
            target = root / 'rust/target' / f'build-study-{label}'
            assert not target.exists(), f'cold build directory already exists: {target}'
            environment['CARGO_TARGET_DIR'] = str(target)
            environment['GM_RUST_BUILD_TRACE_DIR'] = str(output / label)
            notices = target / 'notices'
            command = ['python3', '-B', '-m', 'scripts.legal.rust', '--package', 'graphite-meter-server',
                       '--target', 'x86_64-unknown-linux-musl', '--profile', 'release', '--local',
                       '--version', '0.0.0-study', '--out', str(notices),
                       '--reviews', 'legal/rust-reviewed-components.json',
                       '--supplement', 'legal/rust-platform-debian-bookworm.json']
            for phase in ('cold', 'unchanged', 'source-edit', 'legal-input-edit'):
                if phase == 'source-edit':
                    before = b'clear HTTP/1.1 listen `address`'
                    assert originals[source].count(before) == 1
                    source.write_bytes(originals[source].replace(before, b'clear HTTP/1.1 bind `address`'))
                elif phase == 'legal-input-edit':
                    provenance.write_bytes(originals[provenance] + b'\n')
                log = output / f'{label}-{phase}.log'
                started = time.monotonic()
                with log.open('w') as stream:
                    completed = subprocess.run(command, cwd=root, env=environment, stdout=stream,
                                               stderr=subprocess.STDOUT, timeout=1200)
                elapsed = time.monotonic() - started
                result = {'pipeline': label, 'phase': phase, 'seconds': round(elapsed, 3),
                          'exit_code': completed.returncode, 'profile': profile}
                results.append(result)
                (output / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
                print(json.dumps(result), flush=True)
                print('\n'.join(log.read_text().splitlines()[-12:]), flush=True)
                completed.check_returncode()
                shutil.copyfile(notices / 'inventory.json', output / f'{label}-{phase}-inventory.json')
                shutil.copyfile(notices / 'LEGAL.txt', output / f'{label}-{phase}-LEGAL.txt')
                timings = target / 'cargo-timings'
                if timings.exists():
                    shutil.copytree(timings, output / f'{label}-{phase}-cargo-timings')
                    shutil.rmtree(timings)
            if label == 'baseline':
                shutil.rmtree(target)
            else:
                candidate_mtimes = {path: path.stat().st_mtime_ns for path in (source, provenance)}
        inspections = list((output / 'candidate').glob('capture-inspection-*.json'))
        assert len(inspections) == 2, 'candidate must inspect cold and invalidated legal inputs'
        invocation = json.loads((output / 'candidate/inspection-rustc.json').read_text())
        assert [argument for argument in invocation if argument.startswith('--emit=')] == ['--emit=dep-info,metadata']
        for phase in ('cold', 'unchanged', 'source-edit', 'legal-input-edit'):
            for suffix in ('inventory.json', 'LEGAL.txt'):
                assert (output / f'baseline-{phase}-{suffix}').read_bytes() == (
                    output / f'candidate-{phase}-{suffix}').read_bytes(), (phase, suffix)
        # Separate diagnostic after the comparison: keep the same optimized library and notices.
        collector.write_bytes(originals[collector])
        for path, modified in candidate_mtimes.items():
            os.utime(path, ns=(path.stat().st_atime_ns, modified))
        target = root / 'rust/target/build-study-candidate'
        environment.update(CARGO_TARGET_DIR=str(target), GM_RUST_LEGAL_DIR=str(target / 'notices'))
        cargo_home = environment.get('CARGO_HOME') or str(Path.home() / '.cargo')
        environment['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join(
            f'--remap-path-prefix={path}={name}' for path, name in ((root, '/src'), (cargo_home, '/cargo')))
        linker = environment.get('CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER', 'cc')
        shim = output / 'capture-linker.py'
        # rustc discards successful linker output; record it while preserving the real driver's result.
        shim.write_text(
            '#!/usr/bin/env python3\nimport json, subprocess, sys, time\n'
            f'command = [{linker!r}, *sys.argv[1:]]\nstarted = time.monotonic()\n'
            'result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)\n'
            f'with open({str(output / "native-link-stats.log")!r}, "ab") as stream:\n'
            '    stream.write(result.stdout + result.stderr)\n'
            f'with open({str(output / "native-link.json")!r}, "w") as stream:\n'
            '    json.dump({"command": command, "seconds": time.monotonic() - started, '
            '"exit_code": result.returncode}, stream)\n'
            'sys.stdout.buffer.write(result.stdout)\nsys.stderr.buffer.write(result.stderr)\n'
            'sys.exit(result.returncode)\n')
        shim.chmod(0o755)
        channel = tomllib.loads((root / 'rust/rust-toolchain.toml').read_text())['toolchain']['channel']
        command = ['cargo', f'+{channel}', 'rustc', '--locked', '--package', 'graphite-meter-server',
                   '--bin', 'graphite-meter-server', '--target', 'x86_64-unknown-linux-musl',
                   '--profile', 'release', '--timings', '--', f'-Clinker={shim}', '-Clink-arg=-Wl,--stats']
        started = time.monotonic()
        with (output / 'candidate-native-link-diagnostic.log').open('w') as stream:
            completed = subprocess.run(command, cwd=root / 'rust', env=environment, stdout=stream,
                                       stderr=subprocess.STDOUT, timeout=1200)
        result = {'pipeline': 'candidate', 'phase': 'native-link-diagnostic',
                  'seconds': round(time.monotonic() - started, 3), 'exit_code': completed.returncode,
                  'profile': profile}
        results.append(result)
        (output / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
        print(json.dumps(result), flush=True)
        completed.check_returncode()
        print((output / 'native-link-stats.log').read_text(), flush=True)
        shutil.copytree(target / 'cargo-timings', output / 'candidate-native-link-diagnostic-cargo-timings')
    finally:
        for path, content in originals.items():
            path.write_bytes(content)
        # Reports cross from the root container to the unprivileged artifact uploader.
        for path in output.rglob('*'):
            path.chmod(0o755 if path.is_dir() else 0o644)
        output.chmod(0o755)
        for label in ('candidate', 'baseline'):
            shutil.rmtree(root / 'rust/target' / f'build-study-{label}', ignore_errors=True)


if __name__ == '__main__':
    main()
