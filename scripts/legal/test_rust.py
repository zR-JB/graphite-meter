from __future__ import annotations

import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch
from copy import deepcopy

from scripts.ci.github_api import ControlPlaneError
from scripts.legal.artifacts import render
from scripts.legal.model import (Component, LegalError, Project, Provenance, Review, array, manual_files,
                                 manual_sources, marshal, read_json, sha256)
from scripts.legal.review import add_provenance, validate_review
from scripts.legal.rust import (DEVELOPMENT_NOTICE, about, add_cargo_sources, artifacts, capture, cargo,
                                image_additions, legal_report)
from scripts.legal.rust_platform import SYSROOT, candidate, imports, link_map, linked, linker_version, notice

ROOT = Path(__file__).resolve().parents[2]


class RustArtifactTests(unittest.TestCase):
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


class RustPackageTests(unittest.TestCase):
    def test_packaging_takes_listed_platforms_and_writes_only_inside_its_roots(self) -> None:
        from scripts.package_rust import build

        supplement = ROOT / 'legal/rust-platform-macos.json'
        with patch('subprocess.run', side_effect=AssertionError('built')), tempfile.TemporaryDirectory() as scratch:
            with self.assertRaisesRegex(ValueError, 'no TUI target for plan9/amd64'):
                build('1.2.3', 'plan9/amd64', Path(scratch), supplement)
            with self.assertRaisesRegex(ControlPlaneError, 'is outside'):
                build('1.2.3', 'linux/amd64', Path('/'), supplement)
            with self.assertRaisesRegex(ValueError, 'invalid release version'):
                build('1.2.3;id', 'linux/amd64', Path(scratch), supplement)

    @unittest.skipUnless(os.name == 'posix', 'the fake executable is a shell script')
    def test_a_build_for_this_system_and_architecture_reports_its_version(self) -> None:
        import scripts.package_rust as package
        from scripts.ci.toolchains import host_platform, tui_targets

        platforms = tui_targets(ROOT / 'scripts/tui-targets.txt')
        if host_platform() not in platforms:
            self.skipTest(f'no TUI ships for {host_platform()}')
        # The builder links musl or MinGW, whose executables run wherever their system and machine match.
        other = next(platform for platform in platforms if platform.split('/')[0] != host_platform().split('/')[0])
        run = subprocess.run
        with tempfile.TemporaryDirectory() as scratch:
            repo = Path(scratch).resolve()
            for name in ('LICENSE', 'COPYRIGHT', 'scripts/tui-targets.txt'):
                (repo / name).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / name, repo / name)
            ran, reported = repo / 'ran', {'version': '1.2.3-rust'}

            def legal_build(command: list[str], **options: Any) -> object:
                if command[0] != sys.executable:
                    return run(command, **options)
                out, target = (Path(command[command.index(flag) + 1]) for flag in ('--out', '--target'))
                out.mkdir(parents=True)
                (out / 'LEGAL.txt').write_text('notices\n')
                (out / 'THIRD_PARTY_SOURCE.tar.gz').write_bytes(b'source')
                binary = repo / 'rust/target' / target.name / 'release' / (
                    'graphite-meter-client.exe' if '-windows-' in target.name else 'graphite-meter-client')
                binary.parent.mkdir(parents=True, exist_ok=True)
                binary.write_text(f"#!/bin/sh\ntouch '{ran}'\necho graphite-meter-client {reported['version']}\n")
                binary.chmod(0o755)
                return None

            with patch.object(package, 'REPO', repo), patch('subprocess.run', side_effect=legal_build):
                package.build('1.2.3', other, repo / 'dist', ROOT / 'legal/rust-platform-macos.json')
                self.assertFalse(ran.exists())
                package.build('1.2.3', host_platform(), repo / 'dist', ROOT / 'legal/rust-platform-macos.json')
                self.assertTrue(ran.exists())
                reported['version'] = '1.2.2-rust'
                with self.assertRaisesRegex(ValueError, "reports 'graphite-meter-client 1.2.2-rust'"):
                    package.build('1.2.3', host_platform(), repo / 'dist', ROOT / 'legal/rust-platform-macos.json')


class RustLegalReportTests(unittest.TestCase):
    def test_the_report_opens_as_go_tui_report_of_the_same_version_and_keeps_its_notices(self) -> None:
        project = Project.read(ROOT)
        sections = 'THIRD-PARTY SOFTWARE NOTICES\n\nnotices shared with the browser\n'
        for version, tag in (('1.2.3', '/tree/v1.2.3'), ('v1.2.3-rc.1', '/tree/v1.2.3-rc.1'), ('development', '')):
            with self.subTest(version=version):
                go = render(ROOT, project, version, {'server/browser': [], 'tui': [], 'container': []})
                tui = go['go/internal/legal/assets/TUI_LEGAL.txt']
                header = tui[:tui.index(b'THIRD-PARTY SOFTWARE NOTICES')]
                self.assertIn(f'\nSource code: {project.repository}{tag}\n'.encode(), header)
                report = legal_report(ROOT, version, sections)
                self.assertTrue(report.startswith(header))
                self.assertTrue(report.endswith(b'(including build-time dependencies)\n\n' + sections.encode()))
                self.assertEqual(about(project, version, 'engine', [])['sourceURL'], project.repository + tag)
                self.assertEqual(json.loads(go['client/public/legal/about.json'])['sourceURL'], project.repository + tag)

    def test_a_development_report_opens_by_saying_that_no_review_covers_it(self) -> None:
        sections = 'THIRD-PARTY SOFTWARE NOTICES\n'
        report = legal_report(ROOT, 'development', sections, development=True)
        self.assertTrue(report.startswith(b'UNREVIEWED DEVELOPMENT BUILD\n\n'))
        self.assertEqual(report, DEVELOPMENT_NOTICE.encode() + legal_report(ROOT, 'development', sections))


class RustDevelopmentTests(unittest.TestCase):
    def test_only_a_development_build_goes_without_a_target_and_a_platform_record(self) -> None:
        for args, error in ((['--out', 'x'], '--target is required'),
                            (['--development', '--supplement', 'legal/rust-platform-macos.json', '--out', 'x'],
                             '--development reviews no platform'),
                            (['--development', '--review-template', '--out', 'x'], '--development reviews no platform'),
                            (['--dev', '--supplement', 'legal/rust-platform-macos.json', '--out', 'x'],
                             'unrecognized arguments: --dev')):
            with self.subTest(args=args):
                result = subprocess.run([sys.executable, '-m', 'scripts.legal.rust', '--package', 'graphite-meter-client',
                                         *args], cwd=ROOT, capture_output=True, text=True, check=False)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn(error, result.stderr)

    def test_a_development_capture_is_cargo_plain_host_build_without_a_link_map(self) -> None:
        link_map = Path('/notices/x86_64-unknown-linux-musl-release.map')
        commands = []
        for target, mapped in ((None, None), ('x86_64-unknown-linux-musl', link_map)):
            with patch('subprocess.run', return_value=subprocess.CompletedProcess([], 0, '')) as run, \
                    patch('subprocess.check_output', return_value='{}'):
                capture(ROOT, 'graphite-meter-client', target, 'release', mapped)
            commands.append(run.call_args.args[0][2:])
        head = ['rustc', '--locked', '--package', 'graphite-meter-client', '--bin', 'graphite-meter-client']
        tail = ['--profile', 'release', '--message-format=json']
        self.assertEqual(commands, [head + tail, head + ['--target', 'x86_64-unknown-linux-musl'] + tail
                                    + ['--', f'-Clink-arg=-Wl,-Map={link_map}']])


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


class RustPlatformTests(unittest.TestCase):
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
        with tempfile.TemporaryDirectory() as scratch:
            for linker, listing in maps.items():
                path = Path(scratch) / 'link.map'
                path.write_text(listing)
                with self.subTest(linker=linker):
                    self.assertEqual(linked(path, Path('/rust'), {Path('/build'), Path('/registry/crate')}),
                                     {'/usr/lib/crt1.o', '/usr/lib/libgcc.a', '$RUST_SYSROOT/lib/rustlib/t/lib/libstd.rlib'})

    def test_the_link_map_stays_in_the_notice_directory(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            output = Path(scratch).resolve()
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


class RustPlatformRecordTests(unittest.TestCase):
    NATIVE = SYSROOT + 'lib/rustlib/t/lib/self-contained/libc.a'
    CRT1 = SYSROOT + 'lib/rustlib/t/lib/self-contained/crt1.o'
    STD = SYSROOT + 'lib/rustlib/t/lib/libstd-1.rlib'
    MIT = SYSROOT + 'share/doc/rust/licenses/MIT.txt'

    def setUp(self) -> None:
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name).resolve()
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
        with patch('scripts.legal.rust_platform.linker_version', return_value='cc 1'):
            return notice(self.entry if entry is None else entry, **self.facts(**changes))

    def test_notices_are_the_standard_library_texts_then_the_records_in_its_order(self) -> None:
        self.assertEqual(self.notice(), 'Platform notices.\n'
                         '\n--- rust-standard-library/COPYRIGHT-library.html ---\n\n<p>library</p>\n'
                         '\n--- rust-standard-library/Apache-2.0.txt ---\n\nApache\n'
                         '\n--- rust-standard-library/MIT.txt ---\n\nMIT\n'
                         '\n--- zlib/copyright ---\n\nzlib notice\n'
                         '\n--- libc/copyright ---\n\nlibc notice\n')

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
                with patch('scripts.legal.rust_platform.linker_version', return_value='cc 1'):
                    notice(entry, **self.facts(**facts))

    def test_the_candidate_is_the_complete_record_of_this_build_and_its_listing(self) -> None:
        with patch('scripts.legal.rust_platform.linker_version', return_value='cc 1'):
            record, listing = candidate(self.entry, **self.facts())
        self.assertEqual(listing, self.listing)
        # Byte equality also compares the order of the fields and of the notices.
        self.assertEqual(marshal(record), marshal(self.entry | {'reviewDecision': 'pending', 'reviewNotes': ''}))
        # A build that does not link a reviewed native input keeps it while it exists, so the same record returns.
        gone = SYSROOT + 'lib/rustlib/t/lib/self-contained/gone.o'
        with patch('scripts.legal.rust_platform.linker_version', return_value='cc 1'):
            record, listing = candidate(self.entry | {'nativeInputs': [self.NATIVE, gone]}, **self.facts(inputs={self.STD}))
        self.assertEqual(listing, self.listing)
        self.assertEqual(marshal(record), marshal(self.entry | {'reviewDecision': 'pending', 'reviewNotes': ''}))
        # A build that links a new native input: the candidate lists it, and approving it passes.
        self.write(self.CRT1, 'crt1')
        with patch('scripts.legal.rust_platform.linker_version', return_value='cc 1'):
            record, listing = candidate(None, **self.facts(inputs={self.STD, self.NATIVE, self.CRT1}))
        self.assertEqual((record['nativeInputs'], record['notices']), ([self.CRT1, self.NATIVE], {}))
        self.assertIn(f"{self.CRT1}\t{sha256(b'crt1')}\n", listing)
        self.assertEqual(record['inputsSha256'], sha256(listing.encode()))
        approved = record | {'reviewDecision': 'approved', 'reviewNotes': 'reviewed'}
        self.assertIn('--- rust-standard-library/MIT.txt ---', self.notice(approved, inputs={self.STD, self.CRT1}))


class RustBuildTests(unittest.TestCase):
    def test_a_notice_build_names_no_build_machine_path(self) -> None:
        from scripts.ci.toolchains import rust_channel
        from scripts.legal.rust import capture

        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch).resolve()
            upstream, repo = root / 'upstream', root / 'repo'
            (upstream / 'src').mkdir(parents=True)
            (upstream / 'Cargo.toml').write_text('[package]\nname = "fixture"\nversion = "1.0.0"\nedition = "2021"\n')
            # A bounds check embeds its source location, the dependency's path under Cargo's home.
            (upstream / 'src/lib.rs').write_text('pub fn pick(bytes: &[u8], index: usize) -> u8 { bytes[index] }\n')
            for args in (('init', '-q'), ('add', '.'), ('commit', '-qm', 'fixture')):
                subprocess.run(['git', '-C', str(upstream), '-c', 'user.name=fixture',
                                '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false',
                                *args], check=True)
            revision = subprocess.check_output(['git', '-C', str(upstream), 'rev-parse', 'HEAD'], text=True).strip()
            (repo / 'rust/src').mkdir(parents=True)
            shutil.copyfile(ROOT / 'rust/rust-toolchain.toml', repo / 'rust/rust-toolchain.toml')
            (repo / 'rust/Cargo.toml').write_text(
                '[package]\nname = "app"\nversion = "1.0.0"\nedition = "2021"\n'
                f'[dependencies]\nfixture = {{ git = "{upstream.as_uri()}", rev = "{revision}" }}\n')
            (repo / 'rust/src/main.rs').write_text(
                'fn main() { println!("{}", fixture::pick(&[1, 2], std::env::args().count())); }\n')
            host = subprocess.check_output(['rustc', f'+{rust_channel(ROOT)}', '-vV'], text=True)
            target = next(line.split()[1] for line in host.splitlines() if line.startswith('host:'))
            with patch.dict(os.environ, {'CARGO_HOME': str(root / 'cargo-home'), 'CARGO_TERM_QUIET': 'true'}):
                subprocess.run(cargo(repo, 'generate-lockfile'), cwd=repo / 'rust', check=True)
                _, messages = capture(repo, 'app', target, 'release', root / 'app.map')
            binary = Path(next(message['executable'] for message in messages if message.get('executable'))).read_bytes()
            self.assertTrue(b'/cargo/git/checkouts/' in binary, 'the dependency path is not remapped')
            self.assertFalse(str(root).encode() in binary, 'the binary names a build path')


    def test_only_a_target_build_remaps_paths(self) -> None:
        from scripts.legal import rust

        class Built(Exception):
            pass

        def run(command, **kwargs):
            raise Built(kwargs['env'].get('CARGO_ENCODED_RUSTFLAGS'))

        with patch.object(rust.subprocess, 'run', run), patch.dict(os.environ, {'CARGO_ENCODED_RUSTFLAGS': ''}):
            for target, remapped in ((None, False), ('x86_64-unknown-linux-musl', True)):
                with self.assertRaises(Built) as built:
                    rust.capture(ROOT, 'graphite-meter-client', target, 'dev', None)
                # The development tasks build again with plain cargo, whose flags the build identity compares.
                self.assertEqual('--remap-path-prefix' in str(built.exception), remapped, target)


class RustSourceTests(unittest.TestCase):
    def test_git_workspace_source_builds_without_its_checkout(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            upstream = root / 'upstream'
            (upstream / 'dependency/src').mkdir(parents=True)
            (upstream / 'Cargo.toml').write_text(
                '[workspace]\nmembers = ["dependency"]\nresolver = "2"\n'
                '[workspace.package]\nversion = "1.0.0"\nedition = "2021"\n')
            (upstream / 'dependency/Cargo.toml').write_text(
                '[package]\nname = "source-fixture"\nversion.workspace = true\nedition.workspace = true\n')
            (upstream / 'dependency/src/lib.rs').write_text('pub fn value() -> u32 { 42 }\n')
            (upstream / 'LICENSE').write_text('fixture license\n')
            (upstream / 'dependency/LICENSE').symlink_to('../LICENSE')
            for args in (('init', '-q'), ('add', '.'), ('commit', '-qm', 'fixture')):
                subprocess.run(['git', '-C', str(upstream), '-c', 'user.name=fixture',
                                '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false',
                                *args], check=True)
            revision = subprocess.check_output(['git', '-C', str(upstream), 'rev-parse', 'HEAD'], text=True).strip()
            repo = root / 'repo'
            (repo / 'rust/src').mkdir(parents=True)
            shutil.copyfile(Path(__file__).resolve().parents[2] / 'rust/rust-toolchain.toml',
                            repo / 'rust/rust-toolchain.toml')
            (repo / 'rust/Cargo.toml').write_text(
                '[package]\nname = "consumer"\nversion = "1.0.0"\nedition = "2021"\n'
                f'[dependencies]\nsource-fixture = {{ git = "{upstream.as_uri()}", rev = "{revision}" }}\n')
            (repo / 'rust/src/lib.rs').write_text('pub use source_fixture::value;\n')
            with patch.dict(os.environ, {'CARGO_HOME': str(root / 'cargo-home')}):
                subprocess.run(cargo(repo, 'generate-lockfile'), cwd=repo / 'rust', check=True,
                               stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                payload = io.BytesIO()
                with tarfile.open(fileobj=payload, mode='w') as archive:
                    add_cargo_sources(archive, repo, [Component('source-fixture', '1.0.0', 'cargo',
                                                               upstream.as_uri(), '')])
                shutil.rmtree(upstream)
                shutil.rmtree(root / 'cargo-home')
                payload.seek(0)
                with tarfile.open(fileobj=payload) as archive:
                    archive.extractall(root / 'unpacked', filter='data')
                package = root / 'unpacked/third_party/cargo/source-fixture-1.0.0'
                self.assertEqual((package / 'LICENSE').read_text(), 'fixture license\n')
                self.assertFalse((package / 'LICENSE').is_symlink())
                subprocess.run(cargo(repo, 'build', '--offline', '--manifest-path', str(package / 'Cargo.toml')),
                               check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


if __name__ == '__main__':
    unittest.main()
