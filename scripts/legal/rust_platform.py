"""Reviewed native platform inputs not visible in Cargo's package graph.

A record lists the linked inputs beyond the target's sysroot rlibs (`nativeInputs`) and the notice
texts beyond the Rust standard library's (`notices`); the rlibs and the standard-library texts are
found in the sysroot by rule. `noticesSha256` hashes only the canonical listing of notice texts;
the pinned release builder and toolchain establish binary provenance separately.
"""
from __future__ import annotations

import os
import re
import subprocess
import urllib.request
from pathlib import Path

from ..ci.github_api import confined_path
from ..ci.toolchains import rust_channel
from .model import Json, LegalError, array, obj, read_json, sha256, string, strings, text

SYSROOT = '$RUST_SYSROOT/'
IMPORTS = {
    'elf': (['readelf', '--dynamic'], r'\(NEEDED\).*\[([^]]+)\]'),
    'pe': (['objdump', '-p'], r'DLL Name: (\S+)'),
    # Every install name, absolute or relative to @rpath, @executable_path or @loader_path.
    'macho': (['otool', '-L'], r'(?m)^\s+(\S.*?) \(compatibility version'),
}


def link_map(output: Path, target: str, profile: str) -> Path:
    """The linker map of `target`, which stays inside `output` whatever the target names."""
    return confined_path(output / f'{target}-{profile}.map', output)


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


def record(path: Path, target: str) -> dict[str, Json] | None:
    entries = [obj(item) for item in array(read_json(path))] if path.exists() else []
    matching = [entry for entry in entries if entry.get('target') == target]
    if len(matching) > 1:
        raise LegalError(f'duplicate Rust platform record for {target}')
    return matching[0] if matching else None


def source(path: str, sysroot: Path) -> Path:
    return sysroot / path.removeprefix(SYSROOT) if path.startswith(SYSROOT) else Path(path)


def rlibs(sysroot: Path, target: str) -> set[str]:
    return {SYSROOT + path.relative_to(sysroot).as_posix()
            for path in (sysroot / 'lib/rustlib' / target / 'lib').glob('*.rlib')}


def own_notices(entry: dict[str, Json] | None) -> dict[str, str]:
    return {path: string(name) for path, name in obj((entry or {}).get('notices', {})).items()}


def fetch_notices(repo: Path, entry: dict[str, Json]) -> None:
    """Materialize only this platform's notices, checking downloaded and cached bytes."""
    resources = obj(read_json(repo / 'legal/rust-notice-sources.json'))
    external = own_notices(entry).keys() & resources.keys()
    if external and resources.get('rustVersion') != (version := rust_channel(repo)):
        raise LegalError(f'external runtime notice sources require review for Rust {version}')
    for relative in external:
        resource = obj(resources[relative])
        path = confined_path(repo / relative, repo)
        if path.exists():
            data = path.read_bytes()
        else:
            url = text(resource, 'url')
            if not url.startswith('https://'):
                raise LegalError(f'notice source must use HTTPS: {relative}')
            with urllib.request.urlopen(url, timeout=30) as response:
                data = response.read(1_000_001)
        if len(data) > 1_000_000 or sha256(data) != text(resource, 'sha256'):
            raise LegalError(f'runtime notice bytes differ from reviewed source: {relative}')
        if not path.exists():
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)


def notice_names(entry: dict[str, Json] | None, sysroot: Path) -> dict[str, str]:
    """Every notice text in notice order: the Rust standard library's, named by rule, then the record's."""
    documents = sysroot / 'share/doc/rust'
    names = {SYSROOT + path.relative_to(sysroot).as_posix(): 'rust-standard-library/' + path.name
             for path in [documents / 'COPYRIGHT-library.html', *sorted((documents / 'licenses').iterdir())]}
    listed = own_notices(entry)
    if names.keys() & listed.keys() or not all(listed.values()):
        raise LegalError('record notices must name texts beyond the Rust standard library, each with a name')
    return names | listed


def fingerprint(paths: set[str], sysroot: Path) -> tuple[str, dict[str, bytes]]:
    """The notice listing, `path<TAB>sha256<LF>` sorted by path, and the exact texts to embed."""
    lines: list[str] = []
    kept: dict[str, bytes] = {}
    for path in sorted(paths):
        data = source(path, sysroot).read_bytes()
        lines.append(f'{path}\t{sha256(data)}\n')
        kept[path] = data
    return ''.join(lines), kept


def notice(entry: dict[str, Json] | None, *, target: str, sysroot: Path,
           inputs: set[str], libraries: set[str]) -> str:
    if (entry is None or entry.get('target') != target or entry.get('reviewDecision') != 'approved'
            or not entry.get('reviewNotes')):
        raise LegalError(f'Rust platform review for {target} is absent or unresolved')
    reviewed = rlibs(sysroot, target) | set(strings(entry, 'nativeInputs'))
    if unreviewed := sorted(inputs - reviewed):
        raise LegalError(f'linked native inputs lack review: {unreviewed}')
    if unreviewed := sorted(libraries - set(strings(entry, 'systemLibraries'))):
        raise LegalError(f'imported system libraries lack review: {unreviewed}')
    names = notice_names(entry, sysroot)
    listing, texts = fingerprint(set(names), sysroot)
    if (digest := sha256(listing.encode())) != (reviewed_digest := text(entry, 'noticesSha256')):
        raise LegalError(f'reviewed Rust platform notices changed: noticesSha256 is {digest}, '
                         f'reviewed {reviewed_digest or "none"}')
    return text(entry, 'description') + '\n' + ''.join(
        f'\n--- {name} ---\n\n' + texts[path].decode() for path, name in names.items())


def candidate(entry: dict[str, Json] | None, *, target: str, sysroot: Path,
              inputs: set[str], libraries: set[str]) -> tuple[dict[str, object], str]:
    """This build's unreviewed native/import record and canonical notice listing.

    It keeps the reviewed native inputs this build did not link while they exist, such as import
    libraries only unoptimized builds take, so re-approving it retains their coverage.
    """
    rlib = rlibs(sysroot, target)
    native = inputs - rlib | {path for path in strings(entry or {}, 'nativeInputs') if source(path, sysroot).exists()}
    listing, _ = fingerprint(set(notice_names(entry, sysroot)), sysroot)
    return {
        'target': target,
        'systemLibraries': sorted(libraries), 'reviewDecision': 'pending', 'reviewNotes': '',
        'description': text(entry or {}, 'description'), 'nativeInputs': sorted(native),
        'notices': own_notices(entry), 'noticesSha256': sha256(listing.encode()),
    }, listing
