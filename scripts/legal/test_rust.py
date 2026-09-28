from __future__ import annotations

import io
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from copy import deepcopy

from scripts.ci.github_api import ControlPlaneError
from scripts.legal.artifacts import render
from scripts.legal.model import Component, LegalError, Project, marshal, sha256
from scripts.legal.rust import about, add_cargo_sources, artifacts, cargo, legal_report
from scripts.legal.rust_platform import SYSROOT, candidate, link_map, linked, linker_version, notice

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
        # A build that links a new native input: the candidate lists it, and approving it passes.
        self.write(self.CRT1, 'crt1')
        with patch('scripts.legal.rust_platform.linker_version', return_value='cc 1'):
            record, listing = candidate(None, **self.facts(inputs={self.STD, self.NATIVE, self.CRT1}))
        self.assertEqual((record['nativeInputs'], record['notices']), ([self.CRT1, self.NATIVE], {}))
        self.assertIn(f"{self.CRT1}\t{sha256(b'crt1')}\n", listing)
        self.assertEqual(record['inputsSha256'], sha256(listing.encode()))
        approved = record | {'reviewDecision': 'approved', 'reviewNotes': 'reviewed'}
        self.assertIn('--- rust-standard-library/MIT.txt ---', self.notice(approved, inputs={self.STD, self.CRT1}))


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
