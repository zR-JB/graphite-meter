from __future__ import annotations

import unittest

from scripts.legal.check_rust_reviews import shipped, unused

REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index'


def review(name: str, version: str, upstream: str = REGISTRY) -> dict:
    return {'name': name, 'reviewedVersion': version, 'upstream': upstream}


class ReviewScopeTests(unittest.TestCase):
    def test_the_tui_ships_every_target_and_the_server_only_linux_on_glibc(self) -> None:
        targets = 'linux/amd64 x86_64-unknown-linux-musl\ndarwin/arm64 aarch64-apple-darwin\n'
        self.assertEqual(shipped(targets), [
            ('graphite-meter-client', 'x86_64-unknown-linux-musl'),
            ('graphite-meter-server', 'x86_64-unknown-linux-gnu'),
            ('graphite-meter-client', 'aarch64-apple-darwin'),
        ])

    def test_a_review_is_used_only_by_its_exact_name_version_and_source(self) -> None:
        fork = 'git+https://example.invalid/noq?rev=a#a'
        reviews = [review('ring', '0.17.14'), review('ring', '0.17.13'), review('noq', '0.1.0'),
                   review('noq', '0.1.0', fork)]
        used = {('ring', '0.17.14', REGISTRY), ('noq', '0.1.0', fork)}
        self.assertEqual(unused(reviews, used), [review('ring', '0.17.13'), review('noq', '0.1.0')])


if __name__ == '__main__':
    unittest.main()
