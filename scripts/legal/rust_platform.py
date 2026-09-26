"""Reviewed native platform inputs not visible in Cargo's package graph."""
from __future__ import annotations

import subprocess
from pathlib import Path

from .model import LegalError, array, obj, read_json, sha256, text


def notice(path: Path, *, target: str, compiler: str, channel: str) -> str:
    record = obj(read_json(path))
    if (record.get('rustc') != compiler or record.get('target') != target
            or record.get('reviewDecision') != 'approved' or not record.get('reviewNotes')):
        raise LegalError('Rust platform notice review is absent or stale for this compiler/target')
    native = subprocess.check_output(['cc', '--version'], text=True)
    if record.get('nativeCompiler') != native:
        raise LegalError('native linker toolchain differs from reviewed platform')
    sysroot = Path(subprocess.check_output(['rustc', f'+{channel}', '--print', 'sysroot'], text=True).strip())
    output = [text(record, 'description') + '\n']
    for value in array(record.get('inputs', [])):
        item = obj(value)
        source = text(item, 'path')
        actual = sysroot / source.removeprefix('$RUST_SYSROOT/') if source.startswith('$RUST_SYSROOT/') else Path(source)
        data = actual.read_bytes()
        if sha256(data) != text(item, 'sha256'):
            raise LegalError(f'reviewed Rust platform input changed: {source}')
        if name := text(item, 'noticeName'):
            output.append(f'\n--- {name} ---\n\n' + data.decode())
    if len(output) == 1:
        raise LegalError('reviewed Rust platform has no notice files')
    return ''.join(output)


def verify_dynamic_runtime(executable: Path) -> None:
    """This reviewed GNU target excludes static libc and additional native DSOs."""
    import re

    dynamic = subprocess.check_output(['readelf', '--dynamic', str(executable)], text=True)
    libraries = set(re.findall(r'\(NEEDED\).*\[([^]]+)\]', dynamic))
    supported = {'libgcc_s.so.1', 'libm.so.6', 'libc.so.6', 'ld-linux-x86-64.so.2'}
    if 'libc.so.6' not in libraries or not libraries <= supported:
        raise LegalError(f'GNU runtime linkage differs from reviewed dynamic-library scope: {sorted(libraries)}')
