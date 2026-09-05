from __future__ import annotations

import tarfile
import tempfile
import unittest
from pathlib import Path

from scripts.legal.artifacts import manual_source_destination, safe_name, third_party_source_bundle
from scripts.legal.model import Component, LegalError, LegalFile, Project, Provenance


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
