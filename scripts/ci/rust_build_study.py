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
    try:
        for label, implementation in [('baseline', baseline), ('candidate', originals[collector])]:
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
            # Keep small reports, not duplicate multi-gigabyte release target trees.
            shutil.rmtree(target)
        inspections = list((output / 'candidate').glob('capture-inspection-*.json'))
        assert len(inspections) == 2, 'candidate must inspect cold and invalidated legal inputs'
        invocation = json.loads((output / 'candidate/inspection-rustc.json').read_text())
        assert [argument for argument in invocation if argument.startswith('--emit=')] == ['--emit=dep-info,metadata']
        for phase in ('cold', 'unchanged', 'source-edit', 'legal-input-edit'):
            for suffix in ('inventory.json', 'LEGAL.txt'):
                assert (output / f'baseline-{phase}-{suffix}').read_bytes() == (
                    output / f'candidate-{phase}-{suffix}').read_bytes(), (phase, suffix)
    finally:
        for path, content in originals.items():
            path.write_bytes(content)


if __name__ == '__main__':
    main()
