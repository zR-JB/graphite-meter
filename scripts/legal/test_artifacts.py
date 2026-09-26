from __future__ import annotations

import os
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts.legal.artifacts import manual_source_destination, safe_name, third_party_source_bundle
from scripts.legal.model import (
    Component, LegalError, LegalFile, Project, Provenance, local_path,
)


class SourceArchiveTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.scopes: dict[str, list[Component]] = {'server/browser': [], 'tui': [], 'container': []}

    def write(self, name: str, data: str) -> Path:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(data)
        return path

    def contents(self, path: Path) -> dict[str, bytes]:
        result: dict[str, bytes] = {}
        with tarfile.open(path, 'r:gz') as archive:
            for member in archive:
                data = archive.extractfile(member)
                assert data is not None
                result[member.name] = data.read()
                self.assertEqual((member.uid, member.gid, member.mode, member.mtime), (0, 0, 0o644, 0))
        return result

    def test_deterministic_split_source_offer_excludes_unlisted_project_and_local_files(self) -> None:
        for name in ('LICENSE', '.dev-certs/private.pem', 'go/cover.out'):
            self.write(name, 'excluded material')
        self.write('manual.txt', 'manual source')
        provenance = [Provenance(name='sample', localPaths=['manual.txt'], correspondingSource='third_party/manual/sample')]
        project = Project(name='Graphite Meter', repository='https://example.invalid/repo')
        first, second = self.root / 'first.tar.gz', self.root / 'second.tar.gz'
        for path in (first, second):
            third_party_source_bundle(self.root, project, 'development', self.scopes, provenance, path)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        files = self.contents(first)
        prefix = 'graphite-meter_development_third-party-source/'
        self.assertEqual(set(files), {prefix + name for name in ('third_party/manual/sample/manual.txt', 'LEGAL_INVENTORY.json', 'PROVENANCE.json', 'README.txt')})
        for text in ('Source code (tar.gz)', 'Source code (zip)', project.repository):
            self.assertIn(text.encode(), files[prefix + 'README.txt'])

    def test_archive_uses_resolved_browser_and_go_replacement_paths(self) -> None:
        npm = self.write('client/node_modules/outer/node_modules/svelte/LICENSE.md', 'MIT\n').parent
        go = self.write('replacement/replacement.go', 'package replacement\n').parent
        self.scopes['server/browser'] = [Component('svelte', '5', 'npm', source_path=npm), Component('../replacement', ecosystem='go', source_path=go)]
        self.scopes['container'] = list(self.scopes['server/browser'])
        output = self.root / 'sources.tar.gz'
        third_party_source_bundle(self.root, Project(), 'development', self.scopes, [], output)
        files = self.contents(output)
        prefix = 'graphite-meter_development_third-party-source/third_party/'
        self.assertEqual(files[prefix + 'npm/svelte_at_5/LICENSE.md'], b'MIT\n')
        self.assertEqual(files[prefix + 'go/.._replacement_at_/replacement.go'], b'package replacement\n')
        self.assertEqual(sum(name.endswith('/replacement.go') for name in files), 1)
        self.assertNotIn(str(self.root).encode(), files['graphite-meter_development_third-party-source/LEGAL_INVENTORY.json'])

    def test_links_may_not_leave_the_component_or_repository(self) -> None:
        package = self.write('client/node_modules/pkg/index.js', 'code\n').parent
        (package / 'README').symlink_to(package / 'index.js')
        self.scopes['server/browser'] = [Component('pkg', '1', 'npm', source_path=package)]
        output = self.root / 'sources.tar.gz'
        third_party_source_bundle(self.root, Project(), 'development', self.scopes, [], output)
        self.assertEqual(self.contents(output)[
            'graphite-meter_development_third-party-source/third_party/npm/pkg_at_1/README'], b'code\n')
        outside = tempfile.NamedTemporaryFile()
        self.addCleanup(outside.close)
        (package / 'secret').symlink_to(outside.name)
        with self.assertRaisesRegex(LegalError, 'links outside'):
            third_party_source_bundle(self.root, Project(), 'development', self.scopes, [], output)
        self.scopes['server/browser'] = []
        self.root.joinpath('manual.txt').symlink_to(outside.name)
        entry = Provenance(name='m', localPaths=['manual.txt'], correspondingSource='third_party/manual/m')
        with self.assertRaisesRegex(LegalError, 'links outside'):
            third_party_source_bundle(self.root, Project(), 'development', self.scopes, [entry], output)

    def test_command_line_paths_stay_in_the_checkout_or_a_temporary_directory(self) -> None:
        repo = self.root / 'repo'
        (repo / 'go').mkdir(parents=True)
        (repo / 'escape').symlink_to('/')
        with patch.dict(os.environ, {'RUNNER_TEMP': str(repo / 'go')}):
            self.assertEqual(local_path(repo / 'go/dist/x.tar.gz', repo),
                             repo.resolve() / 'go/dist/x.tar.gz')
            scan = self.root / 'scan.json'
            self.assertEqual(local_path(scan, repo), self.root.resolve() / 'scan.json')
            for outside in ('/etc/passwd', repo / 'go/../../../..' / 'etc', repo / 'escape/etc'):
                with self.subTest(outside=outside):
                    self.assertRaisesRegex(LegalError, 'is outside', local_path, outside, repo)

    def test_manual_archive_legal_files_and_relative_sources_are_required(self) -> None:
        self.write('manual/LICENSE', 'MIT License\n')
        entry = Provenance(name='font', localLegalFiles=[LegalFile('manual/LICENSE')], correspondingSource='third_party/manual/font')
        output = self.root / 'sources.tar.gz'
        third_party_source_bundle(self.root, Project(), 'development', self.scopes, [entry], output)
        self.assertIn('graphite-meter_development_third-party-source/third_party/manual/font/LICENSE', self.contents(output))
        entry.localPaths = ['missing-source']
        with self.assertRaisesRegex(LegalError, 'missing-source.*font'):
            third_party_source_bundle(self.root, Project(), 'development', self.scopes, [entry], output)
        entry.localPaths = [str(self.root / 'container-only-source')]
        third_party_source_bundle(self.root, Project(), 'development', self.scopes, [entry], output)
        for path in ('../escape', 'third_party/go/not-manual', '/absolute', r'third_party\manual\windows'):
            entry.correspondingSource = path
            with self.assertRaisesRegex(LegalError, 'invalid corresponding source'):
                manual_source_destination(entry)
        self.assertEqual(safe_name('github.com/example/pkg@v1'), 'github.com_example_pkg_at_v1')


if __name__ == '__main__':
    unittest.main()
