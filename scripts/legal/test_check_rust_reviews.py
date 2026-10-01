from __future__ import annotations

import unittest

import json
import tempfile
from pathlib import Path

from scripts.legal.check_rust_reviews import BUDGET, over_budget, shipped, unreviewed, unreviewed_platforms, unused

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

    def test_a_review_is_used_only_by_its_exact_name_version_and_source(self) -> None:
        fork = 'git+https://example.invalid/noq?rev=a#a'
        reviews = [review('ring', '0.17.14'), review('ring', '0.17.13'), review('noq', '0.1.0'),
                   review('noq', '0.1.0', fork)]
        used = {('ring', '0.17.14', REGISTRY), ('noq', '0.1.0', fork)}
        self.assertEqual(unused(reviews, used), [review('ring', '0.17.13'), review('noq', '0.1.0')])

    def test_every_compiled_crate_needs_an_approved_review_of_its_exact_identity(self) -> None:
        # Crates only a release build compiles, such as the Windows TUI's, are compiled crates too.
        reviews = [review('ring', '0.17.14'), review('windows-sys', '0.61.1'),
                   review('dlmalloc', '0.2.14', decision='pending')]
        used = {('ring', '0.17.14', REGISTRY), ('windows-sys', '0.61.2', REGISTRY), ('dlmalloc', '0.2.14', REGISTRY)}
        self.assertEqual(unreviewed(reviews, used), [('dlmalloc', '0.2.14', REGISTRY), ('windows-sys', '0.61.2', REGISTRY)])
        self.assertEqual(unreviewed(reviews, {('ring', '0.17.14', REGISTRY)}), [])

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


RUSTC = 'rustc 1.98.1 (48a229cea 2026-09-01)\nhost: x86_64-pc-windows-gnu\nrelease: 1.98.1\nLLVM version: 22.1.8\n'


class PlatformRecordTests(unittest.TestCase):
    TARGETS = 'linux/amd64 x86_64-unknown-linux-musl\nwindows/amd64 x86_64-pc-windows-gnu\n'

    def repo(self, windows_records: list[dict] | None) -> Path:
        root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        (root / 'container').mkdir()
        (root / 'legal').mkdir()
        (root / 'rust').mkdir()
        (root / 'rust/rust-toolchain.toml').write_text('[toolchain]\nchannel = "1.98.1"\n')
        (root / 'mise.toml').write_text(
            "run = 'python3 -m scripts.legal.rust --host'\n")
        (root / 'container/Dockerfile.rust').write_text(
            'RUN a --supplement legal/linux.json\nRUN b --supplement legal/linux.json\n')
        approved = {'rustc': RUSTC, 'reviewDecision': 'approved', 'reviewNotes': 'reviewed'}
        (root / 'legal/linux.json').write_text(json.dumps([{'target': 'x86_64-unknown-linux-musl', **approved}]))
        if windows_records is not None:
            (root / 'legal/linux.json').write_text(json.dumps([{'target': 'x86_64-unknown-linux-musl', **approved},
                                                             *windows_records]))
        return root

    def test_every_shipped_target_has_an_approved_record_where_it_is_built(self) -> None:
        records = [{'target': 'x86_64-pc-windows-gnu', 'rustc': RUSTC, 'reviewDecision': 'approved',
                    'reviewNotes': 'reviewed'}]
        self.assertEqual(unreviewed_platforms(self.repo(records), self.TARGETS), [])

    def test_a_record_file_no_builder_reads_is_reported(self) -> None:
        records = [{'target': 'x86_64-pc-windows-gnu', 'rustc': RUSTC, 'reviewDecision': 'approved',
                    'reviewNotes': 'reviewed'}]
        root = self.repo(records)
        (root / 'legal/rust-platform-host.json').write_text('[]')
        self.assertEqual(unreviewed_platforms(root, self.TARGETS), ['legal/rust-platform-host.json is read by no builder'])
        with (root / 'mise.toml').open('a') as mise:
            mise.write("host = 'python3 -m scripts.legal.rust --host'\n")
        self.assertEqual(unreviewed_platforms(root, self.TARGETS), ['legal/rust-platform-host.json is read by no builder'])
        host = {'target': 'x86_64-unknown-linux-gnu',
                'rustc': RUSTC.replace('x86_64-pc-windows-gnu', 'x86_64-unknown-linux-gnu'),
                'reviewDecision': 'approved', 'reviewNotes': 'reviewed'}
        (root / 'legal/rust-platform-host.json').write_text(json.dumps([host]))
        self.assertEqual(unreviewed_platforms(root, self.TARGETS), [])

    def test_a_missing_pending_or_stale_record_is_reported_against_its_builder(self) -> None:
        missing = ['legal/linux.json has no approved record for x86_64-pc-windows-gnu on Rust 1.98.1, '
                   'which container/Dockerfile.rust builds']
        self.assertEqual(unreviewed_platforms(self.repo(None), self.TARGETS), missing)
        pending = [{'target': 'x86_64-pc-windows-gnu', 'rustc': RUSTC, 'reviewDecision': 'pending', 'reviewNotes': ''}]
        self.assertEqual(unreviewed_platforms(self.repo(pending), self.TARGETS), missing)
        stale = [{'target': 'x86_64-pc-windows-gnu', 'rustc': RUSTC.replace('1.98.1', '1.97.0'),
                  'reviewDecision': 'approved', 'reviewNotes': 'reviewed'}]
        self.assertEqual(unreviewed_platforms(self.repo(stale), self.TARGETS), missing)


if __name__ == '__main__':
    unittest.main()
