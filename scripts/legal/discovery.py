"""Read dependency closures from their owning toolchains without compiling helpers."""
from __future__ import annotations

import json
import os
import re
import subprocess
from pathlib import Path
from typing import cast

from .model import Component, Json, LegalError, Provenance, Review, array, obj, read_json, string, text
from .review import component_from_files, component_legal_files, find_review, read_legal_files, sort_components

GO_RELEASE = re.compile(r'^(go[0-9]+\.[0-9]+(?:\.[0-9]+)?)(?:[- \t].*)?$')


def run_go(repo: Path, *args: str, target: tuple[str, str] | None = None) -> str:
    env = os.environ.copy()
    if target:
        env.update(CGO_ENABLED='0', GOOS=target[0], GOARCH=target[1])
    result = subprocess.run(['go', *args], cwd=repo / 'go', env=env, capture_output=True, text=True)
    if result.returncode:
        raise LegalError(f'go {" ".join(args)}: {result.stderr.strip()}')
    return result.stdout


def go_packages(repo: Path, command: str, goos: str, goarch: str) -> list[dict[str, Json]]:
    output = run_go(repo, 'list', '-deps', '-json', command, target=(goos, goarch))
    decoder = json.JSONDecoder()
    packages: list[dict[str, Json]] = []
    offset = 0
    while offset < len(output):
        if output[offset].isspace():
            offset += 1
            continue
        value, offset = decoder.raw_decode(output, offset)
        packages.append(obj(cast(Json, value)))
    return packages


def go_discovery_targets(repo: Path) -> list[tuple[str, str, str]]:
    targets: list[tuple[str, str, str]] = []
    seen: set[str] = set()
    for line in (repo / 'scripts/tui-targets.txt').read_text().splitlines():
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        parts = line.split('/')
        if len(parts) != 2 or not all(parts) or line in seen:
            raise LegalError(f'invalid or duplicate TUI target {line!r}')
        seen.add(line)
        targets.append(('tui', parts[0], parts[1]))
    if not targets:
        raise LegalError('scripts/tui-targets.txt contains no targets')
    server = [('server', goos, goarch) for _, goos, goarch in targets
              if goos == 'linux' and goarch in ('amd64', 'arm64')]
    if len(server) != 2:
        raise LegalError('scripts/tui-targets.txt must include linux/amd64 and linux/arm64')
    return server + targets


def has_go_replacement_provenance(entries: list[Provenance], name: str, version: str, scope: str) -> bool:
    return any(entry.ecosystem == 'go' and entry.name == name and bool(entry.version)
               and (not version or entry.version == version)
               and (scope in entry.artifactScopes or scope == 'server' and 'server/browser' in entry.artifactScopes)
               for entry in entries)


def legal_go_version(raw: str) -> str:
    match = GO_RELEASE.fullmatch(raw)
    return match[1] if match else raw


def repository_go_toolchain_version(repo: Path) -> str:
    language = ''
    for line in (repo / 'go/go.mod').read_text().splitlines():
        fields = line.split()
        if not fields:
            continue
        if fields[0] == 'go':
            if len(fields) != 2 or not re.fullmatch(r'\d+\.\d+\.\d+', fields[1]):
                raise LegalError(f'go/go.mod must pin an exact Go release, got {" ".join(fields[1:])!r}')
            language = 'go' + fields[1]
        elif fields[0] == 'toolchain':
            if len(fields) != 2:
                raise LegalError('go/go.mod has malformed toolchain directive')
            match = GO_RELEASE.fullmatch(fields[1])
            if not match or match[1] != fields[1]:
                raise LegalError(f'go/go.mod toolchain must pin an exact Go release, got {fields[1]!r}')
            return fields[1]
    if not language:
        raise LegalError('go/go.mod must declare an exact Go release for legal review')
    return language


def go_toolchain_component(repo: Path) -> Component:
    expected = repository_go_toolchain_version(repo)
    actual = legal_go_version(run_go(repo, 'env', 'GOVERSION').strip())
    if actual != expected:
        raise LegalError(f'go toolchain mismatch during legal review: go/go.mod pins {expected} '
                         f'but go env reports {actual}; run `just doctor`')
    files = read_legal_files(repo / 'legal/toolchains/go', recursive=False)
    return component_from_files('go-toolchain', 'Go standard library', expected, 'https://go.dev/', files)


def source_for(name: str, upstream: str = '') -> str:
    if upstream:
        return upstream
    parts = name.split('/')
    if len(parts) >= 3:
        if parts[0] in ('github.com', 'gitlab.com'):
            return 'https://' + '/'.join(parts[:3])
        if parts[:2] == ['golang.org', 'x'] and len(parts) == 3:
            return 'https://go.googlesource.com/' + parts[2]
    return ''


def discover_go(repo: Path, reviews: list[Review], provenance: list[Provenance]) -> tuple[list[Component], list[Component]]:
    modules: dict[tuple[str, str], tuple[str, str]] = {}
    standard = False
    for scope, goos, goarch in go_discovery_targets(repo):
        command = './cmd/graphite-meter-client' if scope == 'tui' else './cmd/graphite-meter'
        for package in go_packages(repo, command, goos, goarch):
            standard |= package.get('Standard') is True
            raw = package.get('Module')
            if raw is None:
                continue
            module = obj(raw)
            name = text(module, 'Path')
            if name.startswith('github.com/zR-JB/graphite-meter/go'):
                continue
            version, directory = text(module, 'Version'), text(module, 'Dir')
            if replacement := module.get('Replace'):
                module = obj(replacement)
                name, version, directory = (text(module, key) for key in ('Path', 'Version', 'Dir'))
                if not directory:
                    raise LegalError(f'local or custom Go replacement has no resolved directory: {name}')
                if not has_go_replacement_provenance(provenance, name, version, scope):
                    raise LegalError(f'local or custom Go replacement requires provenance: {name}')
            modules[scope, name] = version, directory
    scopes: dict[str, list[Component]] = {'server': [], 'tui': []}
    for (scope, name), (version, directory) in sorted(modules.items()):
        try:
            files = component_legal_files(Path(directory), 'go', name, reviews)
        except (LegalError, OSError):
            # Audit/template modes must expose unresolved modules; normal modes
            # reject UNKNOWN in the review policy before generating anything.
            component = Component(name, version, 'go', source_for(name), 'UNKNOWN', 'UNKNOWN')
        else:
            component = component_from_files('go', name, version, source_for(name), files)
            if review := find_review('go', name, reviews):
                component.declaredLicenseExpression = review.declaredLicenseExpression
                component.selectedLicenseExpression = review.selectedLicenseExpression
        component.source_path = Path(directory) if directory else None
        scopes[scope].append(component)
    if standard:
        toolchain = go_toolchain_component(repo)
        scopes['server'].append(toolchain)
        scopes['tui'].append(toolchain)
    return sort_components(scopes['server']), sort_components(scopes['tui'])


def package_root(module_id: str) -> Path:
    path = Path(module_id.removeprefix('\\x00')).absolute()
    for directory in (path, *path.parents):
        if (directory / 'package.json').is_file():
            return directory
    raise LegalError(f'bundled browser module has no package.json root: {module_id}')


def package_license(value: Json, licenses: Json, fallback: str) -> str:
    if isinstance(value, str) and value:
        return value
    for item in array(licenses):
        if license_type := text(obj(item), 'type'):
            return license_type
    return fallback


def repository_url(value: Json, name: str) -> str:
    url = value if isinstance(value, str) else text(value, 'url') if isinstance(value, dict) else ''
    return url.removeprefix('git+').removesuffix('.git') if url else source_for(name)


def npm_component(root: Path, reviews: list[Review]) -> Component:
    metadata = obj(read_json(root / 'package.json'))
    name = text(metadata, 'name')
    files = component_legal_files(root, 'npm', name, reviews)
    component = component_from_files('npm', name, text(metadata, 'version'), repository_url(metadata.get('repository'), name), files)
    component.source_path = root
    component.declaredLicenseExpression = package_license(metadata.get('license'), metadata.get('licenses', []), component.declaredLicenseExpression)
    component.selectedLicenseExpression = component.declaredLicenseExpression
    return component


def discover_browser(scan: Path, reviews: list[Review]) -> list[Component]:
    ids = [string(item) for item in array(read_json(scan))]
    roots = {package_root(module_id) for module_id in ids if '/node_modules/' in module_id.replace(os.sep, '/')}
    return sort_components([npm_component(root, reviews) for root in sorted(roots)])
