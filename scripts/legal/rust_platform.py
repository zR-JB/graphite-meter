"""Reviewed native platform inputs not visible in Cargo's package graph."""
from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path

from .model import Json, LegalError, array, obj, read_json, sha256, strings, text

SYSROOT = '$RUST_SYSROOT/'
IMPORTS = {
    'elf': (['readelf', '--dynamic'], r'\(NEEDED\).*\[([^]]+)\]'),
    'pe': (['objdump', '-p'], r'DLL Name: (\S+)'),
    'macho': (['otool', '-L'], r'(?m)^\s+(/\S+) \(compatibility version'),
}


def link_map_argument(target: str, path: Path) -> str:
    return f'-Clink-arg=-Wl,-map,{path}' if '-apple-' in target else f'-Clink-arg=-Wl,-Map={path}'


def linked(link_map: Path, sysroot: Path, cargo: set[Path]) -> set[str]:
    """Objects and archives with a linked archive(member) in a GNU ld, LLD or ld64 map, outside Cargo."""
    listing = link_map.read_text(errors='replace')
    paths = re.findall(r'(/[^\s():]+\.o)\b', listing) + re.findall(r'(/[^\s():]+)\([^\s()]+\)', listing)
    found = set()
    for path in {Path(os.path.normpath(path)) for path in paths}:
        if not any(path.is_relative_to(directory) for directory in cargo):
            found.add(SYSROOT + path.relative_to(sysroot).as_posix() if path.is_relative_to(sysroot) else str(path))
    return found


def imports(executable: Path, target: str) -> set[str]:
    kind = 'pe' if '-windows-' in target else 'macho' if '-apple-' in target else 'elf'
    command, pattern = IMPORTS[kind]
    names = re.findall(pattern, subprocess.check_output([*command, str(executable)], text=True))
    return {name.lower() for name in names} if kind == 'pe' else set(names)


def linker_version(target: str) -> str:
    linker = os.environ.get(f'CARGO_TARGET_{target.upper().replace("-", "_")}_LINKER', 'cc')
    return subprocess.check_output([linker, '--version'], text=True).split('\n', 1)[0]


def record(path: Path, target: str) -> dict[str, Json] | None:
    entries = [obj(item) for item in array(read_json(path))] if path.exists() else []
    matching = [entry for entry in entries if entry.get('target') == target]
    if len(matching) > 1:
        raise LegalError(f'duplicate Rust platform record for {target}')
    return matching[0] if matching else None


def source(path: str, sysroot: Path) -> Path:
    return sysroot / path.removeprefix(SYSROOT) if path.startswith(SYSROOT) else Path(path)


def notice(entry: dict[str, Json] | None, *, target: str, compiler: str, sysroot: Path,
           inputs: set[str], libraries: set[str]) -> str:
    if (entry is None or entry.get('rustc') != compiler or entry.get('reviewDecision') != 'approved'
            or not entry.get('reviewNotes')):
        raise LegalError(f'Rust platform review for {target} is absent or stale for this compiler')
    if text(entry, 'nativeCompiler') != linker_version(target):
        raise LegalError('native linker toolchain differs from reviewed platform')
    reviewed = {text(item, 'path'): item for item in map(obj, array(entry.get('inputs', [])))}
    if unreviewed := sorted(inputs - reviewed.keys()):
        raise LegalError(f'linked native inputs lack review: {unreviewed}')
    if unreviewed := sorted(libraries - set(strings(entry, 'systemLibraries'))):
        raise LegalError(f'imported system libraries lack review: {unreviewed}')
    output = [text(entry, 'description') + '\n']
    for path, item in reviewed.items():
        data = source(path, sysroot).read_bytes()
        if sha256(data) != text(item, 'sha256'):
            raise LegalError(f'reviewed Rust platform input changed: {path}')
        if name := text(item, 'noticeName'):
            output.append(f'\n--- {name} ---\n\n' + data.decode())
    if len(output) == 1:
        raise LegalError('reviewed Rust platform has no notice files')
    return ''.join(output)


def candidate(entry: dict[str, Json] | None, *, target: str, compiler: str, sysroot: Path,
              inputs: set[str], libraries: set[str]) -> dict[str, object]:
    documents = sysroot / 'share/doc/rust'
    listed: dict[str, str] = {SYSROOT + path.relative_to(sysroot).as_posix(): 'rust-standard-library/' + path.name
                              for path in [documents / 'COPYRIGHT-library.html', *sorted((documents / 'licenses').iterdir())]}
    for item in map(obj, array((entry or {}).get('inputs', []))):
        if text(item, 'noticeName') and not text(item, 'path').startswith(SYSROOT):
            listed[text(item, 'path')] = text(item, 'noticeName')
    for path in sorted(inputs) + [SYSROOT + path.relative_to(sysroot).as_posix()
                                  for path in sorted((sysroot / 'lib/rustlib' / target / 'lib').glob('*.rlib'))]:
        listed.setdefault(path, '')
    return {
        'target': target, 'rustc': compiler, 'nativeCompiler': linker_version(target),
        'systemLibraries': sorted(libraries), 'reviewDecision': 'pending', 'reviewNotes': '',
        'description': text(entry or {}, 'description'),
        'inputs': [{'path': path, 'sha256': sha256(source(path, sysroot).read_bytes())}
                   | ({'noticeName': name} if name else {}) for path, name in listed.items()],
    }
