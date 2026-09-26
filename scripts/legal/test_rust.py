from __future__ import annotations

import io
import os
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from copy import deepcopy

from scripts.legal.model import Component, LegalError
from scripts.legal.rust import add_cargo_sources, artifacts, cargo


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
