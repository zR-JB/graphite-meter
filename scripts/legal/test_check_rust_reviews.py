from __future__ import annotations

import unittest

import json
import tempfile
from pathlib import Path

from scripts.legal.check_rust_reviews import shipped, unreviewed_platforms, unused

REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index'


def review(name: str, version: str, upstream: str = REGISTRY) -> dict:
    return {'name': name, 'reviewedVersion': version, 'upstream': upstream}


class ReviewScopeTests(unittest.TestCase):
    def test_the_tui_ships_every_target_and_the_server_only_linux(self) -> None:
        targets = 'linux/amd64 x86_64-unknown-linux-musl\ndarwin/arm64 aarch64-apple-darwin\n'
        self.assertEqual(shipped(targets), [
            ('graphite-meter-client', 'x86_64-unknown-linux-musl'),
            ('graphite-meter-server', 'x86_64-unknown-linux-musl'),
            ('graphite-meter-client', 'aarch64-apple-darwin'),
        ])

    def test_a_review_is_used_only_by_its_exact_name_version_and_source(self) -> None:
        fork = 'git+https://example.invalid/noq?rev=a#a'
        reviews = [review('ring', '0.17.14'), review('ring', '0.17.13'), review('noq', '0.1.0'),
                   review('noq', '0.1.0', fork)]
        used = {('ring', '0.17.14', REGISTRY), ('noq', '0.1.0', fork)}
        self.assertEqual(unused(reviews, used), [review('ring', '0.17.13'), review('noq', '0.1.0')])


RUSTC = 'rustc 1.98.1 (48a229cea 2026-09-01)\nhost: aarch64-apple-darwin\nrelease: 1.98.1\nLLVM version: 22.1.8\n'


class PlatformRecordTests(unittest.TestCase):
    TARGETS = 'linux/amd64 x86_64-unknown-linux-musl\ndarwin/arm64 aarch64-apple-darwin\n'

    def repo(self, darwin_records: list[dict] | None) -> Path:
        root = Path(tempfile.mkdtemp())
        (root / '.github/workflows').mkdir(parents=True)
        (root / 'container').mkdir()
        (root / 'legal').mkdir()
        (root / 'rust').mkdir()
        (root / 'rust/rust-toolchain.toml').write_text('[toolchain]\nchannel = "1.98.1"\n')
        (root / '.github/workflows/release-request.yml').write_text(
            'python3 scripts/package-rust.py --supplement legal/macos.json\n')
        (root / 'container/Dockerfile.rust').write_text(
            'RUN a --supplement legal/linux.json\nRUN b --supplement legal/linux.json\n')
        approved = {'rustc': RUSTC, 'reviewDecision': 'approved', 'reviewNotes': 'reviewed'}
        (root / 'legal/linux.json').write_text(json.dumps([{'target': 'x86_64-unknown-linux-musl', **approved}]))
        if darwin_records is not None:
            (root / 'legal/macos.json').write_text(json.dumps(darwin_records))
        return root

    def test_every_shipped_target_has_an_approved_record_where_it_is_built(self) -> None:
        records = [{'target': 'aarch64-apple-darwin', 'rustc': RUSTC, 'reviewDecision': 'approved',
                    'reviewNotes': 'reviewed'}]
        self.assertEqual(unreviewed_platforms(self.repo(records), self.TARGETS), [])

    def test_a_missing_pending_or_stale_record_is_reported_against_its_builder(self) -> None:
        missing = ['legal/macos.json has no approved record for aarch64-apple-darwin on Rust 1.98.1, '
                   'which .github/workflows/release-request.yml builds']
        self.assertEqual(unreviewed_platforms(self.repo(None), self.TARGETS), missing)
        pending = [{'target': 'aarch64-apple-darwin', 'rustc': RUSTC, 'reviewDecision': 'pending', 'reviewNotes': ''}]
        self.assertEqual(unreviewed_platforms(self.repo(pending), self.TARGETS), missing)
        stale = [{'target': 'aarch64-apple-darwin', 'rustc': RUSTC.replace('1.98.1', '1.97.0'),
                  'reviewDecision': 'approved', 'reviewNotes': 'reviewed'}]
        self.assertEqual(unreviewed_platforms(self.repo(stale), self.TARGETS), missing)


if __name__ == '__main__':
    unittest.main()
