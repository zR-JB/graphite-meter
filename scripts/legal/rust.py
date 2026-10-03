"""Collect Rust notices from one real Cargo binary build, never the resolved graph alone.

Run `python3 -m scripts.legal.rust --help`. The inventory is a conservative
compilation-input superset: build scripts and procedural macros are retained.
It does not infer the licensing of the Rust sysroot or system libraries.
`--development` keeps every dependency review but no platform record, for a build on any
host; its notices say that they are unreviewed (legal/README.md).
"""
from __future__ import annotations

import argparse
import gzip
import json
import os
import re
import subprocess
import sys
import tarfile
import tempfile
import time
import zlib
from dataclasses import replace
from pathlib import Path

from ..ci.github_api import ControlPlaneError, local_path
from ..ci.toolchains import rust_channel
from .artifacts import about_components, add_bytes, add_tree, legal_header, notices, release_source
from .discovery import discover_browser
from .model import (Component, LegalError, Project, Provenance, Review, array, manual_files, manual_sources, marshal,
                    obj, read_json, sha256, strings, text)
from .review import add_provenance, component_key, component_legal_files, validate_review
from . import rust_platform as platform

PACKAGES = ('graphite-meter-client', 'graphite-meter-server')
# Opens the notices of a --development build; rust/legal_build.rs also leaves it in its executable.
DEVELOPMENT = 'UNREVIEWED DEVELOPMENT BUILD'
DEVELOPMENT_NOTICE = (f'{DEVELOPMENT}\n\nThis build\'s host generated these notices. They cover the dependencies it '
                      'may compile, but not its Rust standard library, C runtime or system libraries, which only the '
                      'reviewed platform records of the release builders cover. Do not distribute this build.\n\n')
DEVELOPMENT_PLATFORM = ('This development build\'s Rust standard library, C runtime and system libraries are not '
                        'reviewed, so their notices are not included.\n')


def write_changed(path: Path, data: bytes) -> bool:
    """Keep Cargo's input mtimes when the checked bytes did not change."""
    if path.is_file() and path.read_bytes() == data:
        return False
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return True


def stage_browser(source: Path, destination: Path) -> bool:
    """Synchronize assets without replacing the Rust-owned legal directory."""
    if not (source / 'index.html').is_file():
        raise LegalError('browser asset directory has no index.html')
    changed = False
    files = {}
    for path in source.rglob('*'):
        if path.is_symlink():
            raise LegalError('browser legal staging does not accept symbolic links')
        relative = path.relative_to(source)
        if relative.parts[0] != 'legal' and path.is_file():
            files[relative] = path
    # Remove stale paths first: an asset may change from a file to a directory or back.
    for path in sorted(destination.rglob('*'), reverse=True):
        relative = path.relative_to(destination)
        if relative.parts[0] == 'legal':
            continue
        if path.is_symlink():
            raise LegalError('browser legal staging does not accept symbolic links')
        if path.is_file() and relative not in files:
            path.unlink()
            changed = True
        elif path.is_dir() and not any(path.iterdir()):
            path.rmdir()
            changed = True
    for relative, path in files.items():
        changed |= write_changed(destination / relative, path.read_bytes())
    return changed


def cargo(repo: Path, *args: str) -> list[str]:
    return ['cargo', f'+{rust_channel(repo)}', *args]


def cargo_metadata(repo: Path, host: str, target: str | None) -> dict:
    # Keep host build/proc-macro packages as well as cross-target runtime packages.
    filters = ['--filter-platform', host]
    if target and target != host:
        filters += ['--filter-platform', target]
    return json.loads(subprocess.check_output(
        cargo(repo, 'metadata', '--locked', '--format-version=1', *filters), cwd=repo / 'rust', text=True))


def resolved_inputs(metadata: dict, trees: list[str]) -> dict[str, dict]:
    selected: dict[str, dict] = {}
    for tree in trees:
        for line in tree.splitlines():
            name, version, *_ = line.split()
            matches = [item['id'] for item in metadata['packages']
                       if (item['name'], item['version']) == (name, version.removeprefix('v'))]
            if len(matches) != 1:
                raise LegalError(f'Cargo dependency identity is missing or ambiguous: {line}')
            selected[matches[0]] = {'units': [], 'nativeLibraries': []}
    return selected


def prepare(repo: Path, package: str, host: str, target: str) -> tuple[dict, dict[str, dict]]:
    metadata = cargo_metadata(repo, host, target)
    # Cargo owns target selection. Including the host tree conservatively retains host build dependencies.
    trees = [subprocess.check_output(cargo(repo, 'tree', '--locked', '--package', package, '--target', platform,
                                        '--edges', 'normal,build', '--prefix', 'none', '--format', '{p}'),
                                     cwd=repo / 'rust', text=True) for platform in dict.fromkeys((target, host))]
    return metadata, resolved_inputs(metadata, trees)


def capture(repo: Path, package: str, target: str | None, profile: str, link_map: Path | None,
            legal_directory: Path | None = None, asset_directory: Path | None = None,
            *, link_target: str | None = None, metadata: dict | None = None) -> tuple[dict, list[dict]]:
    """Own the final binary build so test/workspace artifacts cannot contaminate the scan."""
    environment = dict(os.environ)
    environment.pop('GM_RUST_LEGAL_DIR', None)
    # A cross-build names neither the checkout nor Cargo's home; local builds keep plain Cargo's flags.
    if target:
        cargo_home = os.environ.get('CARGO_HOME') or str(Path.home() / '.cargo')
        environment['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join(
            f'--remap-path-prefix={path}={name}' for path, name in ((repo, '/src'), (cargo_home, '/cargo')))
    if asset_directory is not None:
        environment['GM_RUST_ASSET_DIR'] = str(asset_directory)
    if legal_directory is not None:
        environment['GM_RUST_LEGAL_DIR'] = str(legal_directory)
    command = cargo(repo, 'rustc', '--locked', '--package', package, '--bin', package,
                    *(['--target', target] if target else []), '--profile', profile, '--message-format=json',
                    *(['--timings'] if environment.get('GM_RUST_BUILD_TIMINGS') == '1' else []),
                    *(['--', platform.link_map_argument(link_target or target or '', link_map)] if link_map else []))
    started = time.perf_counter()
    result = subprocess.run(command, cwd=repo / 'rust', env=environment,
                            stdout=subprocess.PIPE, check=True, text=True)
    elapsed = time.perf_counter() - started
    metadata_started = time.perf_counter()
    messages = [json.loads(line) for line in result.stdout.splitlines() if line.startswith('{')]
    if metadata is None:
        toolchain = subprocess.check_output(['rustc', f'+{rust_channel(repo)}', '-vV'], text=True)
        host = next(line.removeprefix('host: ') for line in toolchain.splitlines() if line.startswith('host: '))
        metadata = cargo_metadata(repo, host, target)
    if environment.get('GM_RUST_BUILD_TIMINGS') == '1':
        print(f'Rust capture [linked]: cargo={elapsed:.3f}s', flush=True)
    if trace_name := environment.get('GM_RUST_BUILD_TRACE_DIR'):
        trace = local_path(Path(trace_name), repo)
        trace.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(mode='w', prefix='capture-linked-', suffix='.json', dir=trace,
                                         delete=False) as destination:
            json.dump({'command': command, 'cargoSeconds': elapsed,
                       'metadataSeconds': time.perf_counter() - metadata_started, 'messages': messages}, destination)
    return metadata, messages


def add_cargo_sources(archive: tarfile.TarFile, repo: Path, components: list[Component]) -> None:
    with tempfile.TemporaryDirectory(prefix='rust-sources-') as scratch:
        destination = Path(scratch) / 'vendor'
        subprocess.run(cargo(repo, 'vendor', '--locked', '--versioned-dirs', str(destination)),
                       cwd=repo / 'rust', stdout=subprocess.DEVNULL, check=True, timeout=600)
        for component in components:
            name = f'{component.name}-{component.version}'
            add_tree(archive, destination / name, f'third_party/cargo/{name}', destination / name)


def artifacts(messages: list[dict], package_id: str, binary: str) -> dict[str, dict]:
    if not messages or messages[-1] != {'reason': 'build-finished', 'success': True}:
        raise LegalError('Cargo did not report a successful completed build')
    collected: dict[str, dict] = {}
    executable = False
    for message in messages:
        reason = message.get('reason')
        if reason not in ('compiler-artifact', 'build-script-executed'):
            continue
        identity = message['package_id']
        entry = collected.setdefault(identity, {'units': [], 'nativeLibraries': []})
        if reason == 'build-script-executed':
            entry['nativeLibraries'] = sorted(set(entry['nativeLibraries']) | set(message['linked_libs']))
            continue
        target = message['target']
        if message['profile']['test'] or any(kind in ('test', 'bench', 'example') for kind in target['kind']):
            raise LegalError('notice build unexpectedly includes tests, examples, or benchmarks')
        unit = {'name': target['name'], 'kinds': sorted(target['kind']), 'features': sorted(message['features'])}
        if unit not in entry['units']:
            entry['units'].append(unit)
        if identity == package_id and target['name'] == binary and target['kind'] == ['bin']:
            executable = bool(message['executable'])
    if not executable:
        raise LegalError('Cargo did not produce the requested executable')
    for entry in collected.values():
        entry['units'].sort(key=lambda unit: (unit['name'], unit['kinds'], unit['features']))
    return collected


def script_output(messages: list[dict], package_id: str) -> Path:
    """The OUT_DIR of the build script that Cargo ran for `package_id`."""
    return Path(next(message['out_dir'] for message in messages
                     if message.get('reason') == 'build-script-executed' and message['package_id'] == package_id))


def discover(repo: Path, metadata: dict, messages: list[dict], package: str, reviews: list[Review],
             provenance: list[Provenance] | None = None,
             *, compiled: dict[str, dict] | None = None) -> tuple[list[Component], list[dict], list[str], str]:
    packages = {item['id']: item for item in metadata['packages']}
    own = {item['id'] for item in packages.values()
           if Path(item['manifest_path']).resolve().parent in
           {repo / 'rust' / name for name in ('client', 'server', 'core', 'net', 'http3')}}
    forks = {(text(fork, 'fork'), text(fork, 'rev')): strings(fork, 'modifiedPackages')
             for fork in map(obj, array(read_json(repo / 'legal/rust-forks.json')))}
    root = next((item['id'] for item in packages.values() if item['name'] == package and item['id'] in own), None)
    if root is None:
        raise LegalError(f'workspace package is missing: {package}')
    if compiled is None:
        compiled = artifacts(messages, root, package)
    components, inventory, failures = [], [], []
    for identity, build in compiled.items():
        item = packages.get(identity)
        if item is None:
            raise LegalError(f'compiled package is absent from locked metadata: {identity}')
        if identity in own:
            continue
        directory = Path(item['manifest_path']).resolve().parent
        source = item['source']
        if source is None:
            raise LegalError(f'unrecognized local dependency: {directory}')
        modified = False
        if source.startswith('git+'):
            location, _, revision = source[4:].partition('?rev=')
            modified_packages = forks.get((location, revision.partition('#')[0]))
            if modified_packages is None:
                raise LegalError(f'unreviewed Cargo git source: {identity}')
            modified = item['name'] in modified_packages
        matching = [review for review in reviews if review.ecosystem == 'cargo' and review.name == item['name']
                    and review.upstream == source and (source.startswith('registry+')
                                                       or review.reviewedVersion == item['version'])]
        exact = [review for review in matching if review.reviewedVersion == item['version']]
        matching = exact or matching
        if len(matching) > 1:
            raise LegalError(f'ambiguous Rust legal review: {identity}')
        expression = item.get('license') or ''
        component = Component(item['name'], item['version'], 'cargo', source, expression,
                              modified=modified, source_path=directory)
        try:
            overrides = [entry for entry in (provenance or [])
                         if (entry.name, entry.version, entry.upstream) == (component.name, component.version, source)]
            if len(overrides) > 1:
                raise LegalError(f'duplicate manual provenance: {identity}')
            if overrides:
                manual = add_provenance(repo, [], overrides, 'rust')[0]
                if manual.declaredLicenseExpression != expression or manual.modified != modified:
                    raise LegalError(f'manual provenance disagrees with package identity: {identity}')
                files = manual.legalTexts + manual.notices
            else:
                files = component_legal_files(directory, 'cargo', component.name, matching)
            # A manifest can explicitly name a file outside conventional LICENSE names.
            if explicit := item.get('license_file'):
                explicit_path = (directory / explicit).resolve()
                if not explicit_path.is_relative_to(directory):
                    raise LegalError(f'license_file escapes package: {identity}')
                if explicit_path.relative_to(directory).as_posix() not in {file.name for file in files}:
                    raise LegalError(f'license_file needs explicit review: {identity}: {explicit}')
            component.legalTexts = [file for file in files if file.kind != 'notice']
            component.notices = [file for file in files if file.kind == 'notice']
            validate_review(component, matching)
            component.selectedLicenseExpression = matching[0].selectedLicenseExpression
        except LegalError as error:
            failures.append(f'{component.name} {component.version}: {error}')
        components.append(component)
        revision = ''
        vcs = directory / '.cargo_vcs_info.json'
        if vcs.exists():
            revision = json.loads(vcs.read_text()).get('git', {}).get('sha1', '')
        if item['source'] and item['source'].startswith('git+'):
            revision = item['source'].rsplit('#', 1)[-1]
        inventory.append({'component': component.json(), 'repository': item.get('repository') or '',
                          'upstreamRevision': revision, **build})
    components.sort(key=lambda item: (item.name, item.version, item.source))
    inventory.sort(key=lambda item: (item['component']['name'], item['component']['version'], item['component']['source']))
    return components, inventory, failures, root


def verify_prepared(prepared: list[Component], actual: list[Component], failures: list[str]) -> None:
    covered = {(item.name, item.version, item.source): item.json() for item in prepared}
    if failures or any(covered.get((item.name, item.version, item.source)) != item.json() for item in actual):
        raise LegalError('final Cargo build has missing or changed dependency notices' +
                         (':\n' + '\n'.join(failures) if failures else ''))


def legal_report(repo: Path, version: str, sections: str, development: bool = False) -> bytes:
    """The --legal output: the copyright, source and LICENSE that open Go's TUI report, then the notices.

    A development build's report opens with its development notice."""
    project = Project.read(repo)
    return ((DEVELOPMENT_NOTICE.encode() if development else b'')
            + legal_header(project, release_source(project, version)[1], (repo / 'LICENSE').read_bytes()) + b'\n'
            + f'Cargo compilation-input notices (including build-time dependencies)\n\n{sections}'.encode())


def about(project: Project, version: str, engine_version: str, components: list[Component],
          repo: Path = Path(__file__).resolve().parents[2]) -> dict[str, object]:
    """The browser's about.json, whose source is a release version's tag as in Go's."""
    return {'schemaVersion': 2, 'project': project.json(), 'sourceVersion': engine_version,
            'sourceURL': release_source(project, version)[1], 'licenseURL': 'legal/LICENSE.txt',
            'noticesURL': 'legal/THIRD_PARTY_NOTICES.txt',
            'components': about_components(repo, release_source(project, version)[1], components)}


def image_additions(repo: Path, browser: list[Component], provenance: list[Provenance]) -> list[Component]:
    """What the server image ships beyond its binary, such as the CA roots: Go's container scope less its server."""
    shipped = {component_key(component) for component in browser}
    return [component for component in add_provenance(repo, browser, provenance, 'container')
            if component_key(component) not in shipped]


def review_candidates(components: list[Component]) -> list[dict]:
    return [Review(ecosystem='cargo', name=item.name, reviewedVersion=item.version,
                   upstream=item.source, declaredLicenseExpression=item.declaredLicenseExpression,
                   modified=item.modified,
                   legalFiles=[replace(file, text='') for file in item.legalTexts + item.notices]).json()
            for item in components]


def checked_platform_notice(record: dict | None, facts: dict) -> str:
    try:
        return platform.notice(record, **facts)
    except (LegalError, OSError) as error:
        candidate, listing = platform.candidate(record, **facts)
        raise LegalError(f'{error}\nUnreviewed platform record of this build:\n{marshal(candidate).decode()}'
                         'Its noticesSha256 hashes this listing of its notices:\n'
                         + listing.removesuffix('\n')) from error


def main() -> None:
    # Only the full --development selects development notices, which the workflow policy looks for.
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument('--repo', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--package', choices=PACKAGES, required=True)
    builds = parser.add_mutually_exclusive_group()
    builds.add_argument('--target', help='the reviewed cross-build target')
    parser.add_argument('--profile', choices=('dev', 'ci', 'release'), default='release')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--reviews', type=Path)
    parser.add_argument('--supplement', type=Path,
                        help='reviewed sysroot/system-library notice record for the target')
    parser.add_argument('--browser-scan', type=Path, help='Vite module scan from the matching production browser asset build')
    parser.add_argument('--version', default=os.environ.get('VERSION') or 'development',
                        help='the release version, whose tag the notices name as the source')
    parser.add_argument('--review-template', action='store_true')
    parser.add_argument('--local', action='store_true', help='omit distribution source archives')
    builds.add_argument('--development', action='store_true',
                        help='notices of an unreviewed development build on any host: they omit the platform record '
                             'and the toolchain facts it reviews, say so, and never pass release verification')
    args = parser.parse_args()
    revision = os.environ.get('GM_ENGINE_VERSION', '').removesuffix('-rust')
    if args.version == 'development' and re.fullmatch(r'[0-9a-f]{7,40}', revision):
        args.version = revision
    if args.development and (args.supplement or args.review_template):
        parser.error('--development reviews no platform')
    if not (args.target or args.development):
        parser.error('--target is required unless --development selects the host')
    try:
        build(args)
    except (LegalError, OSError, ValueError, subprocess.CalledProcessError):
        output = local_path(args.out, args.repo.resolve())
        (output / 'LEGAL.txt').unlink(missing_ok=True)
        raise


def build(args: argparse.Namespace) -> None:
    repo = args.repo.resolve()
    reviews_path = local_path(args.reviews, repo) if args.reviews else None
    supplement = local_path(args.supplement, repo) if args.supplement else None
    browser_scan = local_path(args.browser_scan, repo) if args.browser_scan else None
    reviews = ([Review.parse(item) for item in array(read_json(reviews_path))] if reviews_path else [])
    output = local_path(args.out, repo)
    output.mkdir(parents=True, exist_ok=True)
    provenance = manual_sources(repo, args.package)
    # rustup selects exactly the pinned workspace toolchain.
    channel = rust_channel(repo)
    toolchain = subprocess.check_output(['rustc', f'+{channel}', '-vV'], text=True)
    host = next(line.removeprefix('host: ') for line in toolchain.splitlines() if line.startswith('host: '))
    target = args.target or host
    link_map = platform.link_map(output, target, args.profile)
    mapped = None if args.development else link_map
    sysroot = Path(subprocess.check_output(['rustc', f'+{channel}', '--print', 'sysroot'], text=True).strip())
    record = platform.record(supplement, target) if supplement else None
    if record is not None:
        platform.fetch_notices(repo, record)
    metadata, selected = prepare(repo, args.package, host, target)
    components, inventory, failures, root = discover(repo, metadata, [], args.package, reviews, provenance,
                                                     compiled=selected)
    manifest = {'schemaVersion': 1, 'package': args.package, 'target': target,
                'profile': args.profile, 'rustc': toolchain,
                'scope': 'compiled Cargo inputs, including build scripts and procedural macros',
                'cargoLockSha256': sha256((repo / 'rust/Cargo.lock').read_bytes()), 'components': inventory}
    cargo_outputs = {Path(metadata['target_directory'])} | {
        Path(item['manifest_path']).parent for item in metadata['packages']}
    facts: dict = {'target': target, 'sysroot': sysroot, 'inputs': set(), 'libraries': set()}
    if args.review_template:
        (output / 'LEGAL.txt').unlink(missing_ok=True)
        metadata, messages = capture(repo, args.package, args.target, args.profile, mapped,
                                     link_target=target, metadata=metadata)
        _, inventory, failures, _ = discover(repo, metadata, messages, args.package, reviews, provenance)
        executable = Path(next(message['executable'] for message in messages if message.get('executable')
                               and message['target']['name'] == args.package))
        facts |= {'inputs': platform.linked(link_map, sysroot, cargo_outputs),
                  'libraries': platform.imports(executable, target)}
        (output / 'review-candidates.json').write_bytes(marshal(review_candidates(components)))
        (output / 'review-errors.json').write_bytes(marshal(failures))
        manifest['components'] = inventory
        (output / 'inventory.json').write_bytes(marshal(manifest))
        candidate, listing = platform.candidate(record, **facts)
        (output / 'platform-candidate.json').write_bytes(marshal(candidate))
        (output / 'platform-inputs.txt').write_bytes(listing.encode())
        return
    if failures:
        raise LegalError('Rust dependency notices need review:\n' + '\n'.join(failures))
    if args.development:
        extra = DEVELOPMENT_PLATFORM
    elif supplement is None:
        raise LegalError('reviewed Rust sysroot and platform-library notice supplement is required')
    else:
        extra = checked_platform_notice(record, facts)
    staged_assets = None
    if args.package == 'graphite-meter-server' and os.environ.get('GM_RUST_ASSET_DIR'):
        source_assets = (repo / 'rust/server' / os.environ['GM_RUST_ASSET_DIR']).resolve()
        if not source_assets.is_relative_to(repo):
            raise LegalError('GM_RUST_ASSET_DIR must name a directory inside the repository')
        staged_assets = output / 'browser-assets'
        stage_browser(source_assets, staged_assets)
    browser_components: list[Component] = []
    shared_notices = None
    if args.package == 'graphite-meter-server' and os.environ.get('GM_RUST_ASSET_DIR'):
        if browser_scan is None:
            raise LegalError('server with browser assets requires the matching production --browser-scan')
        browser_reviews = [Review.parse(item) for item in array(read_json(repo / 'legal/reviewed-components.json'))]
        browser_components = add_provenance(repo, discover_browser(browser_scan, browser_reviews),
                                            provenance, 'server/browser')
        image_components = image_additions(repo, browser_components, provenance)
        for component in browser_components + image_components:
            validate_review(component, browser_reviews)
            component.selectedLicenseExpression = next(review.selectedLicenseExpression for review in browser_reviews
                                                       if (review.ecosystem, review.name) == (component.ecosystem, component.name))
        manifest['browserComponents'] = [component.json() for component in browser_components]
        manifest['imageComponents'] = [component.json() for component in image_components]
        if not args.local:
            (output / 'IMAGE_NOTICES.txt').write_bytes(legal_report(
                repo, args.version, notices(components + browser_components + image_components) + '\n' + extra,
                args.development))
        assert staged_assets is not None
        legal_assets = staged_assets / 'legal'
        legal_assets.mkdir(exist_ok=True)
        write_changed(legal_assets / 'LICENSE.txt', (repo / 'LICENSE').read_bytes())
        # The browser's notices open with the development notice too; they end the report either way.
        shared_notices = ((DEVELOPMENT_NOTICE if args.development else '')
                          + notices(components + browser_components) + '\n' + extra)
        write_changed(legal_assets / 'THIRD_PARTY_NOTICES.txt', shared_notices.encode())
        write_changed(legal_assets / 'about.json', marshal(about(
            Project.read(repo), args.version, os.environ.get('GM_ENGINE_VERSION', 'rust-experimental'),
            components + browser_components, repo)))
    write_changed(output / 'LEGAL.txt', legal_report(repo, args.version, shared_notices if shared_notices is not None
                                                    else notices(components) + '\n\nRust sysroot and platform notices\n\n' + extra,
                                                    args.development))
    # Snapshot dependency-selection inputs; build.rs rejects stale supplied reports.
    inputs = ['rust/legal_build.rs', 'rust/client/build.rs', 'rust/server/build.rs', 'rust/Cargo.lock', 'rust/Cargo.toml',
              'rust/rust-toolchain.toml', 'legal/rust-forks.json', 'legal/rust-notice-sources.json']
    for reviewed_input in (reviews_path, supplement):
        if reviewed_input is not None:
            resolved = reviewed_input.resolve()
            if not resolved.is_relative_to(repo):
                raise LegalError('reviewed release inputs must be stored inside the repository')
            inputs.append(str(resolved.relative_to(repo)))
    inputs += [str(resolved.relative_to(repo)) for name in platform.own_notices(record)
               if (resolved := platform.source(name, sysroot).resolve()).is_relative_to(repo)]
    inputs += ['legal/rust-provenance.json', *(['legal/provenance.json'] if args.package == 'graphite-meter-server' else [])]
    inputs += [file.name for entry in provenance for file in entry.localLegalFiles]
    inputs += [str(Path(item['manifest_path']).resolve().relative_to(repo)) for item in metadata['packages']
               if Path(item['manifest_path']).resolve().is_relative_to(repo / 'rust')]
    for relative in sorted(set(inputs)):
        destination = output / 'inputs' / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        write_changed(destination, (repo / relative).read_bytes())
    write_changed(output / 'inputs.txt', ('\n'.join(sorted(set(inputs))) + '\n').encode())
    write_changed(output / 'package.txt', args.package.encode())
    write_changed(output / 'target.txt', target.encode())
    # build.rs requires Cargo to compile with exactly this compiler.
    write_changed(output / 'rustc-path.txt', subprocess.check_output(
        ['rustup', 'which', '--toolchain', channel, 'rustc'], text=True).strip().encode())
    try:
        built_metadata, messages = capture(repo, args.package, args.target, args.profile, mapped,
                                           output, staged_assets, link_target=target)
        actual, inventory, failures, _ = discover(repo, built_metadata, messages, args.package, reviews, provenance)
        if set(artifacts(messages, root, args.package)) - selected.keys():
            raise LegalError('final Cargo build compiled a dependency absent from the prepared inventory')
        verify_prepared(components, actual, failures)
        executable = Path(next(message['executable'] for message in messages if message.get('executable')
                               and message['target']['name'] == args.package))
        if not args.development:
            facts |= {'inputs': platform.linked(link_map, sysroot, cargo_outputs),
                      'libraries': platform.imports(executable, target)}
            if checked_platform_notice(record, facts) != extra:
                raise LegalError('linked executable platform notices changed during build')
        payload = (script_output(messages, root) / 'LEGAL.zlib').read_bytes()
        if payload not in executable.read_bytes() or zlib.decompress(payload) != (output / 'LEGAL.txt').read_bytes():
            raise LegalError('executable does not embed the generated notices')
        manifest['components'] = inventory
        write_changed(output / 'inventory.json', marshal(manifest))
    except (LegalError, OSError, ValueError, zlib.error, subprocess.CalledProcessError):
        (output / 'LEGAL.txt').unlink(missing_ok=True)
        raise
    # The offer includes actual build inputs, including code generators and native crate sources.
    if args.local or args.profile != 'release' or args.development:
        return
    with (output / 'THIRD_PARTY_SOURCE.tar.gz').open('wb') as destination:
        with gzip.GzipFile(filename='', mode='wb', fileobj=destination, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w') as archive:
                add_cargo_sources(archive, repo, actual)
                for component in browser_components:
                    if component.source_path is not None:
                        add_tree(archive, component.source_path,
                                 f'third_party/{component.ecosystem}/{component.name}-{component.version}', component.source_path)
                add_bytes(archive, 'inventory.json', (output / 'inventory.json').read_bytes())
                add_bytes(archive, 'LEGAL.txt', (output / 'LEGAL.txt').read_bytes())
                add_bytes(archive, 'legal/rust-forks.json', (repo / 'legal/rust-forks.json').read_bytes())
                # Entries can share a file, such as one licence text for two fonts; the archive holds it once.
                for path in dict.fromkeys(path for entry in provenance for path in manual_files(entry)):
                    add_tree(archive, repo / path, path, repo / path)


if __name__ == '__main__':
    try:
        main()
    except (ControlPlaneError, LegalError, OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
