from __future__ import annotations

import hashlib
import subprocess
import tempfile
import unittest
from pathlib import Path

from scripts.legal.check_git_sources import REGISTRY, check_fork, check_lock

REV = 'a' * 40


def run(directory: Path, *args: str) -> str:
    return subprocess.run(['git', '-C', str(directory), '-c', 'user.name=t', '-c', 'user.email=t@example.invalid',
                           *args], check=True, text=True, stdout=subprocess.PIPE).stdout.strip()


def commit(directory: Path, path: str, subject: str) -> str:
    (directory / path).parent.mkdir(parents=True, exist_ok=True)
    (directory / path).write_text(subject)
    run(directory, 'add', path)
    run(directory, 'commit', '-q', '-m', subject)
    return run(directory, 'rev-parse', 'HEAD')


class LockTests(unittest.TestCase):
    forks = [{'fork': 'https://example.invalid/fork', 'rev': REV}]

    def lock(self, source: str) -> dict:
        return {'package': [{'name': 'local'}, {'name': 'registry', 'source': REGISTRY},
                            {'name': 'forked', 'source': source}]}

    def test_only_allowlisted_full_sha_revisions_pass(self) -> None:
        self.assertEqual(check_lock(self.forks, self.lock(f'git+https://example.invalid/fork?rev={REV}#{REV}')), [])
        for source in (f'git+https://example.invalid/fork?branch=main#{REV}',
                       f'git+https://example.invalid/fork?rev={REV[:12]}#{REV}',
                       f'git+https://example.invalid/fork?rev={REV}&rev={REV}#{REV}',
                       f'git+https://example.invalid/other?rev={REV}#{REV}',
                       'sparse+https://mirror.example.invalid/index/'):
            with self.subTest(source=source):
                self.assertTrue(check_lock(self.forks, self.lock(source)))

    def test_unused_allowlist_entry_is_reported(self) -> None:
        self.assertEqual(check_lock(self.forks, {'package': []}),
                         [f'unused legal/rust-forks.json entry: https://example.invalid/fork@{REV}'])


class ForkTests(unittest.TestCase):
    def setUp(self) -> None:
        self.scratch = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.upstream = self.scratch / 'upstream'
        self.upstream.mkdir()
        run(self.upstream, 'init', '-q')
        self.base = commit(self.upstream, 'pkg/lib.rs', 'upstream')
        run(self.upstream, 'tag', 'v1')
        self.fork = self.scratch / 'fork'
        run(self.scratch, 'clone', '-q', str(self.upstream), str(self.fork))
        run(self.fork, 'checkout', '-q', '-b', 'gm/v1')
        self.rev = commit(self.fork, 'pkg/lib.rs', 'fix pkg')

    def record(self) -> dict:
        changes = run(self.fork, 'diff-tree', '-r', '--no-renames', '--full-index', self.base, self.rev) + '\n'
        return {'fork': str(self.fork), 'branch': 'gm/v1', 'rev': self.rev, 'upstream': str(self.upstream),
                'baseTag': 'v1', 'base': self.base, 'diffSha256': hashlib.sha256(changes.encode()).hexdigest(),
                'modifiedPackages': ['pkg'], 'commits': [{'subject': 'fix pkg'}]}

    def test_reviewed_fork_passes(self) -> None:
        self.assertEqual(check_fork(self.record(), self.scratch / 'check'), [])

    def test_unreviewed_change_outside_packages_fails(self) -> None:
        record = self.record()
        record['rev'] = commit(self.fork, 'other/build.rs', 'sneak')
        errors = check_fork(record, self.scratch / 'check')
        self.assertTrue(any('differs from the reviewed diffSha256' in error for error in errors))
        self.assertTrue(any('outside modifiedPackages: other/build.rs' in error for error in errors))
        self.assertTrue(any('commits differ' in error for error in errors))

    def test_only_reviewed_metadata_paths_are_allowed(self) -> None:
        self.rev = commit(self.fork, '.github/workflows/graphite-meter.yml', 'test fork')
        self.rev = commit(self.fork, 'src/lib.rs', 'fix root package')
        record = self.record()
        record['commits'] += [{'subject': 'test fork'}, {'subject': 'fix root package'}]
        errors = check_fork(record, self.scratch / 'unreviewed')
        self.assertTrue(any('outside modifiedPackages' in error for error in errors))
        record['metadataFiles'] = ['.github/workflows/graphite-meter.yml']
        self.assertTrue(any('outside modifiedPackages: src/lib.rs' in error
                            for error in check_fork(record, self.scratch / 'metadata')))
        record['modifiedFiles'] = ['src/lib.rs']
        self.assertEqual(check_fork(record, self.scratch / 'reviewed'), [])

    def test_an_unavailable_branch_or_base_says_why_and_keeps_the_other_errors(self) -> None:
        record = self.record() | {'branch': 'missing'}
        self.assertIn("couldn't find remote ref refs/heads/missing", check_fork(record, self.scratch / 'fetch')[0])
        record = self.record() | {'base': 'b' * 40}
        self.assertEqual(check_fork(record, self.scratch / 'base'), [
            f"{self.fork}: upstream v1 is {self.base}, not base {'b' * 40}",
            f"{self.fork}: cannot establish Git ancestry: fatal: Not a valid commit name {'b' * 40}"])

    def test_revision_off_the_fork_branch_fails(self) -> None:
        record = self.record()
        run(self.fork, 'checkout', '-q', '-b', 'scratch')
        record['rev'] = commit(self.fork, 'pkg/lib.rs', 'unreviewed')
        self.assertEqual(check_fork(record, self.scratch / 'check'),
                         [f"{self.fork}: {record['rev']} is not on branch gm/v1"])


if __name__ == '__main__':
    unittest.main()
