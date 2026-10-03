from __future__ import annotations

import unittest

import json
import tempfile
from pathlib import Path

from scripts.legal.check_rust_reviews import BUDGET, over_budget, shipped, unreviewed, unreviewed_platforms

REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index'


def review(name: str, version: str, upstream: str = REGISTRY, decision: str = 'approved') -> dict:
    return {'name': name, 'reviewedVersion': version, 'upstream': upstream, 'reviewDecision': decision}


class ReviewScopeTests(unittest.TestCase):
    def test_rust_skips_macos_and_the_server_ships_only_linux(self) -> None:
        targets = 'linux/amd64 x86_64-unknown-linux-musl\ndarwin/arm64 aarch64-apple-darwin\n'
        self.assertEqual(shipped(targets), [
            ('graphite-meter-client', 'x86_64-unknown-linux-musl'),
            ('graphite-meter-server', 'x86_64-unknown-linux-musl'),
        ])

    def test_registry_versions_share_coverage_but_unapproved_and_changed_sources_do_not(self) -> None:
        # Crates only a release build compiles, such as the Windows TUI's, are compiled crates too.
        reviews = [review('ring', '0.17.14'), review('windows-sys', '0.61.1'),
                   review('dlmalloc', '0.2.14', decision='pending')]
        other = 'registry+https://example.invalid/index'
        used = {('ring', '0.17.14', REGISTRY), ('ring', '0.17.14', other),
                ('windows-sys', '0.61.2', REGISTRY), ('dlmalloc', '0.2.14', REGISTRY)}
        self.assertEqual(unreviewed(reviews, used), [('dlmalloc', '0.2.14', REGISTRY), ('ring', '0.17.14', other)])

    def test_git_reviews_bind_the_exact_version_and_revision(self) -> None:
        fork = 'git+https://example.invalid/noq?rev=a#a'
        used = {('noq', '0.1.0', fork), ('noq', '0.2.0', fork), ('noq', '0.1.0', fork.replace('a#a', 'b#b'))}
        self.assertEqual(unreviewed([review('noq', '0.1.0', fork)], used), sorted(used - {('noq', '0.1.0', fork)}))

    def test_the_static_linux_binaries_stay_within_their_crate_budget(self) -> None:
        def crates(count: int) -> set[tuple[str, str]]:
            return {(f'crate-{index}', '1.0.0') for index in range(count)}

        trees = {pair: crates(limit) for pair, limit in BUDGET.items()}
        self.assertEqual(over_budget(trees), [])
        (package, target), limit = next(iter(BUDGET.items()))
        trees[package, target] = crates(limit + 1)
        self.assertEqual(over_budget(trees), [f'{package} compiles {limit + 1} crates for {target}, budget {limit}'])
        del trees[package, target]
        self.assertEqual(over_budget(trees), [f'{package} compiles 0 crates for {target}, budget {limit}'])


class PlatformRecordTests(unittest.TestCase):
    TARGETS = 'linux/amd64 x86_64-unknown-linux-musl\nwindows/amd64 x86_64-pc-windows-gnu\n'
    APPROVED = {'reviewDecision': 'approved', 'reviewNotes': 'reviewed', 'noticesSha256': 'a' * 64}
    WINDOWS = {'target': 'x86_64-pc-windows-gnu', **APPROVED}

    def repo(self, windows_records: list[dict] | None) -> Path:
        root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        (root / 'container').mkdir()
        (root / 'legal').mkdir()
        (root / 'container/Dockerfile.rust').write_text(
            'RUN a --supplement legal/linux.json\nRUN b --supplement legal/linux.json\n')
        (root / 'legal/linux.json').write_text(json.dumps([
            {'target': 'x86_64-unknown-linux-musl', **self.APPROVED}, *(windows_records or [])]))
        return root

    def test_unrelated_records_do_not_affect_shipped_coverage(self) -> None:
        root = self.repo([self.WINDOWS])
        (root / 'legal/rust-platform-host.json').write_text('[]')
        self.assertEqual(unreviewed_platforms(root, self.TARGETS), [])

    def test_shipped_records_must_be_present_approved_and_name_a_notice_fingerprint(self) -> None:
        missing = ['legal/linux.json has no approved record for x86_64-pc-windows-gnu, '
                   'which container/Dockerfile.rust builds']
        for record, errors in ((self.WINDOWS, []), (None, missing),
                               (self.WINDOWS | {'reviewDecision': 'pending', 'reviewNotes': ''}, missing),
                               (self.WINDOWS | {'noticesSha256': ''}, missing)):
            with self.subTest(record=record):
                self.assertEqual(unreviewed_platforms(self.repo([record] if record else None), self.TARGETS), errors)


if __name__ == '__main__':
    unittest.main()
