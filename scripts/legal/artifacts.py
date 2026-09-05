"""Deterministic legal notices, review output, and third-party source archives."""
from __future__ import annotations

import gzip
import io
import os
import re
import tarfile
from pathlib import Path

from .model import Component, Json, LegalError, Project, Provenance, Review, marshal
from .review import component_key, find_review, validate_review

RELEASE_VERSION = re.compile(r'v?[0-9]+\.[0-9]+\.[0-9]+(?:-(?:alpha|beta|rc)\.[0-9]+)?')


def notices(components: list[Component]) -> str:
    output = ['THIRD-PARTY SOFTWARE NOTICES\n\nGraphite Meter includes third-party software described below.\n\n']
    for component in components:
        modified = 'yes' if component.modified else 'no'
        output.append(f'Component: {component.name}\nVersion: {component.version}\nSource: {component.source}\n'
                      f'License: {component.selectedLicenseExpression}\nModified by Graphite Meter: {modified}\n\n')
        output.extend(f'--- {item.name} ---\n\n{item.text}\n' for item in component.legalTexts + component.notices)
        output.append('============================================================\n\n')
    return ''.join(output)


def render(repo: Path, project: Project, version: str, scopes: dict[str, list[Component]]) -> dict[str, bytes]:
    source_url = project.repository
    if RELEASE_VERSION.fullmatch(version):
        version = version.removeprefix('v')
        source_url += '/tree/v' + version
    license_text = (repo / 'LICENSE').read_bytes()
    copyright_text = (f'{project.name}\nCopyright © {project.copyrightYears} {project.copyrightHolder}\n\n'
                      f'{project.name} is free software licensed under {project.licenseExpression}.\n'
                      'See LICENSE for the complete GNU Affero General Public License version 3 text.\n')
    files = {'COPYRIGHT': copyright_text.encode()}
    for scope, components in scopes.items():
        name = 'server' if scope == 'server/browser' else scope
        base = f'legal/generated/{name}'
        files[base + '/inventory.json'] = marshal({'schemaVersion': 1, 'scope': name, 'components': [item.json() for item in components]})
        files[base + '/THIRD_PARTY_NOTICES.txt'] = notices(components).encode()
        files[base + '/SOURCE.txt'] = (source_url + '\n').encode()
    about_components: list[dict[str, Json]] = []
    for component in scopes['server/browser']:
        value = component.json()
        del value['legalTexts'], value['notices']
        about_components.append(value)
    files['client/public/legal/about.json'] = marshal({
        'schemaVersion': 2, 'project': project.json(), 'sourceVersion': version, 'sourceURL': source_url,
        'licenseURL': 'legal/LICENSE.txt', 'noticesURL': 'legal/THIRD_PARTY_NOTICES.txt',
        'components': about_components,
    })
    files['client/public/legal/LICENSE.txt'] = license_text
    files['client/public/legal/THIRD_PARTY_NOTICES.txt'] = notices(scopes['server/browser']).encode()
    report = copyright_text.encode() + f'\nSource code: {source_url}\n\nLICENSE\n\n'.encode() + license_text
    if not license_text.endswith(b'\n'):
        report += b'\n'
    files['go/internal/legal/assets/TUI_LEGAL.txt'] = report + b'\n' + notices(scopes['tui']).encode()
    return files


def review_template(scopes: dict[str, list[Component]]) -> bytes:
    seen = {(item.ecosystem, item.name): item for items in scopes.values() for item in items}
    # The legacy template used maps rather than records, hence sorted keys.
    result: list[dict[str, Json]] = []
    for component in sorted(seen.values(), key=component_key):
        value: dict[str, Json] = {
            'ecosystem': component.ecosystem, 'name': component.name, 'reviewedVersion': component.version,
            'upstream': component.source, 'declaredLicenseExpression': component.declaredLicenseExpression,
            'selectedLicenseExpression': '', 'legalFiles': [item.json() for item in component.legalTexts + component.notices],
            'reviewDecision': '', 'reviewNotes': '',
        }
        result.append(dict(sorted(value.items())))
    return marshal(result)


def review_audit(scopes: dict[str, list[Component]], reviews: list[Review]) -> bytes:
    result: list[dict[str, Json]] = []
    for scope, components in scopes.items():
        for component in components:
            matched = find_review(component.ecosystem, component.name, reviews)
            value: dict[str, Json] = {'scope': scope, 'component': component.json()}
            state = 'review-required'
            if matched:
                value['review'] = matched.json()
                try:
                    validate_review(component, [matched])
                    state = 'fingerprint-matches'
                except LegalError:
                    pass
            value['reviewState'] = state
            result.append(value)
    return marshal(result)


def safe_name(value: str) -> str:
    return value.replace('/', '_').replace('\\', '_').replace('@', '_at_')


def manual_source_destination(entry: Provenance) -> str:
    if not entry.correspondingSource:
        return 'third_party/manual/' + safe_name(entry.name)
    clean = os.path.normpath(entry.correspondingSource)
    if ('\\' in entry.correspondingSource or Path(clean).is_absolute() or
            not clean.startswith('third_party/manual/')):
        raise LegalError(f'invalid corresponding source path for {entry.name}: {entry.correspondingSource!r}')
    return clean


def add_bytes(archive: tarfile.TarFile, name: str, data: bytes) -> None:
    header = tarfile.TarInfo(name)
    header.mode = 0o644
    header.size = len(data)
    header.mtime = 0
    archive.addfile(header, io.BytesIO(data))


def add_tree(archive: tarfile.TarFile, root: Path, destination: str) -> None:
    # Preserve the former walk's lexical depth-first order and dereference file
    # symlinks without traversing directory links.
    if root.is_symlink() or not root.is_dir():
        add_bytes(archive, destination, root.read_bytes())
        return
    for path in sorted(root.iterdir()):
        add_tree(archive, path, destination + '/' + path.name)


def third_party_source_bundle(repo: Path, project: Project, version: str,
                             scopes: dict[str, list[Component]], provenance: list[Provenance], output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    root = f'graphite-meter_{version}_third-party-source'
    with output.open('wb') as raw, gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode='w|', format=tarfile.PAX_FORMAT) as archive:
            seen: set[tuple[str, str]] = set()
            for ecosystem, components in (
                ('go', [item for items in scopes.values() for item in items]),
                ('npm', scopes['server/browser']),
            ):
                for component in components:
                    key = (component.name, component.version)
                    if component.ecosystem != ecosystem or key in seen:
                        continue
                    seen.add(key)
                    if component.source_path is None:
                        raise LegalError(f'source directory unavailable for {ecosystem} {component.name}@{component.version}')
                    add_tree(archive, component.source_path,
                             f'{root}/third_party/{ecosystem}/' + safe_name(component.name + '@' + component.version))
            for entry in provenance:
                destination = root + '/' + manual_source_destination(entry)
                for local in entry.localPaths + [item.name for item in entry.localLegalFiles]:
                    if Path(local).is_absolute():
                        continue
                    path = repo / local
                    try:
                        path.lstat()
                        add_tree(archive, path, destination + '/' + path.name)
                    except OSError as error:
                        raise LegalError(f'archive manual source {local} for {entry.name}: {error}') from error
            # The inventory envelope was a sorted map; component records keep their field order.
            inventory = {('server' if name == 'server/browser' else name): [item.json() for item in items]
                         for name, items in scopes.items()}
            add_bytes(archive, root + '/LEGAL_INVENTORY.json', marshal(dict(sorted(inventory.items()))))
            add_bytes(archive, root + '/PROVENANCE.json', marshal([entry.json() for entry in provenance]))
            readme = (f'Graphite Meter third-party source for {version}.\n\n'
                      "This archive contains source material for third-party components used by the Graphite Meter release and its generated legal inventories. It intentionally does not duplicate Graphite Meter's own repository source.\n\n"
                      "For a published GitHub release, use this archive together with GitHub's automatic Source code (tar.gz) or Source code (zip) archive for the matching release tag. Together they form the source offer for that release.\n\n"
                      f'Project source repository: {project.repository}\n')
            add_bytes(archive, root + '/README.txt', readme.encode())
