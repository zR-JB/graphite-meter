from __future__ import annotations

import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch
from copy import deepcopy

from scripts.ci.github_api import ControlPlaneError
from scripts.legal.fixtures import CheckoutTests
from scripts.legal.model import (Component, Json, LegalError, Provenance, Review, array, manual_files,
                                 manual_sources, marshal, read_json, sha256)
from scripts.legal.review import add_provenance, validate_review
from scripts.legal.rust import (DEVELOPMENT, DEVELOPMENT_NOTICE, add_cargo_sources, artifacts, capture, cargo,
                                checked_platform_notice,
                                image_additions, legal_report, reusable, stage_browser, write_changed)
from scripts.legal.rust_platform import SYSROOT, candidate, fetch_notices, imports, link_map, linked, linker_version, notice
from scripts.legal.rustc_inspect import MARKER, inspection_arguments

ROOT = Path(__file__).resolve().parents[2]


class RustFreshnessTests(CheckoutTests):
    def test_reuse_requires_successful_validation_and_identical_dependency_inputs(self) -> None:
        output = self.root / 'output'
        for name, data in {'rust/Cargo.lock': b'locked', 'output/inputs/rust/Cargo.lock': b'locked',
                           'output/inputs.txt': b'rust/Cargo.lock\n', 'output/LEGAL.txt': b'notices',
                           'output/invocation.json': b'configuration'}.items():
            write_changed(self.root / name, data)
        self.assertTrue(reusable(output, self.root, b'configuration'))
        self.assertFalse(reusable(output, self.root, b'changed compiler flags'))
        write_changed(self.root / 'rust/Cargo.lock', b'new dependency')
        self.assertFalse(reusable(output, self.root, b'configuration'))
        write_changed(self.root / 'rust/Cargo.lock', b'locked')
        (output / 'invocation.json').unlink()
        self.assertFalse(reusable(output, self.root, b'configuration'))

    def test_staging_preserves_identical_assets_and_rust_notices_but_removes_deleted_assets(self) -> None:
        source, target = self.root / 'source', self.root / 'target'
        for path, data in ((source / 'index.html', b'page'), (source / 'obsolete.js', b'old'),
                           (source / 'legal/about.json', b'Go'), (target / 'legal/about.json', b'Rust')):
            write_changed(path, data)
        self.assertTrue(stage_browser(source, target))
        original = (target / 'index.html').stat().st_mtime_ns
        self.assertFalse(stage_browser(source, target))
        self.assertEqual((target / 'index.html').stat().st_mtime_ns, original)
        self.assertEqual((target / 'legal/about.json').read_bytes(), b'Rust')
        (source / 'obsolete.js').unlink()
        self.assertTrue(stage_browser(source, target))
        self.assertFalse((target / 'obsolete.js').exists())
        (source / 'escape').symlink_to(target, target_is_directory=True)
        with self.assertRaisesRegex(LegalError, 'symbolic links'):
            stage_browser(source, target)

    def test_failed_regeneration_invalidates_the_old_notice_and_reuse_marker(self) -> None:
        from scripts.legal.rust import main
        output = self.root / 'output'
        for name in ('LEGAL.txt', 'invocation.json'):
            write_changed(output / name, b'previous success')
        with patch.object(sys, 'argv', ['rust', '--host', '--local', '--package', 'graphite-meter-client',
                                        '--out', str(output)]), \
                patch('scripts.legal.rust.build', side_effect=LegalError('unreviewed dependency')):
            with self.assertRaisesRegex(LegalError, 'unreviewed dependency'):
                main()
        self.assertFalse((output / 'LEGAL.txt').exists())
        self.assertFalse((output / 'invocation.json').exists())

    def test_staging_allows_assets_to_change_between_files_and_directories(self) -> None:
        source, target = self.root / 'source', self.root / 'target'
        write_changed(source / 'index.html', b'page')
        write_changed(source / 'asset', b'file')
        stage_browser(source, target)
        (source / 'asset').unlink()
        write_changed(source / 'asset/nested/data', b'nested')
        self.assertTrue(stage_browser(source, target))
        self.assertEqual((target / 'asset/nested/data').read_bytes(), b'nested')
        shutil.rmtree(source / 'asset')
        write_changed(source / 'asset', b'file again')
        self.assertTrue(stage_browser(source, target))
        self.assertEqual((target / 'asset').read_bytes(), b'file again')
        self.assertFalse(stage_browser(source, target))

    def test_review_template_cannot_retain_an_earlier_successful_report(self) -> None:
        from scripts.legal.rust import main

        output = self.root / 'output'
        write_changed(output / 'LEGAL.txt', b'previously approved')
        write_changed(output / 'invocation.json', b'previous success')
        write_changed(self.root / 'rust/Cargo.lock', b'locked')
        metadata = {'target_directory': str(self.root / 'target'), 'packages': []}
        messages = [{'reason': 'compiler-artifact', 'executable': str(self.root / 'binary'),
                     'target': {'name': 'graphite-meter-client'}}]
        with patch.object(sys, 'argv', ['rust', '--repo', str(self.root), '--target', 'target',
                                        '--review-template', '--package', 'graphite-meter-client',
                                        '--out', str(output)]), \
                patch('scripts.legal.rust.manual_sources', return_value=[]), \
                patch('scripts.legal.rust.rust_channel', return_value='pinned'), \
                patch('scripts.legal.rust.subprocess.check_output', side_effect=['host: target\n', str(self.root / 'sysroot')]), \
                patch('scripts.legal.rust.capture', return_value=(metadata, messages)), \
                patch('scripts.legal.rust.discover', return_value=([], [], ['unreviewed dependency'], 'root')), \
                patch('scripts.legal.rust.platform.linked', return_value=set()), \
                patch('scripts.legal.rust.platform.imports', return_value=set()), \
                patch('scripts.legal.rust.platform.candidate', return_value=({'reviewDecision': 'pending'}, 'listing\n')):
            main()
        self.assertTrue((output / 'platform-candidate.json').is_file())
        self.assertEqual(json.loads((output / 'review-errors.json').read_text()), ['unreviewed dependency'])
        self.assertFalse((output / 'LEGAL.txt').exists())
        self.assertFalse((output / 'invocation.json').exists())


class RustBrowserFreshnessTests(CheckoutTests):
    def test_browser_reuse_tracks_external_check_inputs_and_validated_outputs(self) -> None:
        from scripts import rust_build

        run = subprocess.run
        run(['git', 'init', '--quiet', str(self.root)], check=True)
        for name, data in {'client/src/app.ts': b'export {}', 'api/golden.json': b'{}',
                           'go/internal/auth/assets/auth.js': b'auth', '.gitignore': b'*.local\n',
                           'client/.env.local': b'VITE_VALUE=one', 'client/public/obsolete.js': b'old',
                           'scripts/rust_build.py': b'build instructions'}.items():
            write_changed(self.root / name, data)
        built = 0
        fail = False

        def tool(command: list[str], **options: Any) -> subprocess.CompletedProcess:
            nonlocal built
            if command == ['bun', '--version']:
                return subprocess.CompletedProcess(command, 0, stdout='test-bun\n')
            if command == ['bun', 'run', 'build']:
                built += 1
                if fail:
                    raise subprocess.CalledProcessError(1, command)
                environment = options['env']
                write_changed(Path(environment['GM_LEGAL_SCAN_DIR']) / 'index.html', b'page')
                for path in (self.root / 'client/public').rglob('*'):
                    if path.is_file():
                        write_changed(Path(environment['GM_LEGAL_SCAN_DIR']) / path.name, path.read_bytes())
                write_changed(Path(environment['GM_LEGAL_SCAN_OUT']), b'[]')
                return subprocess.CompletedProcess(command, 0)
            return run(command, **options)

        with patch.object(rust_build, 'ROOT', self.root), patch('subprocess.run', side_effect=tool):
            environment = {'GM_CLIENT_REVISION': 'source'}
            assets, _ = rust_build.browser('prod', environment)
            rust_build.browser('prod', environment)
            write_changed(self.root / 'rust/server/src/main.rs', b'fn main() {}')
            rust_build.browser('prod', environment | {'GM_H1_ADDR': '127.0.0.1:12345'})
            self.assertEqual(built, 1)
            self.assertTrue((assets / 'obsolete.js').is_file())
            (self.root / 'client/public/obsolete.js').unlink()
            write_changed(self.root / 'go/internal/auth/assets/auth.js', b'changed auth')
            rust_build.browser('prod', environment)
            self.assertEqual(built, 2)
            self.assertFalse((assets / 'obsolete.js').exists())
            (self.root / 'api/golden.json').unlink()
            rust_build.browser('prod', environment)
            self.assertEqual(built, 3)
            write_changed(self.root / 'client/.env.local', b'VITE_VALUE=two')
            rust_build.browser('prod', environment)
            self.assertEqual(built, 4)
            write_changed(assets / 'index.html', b'corrupt')
            fail = True
            with self.assertRaises(subprocess.CalledProcessError):
                rust_build.browser('prod', environment)
            self.assertFalse((assets.parent / 'build.json').exists())
            fail = False
            rust_build.browser('prod', environment)
            self.assertEqual(built, 6)
            write_changed(self.root / 'scripts/rust_build.py', b'changed build instructions')
            rust_build.browser('prod', environment)
            self.assertEqual(built, 7)


class RustArtifactTests(unittest.TestCase):
    def test_inspection_rewrites_only_the_selected_root_library_emission(self) -> None:
        arguments = ['rustc', '--crate-name', 'application', '--crate-type', 'lib',
                     '--emit=dep-info,metadata,link', '--emit', 'llvm-ir', '-C', 'lto=fat']
        self.assertEqual(inspection_arguments(arguments, 'application'), arguments)
        self.assertEqual(inspection_arguments([*arguments, MARKER], 'application'),
                         ['rustc', '--crate-name', 'application', '--crate-type', 'lib', '-C', 'lto=fat',
                          '--emit=dep-info,metadata'])
        for invalid in (arguments + [MARKER, MARKER], arguments + [MARKER, '--test'],
                        [value.replace('application', 'other') for value in arguments] + [MARKER],
                        [value.replace('lib', 'bin') for value in arguments] + [MARKER]):
            with self.subTest(arguments=invalid), self.assertRaises(ValueError):
                inspection_arguments(invalid, 'application')

    def messages(self) -> list[dict]:
        return [
            {'reason': 'compiler-artifact', 'package_id': 'dependency',
             'target': {'name': 'dependency', 'kind': ['lib']},
             'profile': {'test': False}, 'features': ['b', 'a'],
             'fresh': True, 'executable': None},
            {'reason': 'build-script-executed', 'package_id': 'dependency',
             'linked_libs': ['static=crypto']},
            {'reason': 'compiler-artifact', 'package_id': 'application',
             'target': {'name': 'application', 'kind': ['bin']},
             'profile': {'test': False}, 'features': [],
             'fresh': True, 'executable': '/target/application'},
            {'reason': 'build-finished', 'success': True},
        ]

    def test_cached_build_preserves_features_and_native_libraries(self) -> None:
        result = artifacts(self.messages(), 'application', 'application')
        self.assertEqual(result['dependency']['units'][0]['features'], ['a', 'b'])
        self.assertEqual(result['dependency']['nativeLibraries'], ['static=crypto'])
        self.assertEqual(set(result), {'application', 'dependency'})

    def test_inspection_accepts_a_library_but_final_validation_requires_the_executable(self) -> None:
        messages = self.messages()
        messages[2]['target']['kind'] = ['lib']
        messages[2]['executable'] = None
        result = artifacts(messages, 'application', 'application', inspection=True)
        self.assertEqual(result['dependency'], artifacts(self.messages(), 'application', 'application')['dependency'])
        with self.assertRaisesRegex(LegalError, 'requested executable'):
            artifacts(messages, 'application', 'application')
        messages[2]['target']['name'] = 'other'
        with self.assertRaisesRegex(LegalError, 'requested library'):
            artifacts(messages, 'application', 'application', inspection=True)

    def test_failed_truncated_or_wrong_binary_build_is_rejected(self) -> None:
        messages = self.messages()
        for bad in (messages[:-1], messages[:-1] + [{'reason': 'build-finished', 'success': False}], messages[:2] + messages[-1:]):
            with self.subTest(messages=bad), self.assertRaises(LegalError):
                artifacts(bad, 'application', 'application')

    def test_tests_and_examples_cannot_expand_release_closure(self) -> None:
        for kind in ('test', 'example', 'bench'):
            messages = deepcopy(self.messages())
            messages[0]['target']['kind'] = [kind]
            with self.subTest(kind=kind), self.assertRaises(LegalError):
                artifacts(messages, 'application', 'application')


class RustPackageTests(CheckoutTests):
    def test_packaging_takes_listed_platforms_and_writes_only_inside_its_roots(self) -> None:
        from scripts.package_rust import build

        supplement = ROOT / 'legal/rust-platform-debian-bookworm.json'
        with patch('subprocess.run', side_effect=AssertionError('built')):
            with self.assertRaisesRegex(ValueError, 'no TUI target for plan9/amd64'):
                build('1.2.3', 'plan9/amd64', self.root, supplement)
            with self.assertRaisesRegex(ValueError, 'no TUI target for darwin/arm64'):
                build('1.2.3', 'darwin/arm64', self.root, supplement)
            with self.assertRaisesRegex(ControlPlaneError, 'is outside'):
                build('1.2.3', 'linux/amd64', Path('/'), supplement)
            with self.assertRaisesRegex(ValueError, 'invalid release version'):
                build('1.2.3;id', 'linux/amd64', self.root, supplement)

    @unittest.skipUnless(os.name == 'posix', 'the fake executable is a shell script')
    def test_a_build_for_this_system_and_architecture_reports_its_version(self) -> None:
        import scripts.package_rust as package
        from scripts.ci.toolchains import host_platform, rust_tui_targets

        platforms = rust_tui_targets(ROOT / 'scripts/tui-targets.txt')
        if host_platform() not in platforms:
            self.skipTest(f'no TUI ships for {host_platform()}')
        # The builder links musl or MinGW, whose executables run wherever their system and machine match.
        other = next(platform for platform in platforms if platform.split('/')[0] != host_platform().split('/')[0])
        run = subprocess.run
        for name in ('LICENSE', 'COPYRIGHT', 'scripts/tui-targets.txt'):
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, self.root / name)
        ran, reported = self.root / 'ran', {'version': '1.2.3-rust'}

        def legal_build(command: list[str], **options: Any) -> object:
            if command[0] != sys.executable:
                return run(command, **options)
            out, target = (Path(command[command.index(flag) + 1]) for flag in ('--out', '--target'))
            out.mkdir(parents=True)
            (out / 'LEGAL.txt').write_text('notices\n')
            (out / 'THIRD_PARTY_SOURCE.tar.gz').write_bytes(b'source')
            binary = self.root / 'rust/target' / target.name / 'release' / (
                'graphite-meter-client.exe' if '-windows-' in target.name else 'graphite-meter-client')
            binary.parent.mkdir(parents=True, exist_ok=True)
            binary.write_text(f"#!/bin/sh\ntouch '{ran}'\necho graphite-meter-client {reported['version']}\n")
            binary.chmod(0o755)
            return None

        with patch.object(package, 'REPO', self.root), patch('subprocess.run', side_effect=legal_build):
            package.build('1.2.3', other, self.root / 'dist', ROOT / 'legal/rust-platform-debian-bookworm.json')
            self.assertFalse(ran.exists())
            package.build('1.2.3', host_platform(), self.root / 'dist', ROOT / 'legal/rust-platform-debian-bookworm.json')
            self.assertTrue(ran.exists())
            reported['version'] = '1.2.2-rust'
            with self.assertRaisesRegex(ValueError, "reports 'graphite-meter-client 1.2.2-rust'"):
                package.build('1.2.3', host_platform(), self.root / 'dist', ROOT / 'legal/rust-platform-debian-bookworm.json')


class RustLegalReportTests(unittest.TestCase):
    def test_a_development_report_opens_by_saying_that_no_review_covers_it(self) -> None:
        sections = 'THIRD-PARTY SOFTWARE NOTICES\n'
        report = legal_report(ROOT, 'development', sections, development=True)
        self.assertTrue(report.startswith(b'UNREVIEWED DEVELOPMENT BUILD\n\n'))
        self.assertEqual(report, DEVELOPMENT_NOTICE.encode() + legal_report(ROOT, 'development', sections))


class RustDevelopmentTests(unittest.TestCase):
    def test_only_a_development_build_goes_without_a_target_and_a_platform_record(self) -> None:
        for args, error in ((['--out', 'x'], '--target is required'),
                            (['--development', '--supplement', 'legal/rust-platform-debian-bookworm.json', '--out', 'x'],
                             '--development reviews no platform'),
                            (['--development', '--review-template', '--out', 'x'], '--development reviews no platform'),
                            (['--dev', '--supplement', 'legal/rust-platform-debian-bookworm.json', '--out', 'x'],
                             'unrecognized arguments: --dev')):
            with self.subTest(args=args):
                result = subprocess.run([sys.executable, '-m', 'scripts.legal.rust', '--package', 'graphite-meter-client',
                                         *args], cwd=ROOT, capture_output=True, text=True, check=False)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn(error, result.stderr)



class RustImageTests(unittest.TestCase):
    def test_the_server_image_adds_what_go_image_adds_to_its_server(self) -> None:
        go = [Provenance.parse(item) for item in array(read_json(ROOT / 'legal/provenance.json'))]
        expected = {(entry.ecosystem, entry.name) for entry in go if 'container' in entry.artifactScopes
                    and not {'server', 'server/browser'} & set(entry.artifactScopes)}
        provenance = manual_sources(ROOT, 'graphite-meter-server')
        browser = add_provenance(ROOT, [], provenance, 'server/browser')
        image = image_additions(ROOT, browser, provenance)
        self.assertEqual({(component.ecosystem, component.name) for component in image}, expected)
        self.assertIn(('container', 'ca-certificates'), expected)
        reviews = [Review.parse(item) for item in array(read_json(ROOT / 'legal/reviewed-components.json'))]
        for component in image:
            validate_review(component, reviews)
        # The offer carries each entry's repository files; the bundle it names is image content.
        for entry in provenance:
            self.assertTrue(all((ROOT / path).exists() for path in manual_files(entry)))
            self.assertFalse(any(path.startswith('/') for path in manual_files(entry)))
        self.assertFalse(any('container' in entry.artifactScopes
                             for entry in manual_sources(ROOT, 'graphite-meter-client')))


class RustPlatformTests(CheckoutTests):
    def test_downloaded_and_cached_runtime_notices_require_the_reviewed_bytes(self) -> None:
        (self.root / 'legal').mkdir()
        relative = 'legal/manual/runtime/LICENSE'
        resource = {'url': 'https://example.invalid/immutable/LICENSE', 'sha256': sha256(b'reviewed license')}
        (self.root / 'legal/rust-notice-sources.json').write_text(json.dumps({relative: resource}))
        entry: dict[str, Json] = {'notices': {relative: 'runtime/LICENSE'}}
        with patch('urllib.request.urlopen', return_value=io.BytesIO(b'changed license')):
            with self.assertRaisesRegex(LegalError, 'bytes differ'):
                fetch_notices(self.root, entry)
        self.assertFalse((self.root / relative).exists())
        with patch('urllib.request.urlopen', return_value=io.BytesIO(b'reviewed license')):
            fetch_notices(self.root, entry)
        self.assertEqual((self.root / relative).read_bytes(), b'reviewed license')
        with patch('urllib.request.urlopen', side_effect=AssertionError('cache must work offline')):
            fetch_notices(self.root, entry)
        (self.root / relative).write_bytes(b'changed cache')
        with self.assertRaisesRegex(LegalError, 'bytes differ'):
            fetch_notices(self.root, entry)

    def test_each_linker_map_names_the_native_inputs_that_contributed_code(self) -> None:
        maps = {
            'GNU ld': 'Archive member included to satisfy reference by file (symbol)\n\n'
                      '/usr/lib/libgcc.a(_ctors.o)\n                              /build/app.o (__CTOR_LIST__)\n'
                      '/rust/lib/rustlib/t/lib/libstd.rlib(std.o)\n                              /build/app.o (main)\n'
                      '/registry/crate/lib/libimport.a(stub.o)\n                              /build/app.o (Stub)\n\n'
                      'Linker script and memory map\n\nLOAD /usr/lib/gcc/../crt1.o\nLOAD /usr/lib/libunused.a\n'
                      'LOAD /usr/lib/libc.so\nLOAD /build/app.o\n/DISCARD/\n',
            'LLD': '             VMA              LMA     Size Align Out     In      Symbol\n'
                   '             2fc              2fc       20     4         /usr/lib/gcc/../crt1.o:(.note.ABI-tag)\n'
                   '            1000             1000       10     1         /usr/lib/libgcc.a(_ctors.o):(.text)\n'
                   '            1010             1010       10     1         /rust/lib/rustlib/t/lib/libstd.rlib(std.o):(.text)\n'
                   '            1020             1020       10     1         /build/app.o:(.text)\n',
            'ld64': '# Path: /build/app\n# Object files:\n[  0] linker synthesized\n[  1] /usr/lib/crt1.o\n'
                    '[  2] /usr/lib/libgcc.a(_ctors.o)\n[  3] /SDK/usr/lib/libSystem.tbd\n'
                    '[  4] /rust/lib/rustlib/t/lib/libstd.rlib(std.o)\n[  5] /build/app.o\n',
        }
        for linker, listing in maps.items():
            path = self.root / 'link.map'
            path.write_text(listing)
            with self.subTest(linker=linker):
                self.assertEqual(linked(path, Path('/rust'), {Path('/build'), Path('/registry/crate')}),
                                 {'/usr/lib/crt1.o', '/usr/lib/libgcc.a', '$RUST_SYSROOT/lib/rustlib/t/lib/libstd.rlib'})

    def test_the_link_map_stays_in_the_notice_directory(self) -> None:
        output = self.root
        self.assertEqual(link_map(output, 'x86_64-unknown-linux-gnu', 'ci'),
                         output / 'x86_64-unknown-linux-gnu-ci.map')
        with self.assertRaises(ControlPlaneError):
            link_map(output, '../escape', 'ci')

    def test_every_macos_install_name_is_an_import_that_needs_review(self) -> None:
        listing = ('/build/graphite-meter-client:\n'
                   '\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1351.0.0)\n'
                   '\t@rpath/libdep.dylib (compatibility version 1.0.0, current version 1.0.0)\n'
                   '\t@executable_path/../Frameworks/Dep Kit.framework/Dep Kit (compatibility version 1.0.0, current version 1.0.0)\n'
                   '\t@loader_path/libweak.dylib (compatibility version 0.0.0, current version 0.0.0, weak)\n')
        relative = {'@rpath/libdep.dylib', '@executable_path/../Frameworks/Dep Kit.framework/Dep Kit',
                    '@loader_path/libweak.dylib'}
        with patch('subprocess.check_output', return_value=listing):
            self.assertEqual(imports(Path('/build/graphite-meter-client'), 'aarch64-apple-darwin'),
                             {'/usr/lib/libSystem.B.dylib'} | relative)

    def test_only_a_reviewed_linker_runs(self) -> None:
        with patch.dict(os.environ, {'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER': './linker'}):
            with self.assertRaisesRegex(LegalError, 'unreviewed linker'):
                linker_version('x86_64-unknown-linux-gnu')
        # A Cargo configuration file can name a linker too.
        for config in ('.cargo/config.toml', 'rust/.cargo/config'):
            (self.root / config).parent.mkdir(parents=True)
            (self.root / config).write_text('[target.x86_64-unknown-linux-gnu]\nlinker = "./linker"\n')
            with self.subTest(config=config), self.assertRaisesRegex(LegalError, 'no Cargo configuration'):
                linker_version('x86_64-unknown-linux-gnu', self.root)
            (self.root / config).unlink()


class RustPlatformRecordTests(CheckoutTests):
    NATIVE = SYSROOT + 'lib/rustlib/t/lib/self-contained/libc.a'
    CRT1 = SYSROOT + 'lib/rustlib/t/lib/self-contained/crt1.o'
    STD = SYSROOT + 'lib/rustlib/t/lib/libstd-1.rlib'
    MIT = SYSROOT + 'share/doc/rust/licenses/MIT.txt'

    def setUp(self) -> None:
        self.enterContext(patch('scripts.legal.rust_platform.linker_version', return_value='cc 1'))
        super().setUp()
        self.sysroot = self.root / 'sysroot'
        zlib, libc = str(self.root / 'zlib/copyright'), str(self.root / 'libc/copyright')
        self.files = {
            SYSROOT + 'share/doc/rust/COPYRIGHT-library.html': '<p>library</p>\n', self.MIT: 'MIT\n',
            SYSROOT + 'share/doc/rust/licenses/Apache-2.0.txt': 'Apache\n',
            self.STD: 'std', SYSROOT + 'lib/rustlib/t/lib/libcore-2.rlib': 'core', self.NATIVE: 'libc',
            zlib: 'zlib notice\n', libc: 'libc notice\n',
        }
        for path, content in self.files.items():
            self.write(path, content)
        # Every file above is an input; its listing and digest are computed here independently.
        self.listing = ''.join(f'{path}\t{sha256(self.files[path].encode())}\n' for path in sorted(self.files))
        self.entry: dict = {
            'target': 't', 'rustc': 'rustc 1', 'nativeCompiler': 'cc 1', 'systemLibraries': ['libc.so.6'],
            'reviewDecision': 'approved', 'reviewNotes': 'reviewed', 'description': 'Platform notices.',
            'nativeInputs': [self.NATIVE],
            # The record's order, not path order, orders its notices.
            'notices': {zlib: 'zlib/copyright', libc: 'libc/copyright'},
            'inputsSha256': sha256(self.listing.encode()),
        }

    def write(self, path: str, content: str) -> None:
        local = self.sysroot / path.removeprefix(SYSROOT) if path.startswith(SYSROOT) else Path(path)
        local.parent.mkdir(parents=True, exist_ok=True)
        local.write_text(content)

    def facts(self, **changes: object) -> dict:
        return {'target': 't', 'compiler': 'rustc 1', 'sysroot': self.sysroot,
                'inputs': {self.NATIVE, self.STD}, 'libraries': {'libc.so.6'}} | changes

    def notice(self, entry: dict | None = None, **changes: object) -> str:
        return notice(self.entry if entry is None else entry, **self.facts(**changes))

    def test_any_changed_added_or_missing_input_is_refused(self) -> None:
        for path, content in self.files.items():
            with self.subTest(changed=path):
                self.write(path, content + ' ')
                with self.assertRaisesRegex(LegalError, 'inputs changed'):
                    self.notice()
                self.write(path, content)
        for path in (SYSROOT + 'lib/rustlib/t/lib/libextra-3.rlib', SYSROOT + 'share/doc/rust/licenses/ISC.txt'):
            with self.subTest(added=path):
                self.write(path, 'added')
                with self.assertRaisesRegex(LegalError, 'inputs changed'):
                    self.notice()
                (self.sysroot / path.removeprefix(SYSROOT)).unlink()
        with self.assertRaisesRegex(LegalError, 'reviewed none'):
            self.notice({key: value for key, value in self.entry.items() if key != 'inputsSha256'})
        (self.sysroot / self.NATIVE.removeprefix(SYSROOT)).unlink()
        with self.assertRaises(FileNotFoundError):
            self.notice()

    def test_a_linked_input_outside_the_rlibs_and_native_inputs_is_refused(self) -> None:
        for linked_input in (self.CRT1, '/elsewhere/libother.rlib'):
            with self.subTest(linked=linked_input), self.assertRaisesRegex(LegalError, 'native inputs lack review'):
                self.notice(inputs={self.STD, linked_input})

    def test_provisional_platform_notice_still_requires_the_final_native_closure(self) -> None:
        notice(self.entry, **self.facts(inputs=set(), libraries=set()))
        unexpected = str(self.root / 'unexpected.o')
        self.write(unexpected, 'unreviewed native code')
        with self.assertRaises(LegalError) as failure:
            checked_platform_notice(self.entry, self.facts(inputs={self.NATIVE, unexpected}))
        self.assertIn('linked native inputs lack review', str(failure.exception))
        self.assertIn('"reviewDecision": "pending"', str(failure.exception))
        self.assertIn(unexpected, str(failure.exception))

    def test_every_review_fact_is_still_required(self) -> None:
        cases = [(None, {}, 'absent or stale'), ({'rustc': 'rustc 2'}, {}, 'absent or stale'),
                 ({'reviewDecision': 'pending'}, {}, 'absent or stale'), ({'reviewNotes': ''}, {}, 'absent or stale'),
                 ({'nativeCompiler': 'cc 2'}, {}, 'native linker toolchain differs'),
                 ({}, {'libraries': {'libc.so.6', 'libm.so.6'}}, 'system libraries lack review'),
                 ({'notices': {self.MIT: 'MIT'}}, {}, 'beyond the Rust standard library'),
                 ({'notices': {str(self.root / 'zlib/copyright'): ''}}, {}, 'each with a name')]
        for change, facts, message in cases:
            with self.subTest(change=change, facts=facts), self.assertRaisesRegex(LegalError, message):
                entry: dict | None = None if change is None else self.entry | change
                notice(entry, **self.facts(**facts))

    def test_the_candidate_is_the_complete_record_of_this_build_and_its_listing(self) -> None:
        record, listing = candidate(self.entry, **self.facts())
        self.assertEqual(listing, self.listing)
        # Byte equality also compares the order of the fields and of the notices.
        self.assertEqual(marshal(record), marshal(self.entry | {'reviewDecision': 'pending', 'reviewNotes': ''}))
        # A build that does not link a reviewed native input keeps it while it exists, so the same record returns.
        gone = SYSROOT + 'lib/rustlib/t/lib/self-contained/gone.o'
        record, listing = candidate(self.entry | {'nativeInputs': [self.NATIVE, gone]}, **self.facts(inputs={self.STD}))
        self.assertEqual(listing, self.listing)
        self.assertEqual(marshal(record), marshal(self.entry | {'reviewDecision': 'pending', 'reviewNotes': ''}))
        # A build that links a new native input: the candidate lists it, and approving it passes.
        self.write(self.CRT1, 'crt1')
        record, listing = candidate(None, **self.facts(inputs={self.STD, self.NATIVE, self.CRT1}))
        self.assertEqual((record['nativeInputs'], record['notices']), ([self.CRT1, self.NATIVE], {}))
        self.assertIn(f"{self.CRT1}\t{sha256(b'crt1')}\n", listing)
        self.assertEqual(record['inputsSha256'], sha256(listing.encode()))
        approved = record | {'reviewDecision': 'approved', 'reviewNotes': 'reviewed'}
        self.assertIn('--- rust-standard-library/MIT.txt ---', self.notice(approved, inputs={self.STD, self.CRT1}))


class RustBuildTests(CheckoutTests):
    def test_git_sources_are_remapped_and_the_vendored_workspace_builds_without_its_checkout(self) -> None:
        from scripts.ci.toolchains import rust_channel

        upstream, repo = self.root / 'upstream', self.root / 'repo'
        files = {
            'Cargo.toml': '[workspace]\nmembers = ["dependency"]\nresolver = "2"\n'
                          '[workspace.package]\nversion = "1.0.0"\nedition = "2021"\n',
            'dependency/Cargo.toml': '[package]\nname = "source-fixture"\nversion.workspace = true\nedition.workspace = true\n',
            # A bounds check embeds the source location under Cargo's home.
            'dependency/src/lib.rs': 'pub fn pick(bytes: &[u8], index: usize) -> u8 { bytes[index] }\n',
            'LICENSE': 'fixture license\n',
        }
        for name, content in files.items():
            write_changed(upstream / name, content.encode())
        (upstream / 'dependency/LICENSE').symlink_to('../LICENSE')
        for args in (('init', '-q'), ('add', '.'), ('commit', '-qm', 'fixture')):
            subprocess.run(['git', '-C', str(upstream), '-c', 'user.name=fixture',
                            '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false',
                            *args], check=True)
        revision = subprocess.check_output(['git', '-C', str(upstream), 'rev-parse', 'HEAD'], text=True).strip()
        write_changed(repo / 'rust/rust-toolchain.toml', (ROOT / 'rust/rust-toolchain.toml').read_bytes())
        write_changed(repo / 'rust/Cargo.toml', (
            '[package]\nname = "app"\nversion = "1.0.0"\nedition = "2021"\n'
            f'[dependencies]\nsource-fixture = {{ git = "{upstream.as_uri()}", rev = "{revision}" }}\n').encode())
        write_changed(repo / 'rust/src/main.rs',
                      b'fn main() { println!("{}", source_fixture::pick(&[1, 2], std::env::args().count())); }\n')
        host = subprocess.check_output(['rustc', f'+{rust_channel(ROOT)}', '-vV'], text=True)
        target = next(line.split()[1] for line in host.splitlines() if line.startswith('host:'))
        with patch.dict(os.environ, {'CARGO_HOME': str(self.root / 'cargo-home'), 'CARGO_TERM_QUIET': 'true'}):
            subprocess.run(cargo(repo, 'generate-lockfile'), cwd=repo / 'rust', check=True)
            _, messages = capture(repo, 'app', target, 'release', self.root / 'app.map')
            binary = Path(next(message['executable'] for message in messages if message.get('executable'))).read_bytes()
            self.assertTrue(b'/cargo/git/checkouts/' in binary, 'the dependency path is not remapped')
            self.assertFalse(str(self.root).encode() in binary, 'the binary names a build path')
            payload = io.BytesIO()
            with tarfile.open(fileobj=payload, mode='w') as archive:
                add_cargo_sources(archive, repo, [Component('source-fixture', '1.0.0', 'cargo', upstream.as_uri(), '')])
            shutil.rmtree(upstream)
            shutil.rmtree(self.root / 'cargo-home')
            payload.seek(0)
            with tarfile.open(fileobj=payload) as archive:
                archive.extractall(self.root / 'unpacked', filter='data')
            package = self.root / 'unpacked/third_party/cargo/source-fixture-1.0.0'
            self.assertEqual((package / 'LICENSE').read_text(), 'fixture license\n')
            self.assertFalse((package / 'LICENSE').is_symlink())
            subprocess.run(cargo(repo, 'build', '--offline', '--manifest-path', str(package / 'Cargo.toml')),
                           check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

    def test_only_development_notices_leave_their_marker_in_the_executable(self) -> None:
        from scripts.ci.toolchains import rust_channel

        # The real build script and release profile, and a compressor that keeps the notices unreadable.
        files = {
            'rust/Cargo.toml': '[workspace]\nmembers = ["app"]\nresolver = "2"\n'
                               '[profile.release]\nlto = "fat"\ncodegen-units = 1\nstrip = "symbols"\n',
            'rust/app/Cargo.toml': '[package]\nname = "app"\nversion = "1.0.0"\nedition = "2021"\n'
                                   '[build-dependencies]\nminiz_oxide = { path = "../miniz" }\n',
            'rust/app/build.rs': '#[path = "../legal_build.rs"]\nmod legal;\nfn main() { legal::embed(false).unwrap() }\n',
            'rust/app/src/main.rs': 'include!(concat!(env!("OUT_DIR"), "/legal.rs"));\n'
                                    'fn main() { std::hint::black_box((LEGAL, DEVELOPMENT_NOTICES)); }\n',
            'rust/miniz/Cargo.toml': '[package]\nname = "miniz_oxide"\nversion = "0.9.1"\nedition = "2021"\n',
            'rust/miniz/src/lib.rs': 'pub mod deflate {\n    pub fn compress_to_vec_zlib(data: &[u8], _: u8) -> Vec<u8> '
                                     '{ data.iter().map(|byte| !byte).collect() }\n}\n',
            'rust/legal_build.rs': (ROOT / 'rust/legal_build.rs').read_text(),
            'rust/rust-toolchain.toml': (ROOT / 'rust/rust-toolchain.toml').read_text(),
        }
        for name, content in files.items():
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            (self.root / name).write_text(content)
        channel = rust_channel(ROOT)
        rustc = subprocess.check_output(['rustup', 'which', '--toolchain', channel, 'rustc'], text=True).strip()
        host = next(line.split()[1] for line in subprocess.check_output([rustc, '-vV'], text=True).splitlines()
                    if line.startswith('host:'))
        with patch.dict(os.environ, {'CARGO_TERM_QUIET': 'true'}):
            subprocess.run(cargo(self.root, 'generate-lockfile', '--offline'), cwd=self.root / 'rust', check=True)
            # A development build is Cargo's host build; a release build names its --target.
            for target, notices in ((None, DEVELOPMENT_NOTICE + 'notices\n'), (host, 'Reviewed notices\n')):
                # The notices must match the identity of the build that embeds them, which a first build writes.
                _, messages = capture(self.root, 'app', target, 'release', None)
                output = Path(next(item['out_dir'] for item in messages if item.get('out_dir')))
                legal = self.root / 'rust/target/notices' / (target or 'host')
                (legal / 'inputs/rust').mkdir(parents=True)
                for name, content in (('LEGAL.txt', notices), ('package.txt', 'app'), ('target.txt', host),
                                      ('rustc-path.txt', rustc), ('inputs.txt', 'rust/Cargo.lock\n'),
                                      ('inputs/rust/Cargo.lock', (self.root / 'rust/Cargo.lock').read_text()),
                                      ('build-identity.txt', (output / 'legal-build-identity.txt').read_text())):
                    (legal / name).write_text(content)
                _, messages = capture(self.root, 'app', target, 'release', None, legal)
                executable = Path(next(item['executable'] for item in messages if item.get('executable')))
                self.assertEqual(DEVELOPMENT.encode() in executable.read_bytes(), target is None, target)


if __name__ == '__main__':
    unittest.main()
