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
import shutil
import subprocess
import sys
import tarfile
import tempfile
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
                      'compiled, but not its Rust standard library, C runtime or system libraries, which only the '
                      'reviewed platform records of the release builders cover. Do not distribute this build.\n\n')
DEVELOPMENT_PLATFORM = ('This development build\'s Rust standard library, C runtime and system libraries are not '
                        'reviewed, so their notices are not included.\n')


def cargo(repo: Path, *args: str) -> list[str]:
    return ['cargo', f'+{rust_channel(repo)}', *args]


def capture(repo: Path, package: str, target: str | None, profile: str, link_map: Path | None,
            legal_directory: Path | None = None, asset_directory: Path | None = None,
            *, link_target: str | None = None) -> tuple[dict, list[dict]]:
    """Own the invocation so test/workspace artifacts cannot contaminate the scan.

    Without a target it is Cargo's plain build for this host, and without a link map it maps no native inputs.
    """
    environment = dict(os.environ)
    environment.pop('GM_RUST_LEGAL_DIR', None)
    # Like Go's -trimpath, a release build names neither the checkout nor Cargo's home, which holds every
    # dependency's source; both of its builds take these flags, which build.rs keeps in the build identity.
    # A development build is followed by plain cargo, so it keeps plain cargo's flags.
    if target:
        cargo_home = os.environ.get('CARGO_HOME') or str(Path.home() / '.cargo')
        environment['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join(
            f'--remap-path-prefix={path}={name}' for path, name in ((repo, '/src'), (cargo_home, '/cargo')))
    if asset_directory is not None:
        environment['GM_RUST_ASSET_DIR'] = str(asset_directory)
    if legal_directory is not None:
        environment['GM_RUST_LEGAL_DIR'] = str(legal_directory)
    # Only the executable's link gets the map request; a fresh executable keeps the map of its last link.
    command = cargo(repo, 'rustc', '--locked', '--package', package, '--bin', package,
                    *(['--target', target] if target else []), '--profile', profile, '--message-format=json',
                    *(['--', platform.link_map_argument(link_target or target or '', link_map)] if link_map else []))
    result = subprocess.run(command, cwd=repo / 'rust', env=environment,
                            stdout=subprocess.PIPE, check=True, text=True)
    messages = []
    for line in result.stdout.splitlines():
        if line.startswith('{'):
            messages.append(json.loads(line))
    metadata = subprocess.check_output(cargo(repo, 'metadata', '--locked', '--format-version=1'),
                                       cwd=repo / 'rust', text=True)
    return json.loads(metadata), messages


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
             provenance: list[Provenance] | None = None) -> tuple[list[Component], list[dict], list[str], str]:
    packages = {item['id']: item for item in metadata['packages']}
    own = {item['id'] for item in packages.values()
           if Path(item['manifest_path']).resolve().parent in
           {repo / 'rust' / name for name in ('client', 'server', 'core', 'net', 'http3')}}
    forks = {(text(fork, 'fork'), text(fork, 'rev')): strings(fork, 'modifiedPackages')
             for fork in map(obj, array(read_json(repo / 'legal/rust-forks.json')))}
    root = next((item['id'] for item in packages.values() if item['name'] == package and item['id'] in own), None)
    if root is None:
        raise LegalError(f'workspace package is missing: {package}')
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
        matching = [review for review in reviews if (review.name, review.reviewedVersion, review.upstream)
                    == (item['name'], item['version'], source)]
        if len(matching) > 1:
            raise LegalError(f'duplicate Rust legal review: {identity}')
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


def main() -> None:
    # Only the full --development selects development notices, which the workflow policy looks for.
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument('--repo', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--package', choices=PACKAGES, required=True)
    builds = parser.add_mutually_exclusive_group()
    builds.add_argument('--target', help='the reviewed cross-build target')
    builds.add_argument('--host', action='store_true', help='review Cargo\'s plain host build against its platform record')
    parser.add_argument('--profile', choices=('dev', 'ci', 'release'), default='release')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--reviews', type=Path)
    parser.add_argument('--supplement', type=Path,
                        help='reviewed sysroot/system-library notice record bound to toolchain and target')
    parser.add_argument('--browser-scan', type=Path, help='Vite module scan from the matching production browser asset build')
    parser.add_argument('--version', default=os.environ.get('VERSION') or 'development',
                        help='the release version, whose tag the notices name as the source')
    parser.add_argument('--review-template', action='store_true')
    builds.add_argument('--development', action='store_true',
                        help='notices of an unreviewed development build on any host: they omit the platform record '
                             'and the toolchain facts it reviews, say so, and never pass release verification')
    args = parser.parse_args()
    revision = os.environ.get('GM_ENGINE_VERSION', '').removesuffix('-rust')
    if args.version == 'development' and re.fullmatch(r'[0-9a-f]{7,40}', revision):
        args.version = revision
    if args.development and (args.supplement or args.review_template):
        parser.error('--development reviews no platform')
    if not (args.target or args.host or args.development):
        parser.error('--target is required unless --host or --development selects the host')
    repo = args.repo.resolve()
    reviews_path = local_path(args.reviews, repo) if args.reviews else None
    supplement = local_path(args.supplement, repo) if args.supplement else None
    browser_scan = local_path(args.browser_scan, repo) if args.browser_scan else None
    reviews = ([Review.parse(item) for item in array(read_json(reviews_path))] if reviews_path else [])
    output = local_path(args.out, repo)
    output.mkdir(parents=True, exist_ok=True)
    # Invalidate before the build too: a failed compilation must not retain an old report.
    (output / 'LEGAL.txt').unlink(missing_ok=True)
    provenance = manual_sources(repo, args.package)
    # rustup selects exactly the pinned workspace toolchain.
    channel = rust_channel(repo)
    toolchain = subprocess.check_output(['rustc', f'+{channel}', '-vV'], text=True)
    target = args.target or next(line.removeprefix('host: ') for line in toolchain.splitlines()
                                 if line.startswith('host: '))
    if args.host and supplement is None:
        native_compiler = platform.linker_version(target)
        matches = [path for path in sorted((repo / 'legal').glob('rust-platform-*.json'))
                   if (entry := platform.record(path, target)) is not None
                   and entry.get('nativeCompiler') == native_compiler]
        if len(matches) > 1:
            raise LegalError(f'duplicate host platform reviews for {target}: {matches}')
        supplement = matches[0] if matches else None
    link_map = platform.link_map(output, target, args.profile)
    # A development build reads no native inputs or imports: only a platform record, which it lacks, reviews them.
    mapped = None if args.development else link_map
    metadata, messages = capture(repo, args.package, args.target, args.profile, mapped, link_target=target)
    components, inventory, failures, root = discover(repo, metadata, messages, args.package, reviews, provenance)
    manifest = {'schemaVersion': 1, 'package': args.package, 'target': target,
                'profile': args.profile, 'rustc': toolchain,
                'scope': 'compiled Cargo inputs, including build scripts and procedural macros',
                'cargoLockSha256': sha256((repo / 'rust/Cargo.lock').read_bytes()), 'components': inventory}
    (output / 'inventory.json').write_bytes(marshal(manifest))
    sysroot = Path(subprocess.check_output(['rustc', f'+{channel}', '--print', 'sysroot'], text=True).strip())
    executable = Path(next(message['executable'] for message in messages
                           if message.get('reason') == 'compiler-artifact' and message.get('executable')
                           and message['target']['name'] == args.package))
    cargo_outputs = {Path(metadata['target_directory'])} | {
        Path(item['manifest_path']).parent for item in metadata['packages']}
    facts = {'target': target, 'compiler': toolchain, 'sysroot': sysroot,
             'inputs': set() if args.development else platform.linked(link_map, sysroot, cargo_outputs),
             'libraries': set() if args.development else platform.imports(executable, target)}
    record = platform.record(supplement, target) if supplement else None
    if args.review_template:
        (output / 'review-candidates.json').write_bytes(marshal(review_candidates(components)))
        (output / 'review-errors.json').write_bytes(marshal(failures))
        candidate, listing = platform.candidate(record, **facts)
        (output / 'platform-candidate.json').write_bytes(marshal(candidate))
        (output / 'platform-inputs.txt').write_bytes(listing.encode())
        return
    if failures:
        raise LegalError('Rust dependency notices need review:\n' + '\n'.join(failures))
    if args.development:
        extra = DEVELOPMENT_PLATFORM
    elif supplement is None and not args.host:
        raise LegalError('reviewed Rust sysroot and platform-library notice supplement is required')
    else:
        try:
            extra = platform.notice(record, **facts)
        except (LegalError, OSError) as error:
            candidate, listing = platform.candidate(record, **facts)
            # Exiting adds the final line break: the output ends with exactly the listing inputsSha256 hashes.
            raise LegalError(f'{error}\nUnreviewed platform record of this build:\n{marshal(candidate).decode()}'
                             'Its inputsSha256 hashes this listing of its inputs:\n'
                             + listing.removesuffix('\n')) from error
    browser_components: list[Component] = []
    staged_assets = None
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
        (output / 'inventory.json').write_bytes(marshal(manifest))
        (output / 'IMAGE_NOTICES.txt').write_bytes(legal_report(
            repo, args.version, notices(components + browser_components + image_components) + '\n' + extra,
            args.development))
        source_assets = os.path.realpath(repo / 'rust/server' / os.environ['GM_RUST_ASSET_DIR'])
        if not source_assets.startswith(str(repo) + os.sep):
            raise LegalError('GM_RUST_ASSET_DIR must name a directory inside the repository')
        staged_assets = output / 'browser-assets'
        if staged_assets.exists():
            shutil.rmtree(staged_assets)
        shutil.copytree(source_assets, staged_assets, symlinks=True)
        if any(path.is_symlink() for path in staged_assets.rglob('*')):
            raise LegalError('browser legal staging does not accept symbolic links')
        legal_assets = staged_assets / 'legal'
        legal_assets.mkdir(exist_ok=True)
        (legal_assets / 'LICENSE.txt').write_bytes((repo / 'LICENSE').read_bytes())
        # The browser's notices open with the development notice too; they end the report either way.
        shared_notices = ((DEVELOPMENT_NOTICE if args.development else '')
                          + notices(components + browser_components) + '\n' + extra)
        (legal_assets / 'THIRD_PARTY_NOTICES.txt').write_bytes(shared_notices.encode())
        (legal_assets / 'about.json').write_bytes(marshal(about(
            Project.read(repo), args.version, os.environ.get('GM_ENGINE_VERSION', 'rust-experimental'),
            components + browser_components, repo)))
    (output / 'LEGAL.txt').write_bytes(legal_report(repo, args.version, shared_notices if shared_notices is not None
                                                    else notices(components) + '\n\nRust sysroot and platform notices\n\n' + extra,
                                                    args.development))
    # Snapshot dependency-selection inputs; build.rs rejects stale supplied reports.
    inputs = ['rust/legal_build.rs', 'rust/client/build.rs', 'rust/server/build.rs', 'rust/Cargo.lock', 'rust/Cargo.toml',
              'rust/rust-toolchain.toml', 'legal/rust-forks.json']
    for reviewed_input in (args.reviews, args.supplement):
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
        destination.write_bytes((repo / relative).read_bytes())
    (output / 'inputs.txt').write_text('\n'.join(sorted(set(inputs))) + '\n')
    (output / 'package.txt').write_text(args.package)
    (output / 'target.txt').write_text(target)
    # build.rs requires Cargo to compile with exactly this compiler.
    (output / 'rustc-path.txt').write_text(subprocess.check_output(
        ['rustup', 'which', '--toolchain', channel, 'rustc'], text=True).strip())
    (output / 'build-identity.txt').write_bytes((script_output(messages, root) / 'legal-build-identity.txt').read_bytes())
    try:
        rebuilt_metadata, rebuilt_messages = capture(repo, args.package, args.target, args.profile, mapped,
                                                     output, staged_assets, link_target=target)
        _, rebuilt_inventory, rebuilt_failures, _ = discover(repo, rebuilt_metadata, rebuilt_messages, args.package,
                                                             reviews, provenance)
        if rebuilt_failures or rebuilt_inventory != inventory or not args.development and (
                platform.linked(link_map, sysroot, cargo_outputs) != facts['inputs']
                or platform.imports(executable, target) != facts['libraries']):
            raise LegalError('embedded-notice rebuild changed the compiled dependency or native closure')
        payload = (script_output(rebuilt_messages, root) / 'LEGAL.zlib').read_bytes()
        if payload not in executable.read_bytes() or zlib.decompress(payload) != (output / 'LEGAL.txt').read_bytes():
            raise LegalError('executable does not embed the generated notices')
    except (LegalError, OSError, ValueError, zlib.error, subprocess.CalledProcessError):
        (output / 'LEGAL.txt').unlink(missing_ok=True)
        raise
    # Only shipped (reviewed release) builds bundle their compilation input sources, including native
    # code nested in crates, with unchanged license files and explicit local-patch provenance.
    if args.profile != 'release' or args.development:
        return
    with (output / 'THIRD_PARTY_SOURCE.tar.gz').open('wb') as destination:
        with gzip.GzipFile(filename='', mode='wb', fileobj=destination, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w') as archive:
                add_cargo_sources(archive, repo, components)
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
