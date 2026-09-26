"""Collect Rust notices from one real Cargo binary build, never the resolved graph alone.

Run `python3 -m scripts.legal.rust --help`. The inventory is a conservative
compilation-input superset: build scripts and procedural macros are retained.
It does not infer the licensing of the Rust sysroot or system libraries.
"""
from __future__ import annotations

import argparse
import gzip
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tomllib
from dataclasses import replace
from pathlib import Path

from .artifacts import add_bytes, add_tree, notices
from .discovery import discover_browser
from .model import Component, LegalError, Project, Provenance, Review, array, marshal, read_json, sha256
from .review import add_provenance, component_legal_files, validate_review
from .rust_platform import notice as platform_notice, verify_dynamic_runtime

PACKAGES = ('graphite-meter-client', 'graphite-meter-server')


def cargo(repo: Path, *args: str) -> list[str]:
    toolchain = tomllib.loads((repo / 'rust/rust-toolchain.toml').read_text())['toolchain']['channel']
    return ['cargo', f'+{toolchain}', *args]


def capture(repo: Path, package: str, target: str, profile: str,
            legal_directory: Path | None = None, asset_directory: Path | None = None) -> tuple[dict, list[dict]]:
    """Own the invocation so test/workspace artifacts cannot contaminate the scan."""
    environment = dict(os.environ)
    environment.pop('GM_RUST_LEGAL_DIR', None)
    if asset_directory is not None:
        environment['GM_RUST_ASSET_DIR'] = str(asset_directory)
    if legal_directory is not None:
        environment['GM_RUST_LEGAL_DIR'] = str(legal_directory)
    command = cargo(repo, 'build', '--locked', '--package', package, '--bin', package,
                    '--target', target, '--profile', profile, '--message-format=json')
    result = subprocess.run(command, cwd=repo / 'rust', env=environment,
                            stdout=subprocess.PIPE, check=True, text=True)
    messages = []
    for line in result.stdout.splitlines():
        if line.startswith('{'):
            messages.append(json.loads(line))
    metadata = subprocess.check_output(cargo(repo, 'metadata', '--locked', '--format-version=1'),
                                       cwd=repo / 'rust', text=True)
    return json.loads(metadata), messages


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


def discover(repo: Path, metadata: dict, messages: list[dict], package: str,
             reviews: list[Review], provenance: list[Provenance] | None = None) -> tuple[list[Component], list[dict], list[str]]:
    packages = {item['id']: item for item in metadata['packages']}
    own = {item['id'] for item in packages.values()
           if Path(item['manifest_path']).resolve().parent in
           {repo / 'rust' / name for name in ('client', 'server', 'core')}}
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
        modified = item['source'] is None
        if modified and not directory.is_relative_to(repo / 'rust/vendor'):
            raise LegalError(f'unrecognized local dependency: {directory}')
        source = (f'https://github.com/zR-JB/graphite-meter/tree/main/{directory.relative_to(repo)}'
                  if modified else item['source'])
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
    return components, inventory, failures


def review_candidates(components: list[Component]) -> list[dict]:
    return [Review(ecosystem='cargo', name=item.name, reviewedVersion=item.version,
                   upstream=item.source, declaredLicenseExpression=item.declaredLicenseExpression,
                   modified=item.modified,
                   legalFiles=[replace(file, text='') for file in item.legalTexts + item.notices]).json()
            for item in components]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--package', choices=PACKAGES, required=True)
    parser.add_argument('--target', required=True)
    parser.add_argument('--profile', choices=('dev', 'release'), default='release')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--reviews', type=Path)
    parser.add_argument('--supplement', type=Path,
                        help='reviewed sysroot/system-library notice record bound to toolchain and target')
    parser.add_argument('--browser-scan', type=Path, help='Vite module scan from the matching production browser asset build')
    parser.add_argument('--review-template', action='store_true')
    args = parser.parse_args()
    repo = args.repo.resolve()
    reviews = ([Review.parse(item) for item in array(read_json(args.reviews))] if args.reviews else [])
    output = args.out.resolve()
    output.mkdir(parents=True, exist_ok=True)
    # Invalidate before the build too: a failed compilation must not retain an old report.
    (output / 'LEGAL.txt').unlink(missing_ok=True)
    provenance_path = repo / 'legal/rust-provenance.json'
    provenance = [Provenance.parse(item) for item in array(read_json(provenance_path))] if provenance_path.exists() else []
    metadata, messages = capture(repo, args.package, args.target, args.profile)
    components, inventory, failures = discover(repo, metadata, messages, args.package, reviews, provenance)
    # rustup selects exactly the pinned workspace toolchain.
    channel = tomllib.loads((repo / 'rust/rust-toolchain.toml').read_text())['toolchain']['channel']
    toolchain = subprocess.check_output(['rustc', f'+{channel}', '-vV'], text=True)
    manifest = {'schemaVersion': 1, 'package': args.package, 'target': args.target,
                'profile': args.profile, 'rustc': toolchain,
                'scope': 'compiled Cargo inputs, including build scripts and procedural macros',
                'cargoLockSha256': sha256((repo / 'rust/Cargo.lock').read_bytes()), 'components': inventory}
    (output / 'inventory.json').write_bytes(marshal(manifest))
    if args.review_template:
        (output / 'review-candidates.json').write_bytes(marshal(review_candidates(components)))
        (output / 'review-errors.json').write_bytes(marshal(failures))
        return
    if failures:
        raise LegalError('Rust dependency notices need review:\n' + '\n'.join(failures))
    if args.supplement is None:
        raise LegalError('reviewed Rust sysroot and platform-library notice supplement is required')
    extra = platform_notice(args.supplement, target=args.target, compiler=toolchain, channel=channel)
    executable = Path(next(message['executable'] for message in messages
                           if message.get('reason') == 'compiler-artifact' and message.get('executable')
                           and message['target']['name'] == args.package))
    verify_dynamic_runtime(executable)
    browser_components: list[Component] = []
    browser_provenance: list[Provenance] = []
    staged_assets = None
    shared_notices = None
    if args.package == 'graphite-meter-server' and os.environ.get('GM_RUST_ASSET_DIR'):
        if args.browser_scan is None:
            raise LegalError('server with browser assets requires the matching production --browser-scan')
        browser_reviews = [Review.parse(item) for item in array(read_json(repo / 'legal/reviewed-components.json'))]
        browser_provenance = [entry for item in array(read_json(repo / 'legal/provenance.json'))
                              if 'server/browser' in (entry := Provenance.parse(item)).artifactScopes]
        browser_components = add_provenance(repo, discover_browser(args.browser_scan, browser_reviews),
                                            browser_provenance, 'server/browser')
        for component in browser_components:
            validate_review(component, browser_reviews)
            component.selectedLicenseExpression = next(review.selectedLicenseExpression for review in browser_reviews
                                                       if (review.ecosystem, review.name) == (component.ecosystem, component.name))
        manifest['browserComponents'] = [component.json() for component in browser_components]
        (output / 'inventory.json').write_bytes(marshal(manifest))
        source_assets = os.path.realpath(repo / 'rust/server' / os.environ['GM_RUST_ASSET_DIR'])
        if not source_assets.startswith(str(repo) + os.sep):
            raise LegalError('GM_RUST_ASSET_DIR must name a directory inside the repository')
        staged_assets = output / 'browser-assets'
        if staged_assets.exists():
            shutil.rmtree(staged_assets)
        shutil.copytree(source_assets, staged_assets, symlinks=True)
        if any(path.is_symlink() for path in staged_assets.rglob('*')):
            raise LegalError('browser legal staging does not accept symbolic links')
        project = Project.read(repo)
        legal_assets = staged_assets / 'legal'
        legal_assets.mkdir(exist_ok=True)
        (legal_assets / 'LICENSE.txt').write_bytes((repo / 'LICENSE').read_bytes())
        shared_notices = notices(components + browser_components) + '\n' + extra
        (legal_assets / 'THIRD_PARTY_NOTICES.txt').write_text(shared_notices)
        (legal_assets / 'about.json').write_bytes(marshal({
            'schemaVersion': 2, 'project': project.json(), 'sourceVersion': os.environ.get('GM_ENGINE_VERSION', 'rust-experimental'),
            'sourceURL': project.repository, 'licenseURL': 'legal/LICENSE.txt',
            'noticesURL': 'legal/THIRD_PARTY_NOTICES.txt',
            'components': [{key: value for key, value in component.json().items()
                            if key not in ('legalTexts', 'notices')} for component in components + browser_components],
        }))
    report = ('Graphite Meter experimental Rust binary\n\n' + (repo / 'LICENSE').read_text()
              + '\n\nCargo compilation-input notices (including build-time dependencies)\n\n'
              + (shared_notices if shared_notices is not None
                 else notices(components + browser_components) + '\n\nRust sysroot and platform notices\n\n' + extra))
    (output / 'LEGAL.txt').write_text(report)
    # Snapshot dependency-selection inputs; build.rs rejects stale supplied reports.
    inputs = ['rust/legal_build.rs', 'rust/client/build.rs', 'rust/server/build.rs', 'rust/Cargo.lock', 'rust/Cargo.toml', 'rust/rust-toolchain.toml']
    if (repo / 'rust/vendor/PATCHES.md').exists():
        inputs.append('rust/vendor/PATCHES.md')
    for reviewed_input in (args.reviews, args.supplement):
        if reviewed_input is not None:
            resolved = reviewed_input.resolve()
            if not resolved.is_relative_to(repo):
                raise LegalError('reviewed release inputs must be stored inside the repository')
            inputs.append(str(resolved.relative_to(repo)))
    if provenance_path.exists():
        inputs.append('legal/rust-provenance.json')
        inputs.extend(file.name for entry in provenance for file in entry.localLegalFiles)
    inputs += [str(Path(item['manifest_path']).resolve().relative_to(repo)) for item in metadata['packages']
               if Path(item['manifest_path']).resolve().is_relative_to(repo / 'rust')]
    for relative in sorted(set(inputs)):
        destination = output / 'inputs' / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes((repo / relative).read_bytes())
    (output / 'inputs.txt').write_text('\n'.join(sorted(set(inputs))) + '\n')
    (output / 'package.txt').write_text(args.package)
    (output / 'target.txt').write_text(args.target)
    # build.rs requires Cargo to compile with exactly this compiler.
    (output / 'rustc-path.txt').write_text(subprocess.check_output(
        ['rustup', 'which', '--toolchain', channel, 'rustc'], text=True).strip())
    root_directory = 'client' if args.package == 'graphite-meter-client' else 'server'
    root_id = next(item['id'] for item in metadata['packages']
                   if Path(item['manifest_path']).resolve() == repo / f'rust/{root_directory}/Cargo.toml')
    script = next(message for message in messages
                  if message.get('reason') == 'build-script-executed' and message['package_id'] == root_id)
    identity = (Path(script['out_dir']) / 'legal-build-identity.txt').read_bytes()
    (output / 'build-identity.txt').write_bytes(identity)
    try:
        rebuilt_metadata, rebuilt_messages = capture(repo, args.package, args.target, args.profile, output, staged_assets)
        _, rebuilt_inventory, rebuilt_failures = discover(repo, rebuilt_metadata, rebuilt_messages, args.package, reviews, provenance)
        verify_dynamic_runtime(executable)
        if rebuilt_failures or rebuilt_inventory != inventory:
            raise LegalError('embedded-notice rebuild changed the compiled dependency closure')
    except (LegalError, OSError, ValueError, subprocess.CalledProcessError):
        (output / 'LEGAL.txt').unlink(missing_ok=True)
        raise
    # Compilation input sources, including native code nested in crates, are
    # bundled with unchanged license files and explicit local-patch provenance.
    with (output / 'THIRD_PARTY_SOURCE.tar.gz').open('wb') as destination:
        with gzip.GzipFile(filename='', mode='wb', fileobj=destination, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w') as archive:
                for component in components + browser_components:
                    if component.source_path is not None:
                        add_tree(archive, component.source_path,
                                 f'third_party/{component.ecosystem}/{component.name}-{component.version}')
                add_bytes(archive, 'inventory.json', (output / 'inventory.json').read_bytes())
                add_bytes(archive, 'LEGAL.txt', (output / 'LEGAL.txt').read_bytes())
                if (repo / 'rust/vendor/PATCHES.md').exists():
                    add_bytes(archive, 'rust/vendor/PATCHES.md', (repo / 'rust/vendor/PATCHES.md').read_bytes())
                for entry in provenance + browser_provenance:
                    for file in entry.localLegalFiles:
                        add_bytes(archive, file.name, (repo / file.name).read_bytes())
                    for path in entry.localPaths:
                        add_tree(archive, repo / path, path)


if __name__ == '__main__':
    try:
        main()
    except (LegalError, OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
