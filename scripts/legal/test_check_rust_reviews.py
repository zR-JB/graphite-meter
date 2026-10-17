import json
import unittest
from pathlib import Path

from scripts.ci.workspace import Workspace
from scripts.legal.check_rust_reviews import BUDGET, dead, layout, over_budget, shipped, unreviewed_platforms
from scripts.legal.model import Review

REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


def review(name: str, version: str, upstream: str = REGISTRY) -> Review:
    return Review("cargo", name, version, upstream)


class ReviewTests(unittest.TestCase):
    def test_the_client_ships_on_tui_platforms_and_the_server_on_server_platforms(self) -> None:
        linux, windows = {"linux/amd64": "x86_64-unknown-linux-musl"}, {"windows/amd64": "x86_64-pc-windows-gnu"}
        workspace = Workspace((), linux | windows, linux, Path("legal/platform.json"))
        self.assertEqual(shipped(workspace), [
            ("graphite-meter-client", "x86_64-unknown-linux-musl"), ("graphite-meter-client", "x86_64-pc-windows-gnu"),
            ("graphite-meter-server", "x86_64-unknown-linux-musl")])

    def test_a_review_no_compiled_crate_is_checked_against_is_dead(self) -> None:
        fork, refork = (f"git+https://example.invalid/noq?rev={rev * 40}#{rev * 40}" for rev in "ab")
        used = {("libc", "0.2.190", REGISTRY), ("noq", "0.1.0", refork)}
        reviews = [review("libc", "0.2.189"), review("hmac", "0.13.0"), review("noq", "0.1.0", fork)]
        self.assertEqual(dead(reviews, used), reviews[1:])

    def test_the_static_linux_binaries_stay_within_their_crate_budgets(self) -> None:
        def crates(count: int) -> set[tuple[str, str]]:
            return {(f"crate-{index}", "1.0.0") for index in range(count)}

        trees = {build: crates(limit) for build, limit in BUDGET.items()}
        self.assertEqual(over_budget(trees), [])
        (package, target), limit = next(iter(BUDGET.items()))
        trees[package, target] = crates(limit + 1)
        self.assertEqual(over_budget(trees), [f"{package} compiles {limit + 1} crates for {target}, budget {limit}"])
        del trees[package, target]
        self.assertEqual(over_budget(trees), [f"{package} compiles 0 crates for {target}, budget {limit}"])

    def test_every_shipped_target_needs_an_approved_platform_record(self) -> None:
        targets = {"x86_64-unknown-linux-musl", "x86_64-pc-windows-gnu"}
        approved = {"reviewDecision": "approved", "reviewNotes": "reviewed", "noticesSha256": "a" * 64}
        linux = {"target": "x86_64-unknown-linux-musl", **approved}
        windows = {"target": "x86_64-pc-windows-gnu", **approved}
        unrelated = {"target": "aarch64-apple-darwin", **approved}
        self.assertEqual(unreviewed_platforms([linux, windows, unrelated], targets), [])
        for record in (None, windows | {"reviewDecision": "pending"}, windows | {"reviewNotes": ""},
                       windows | {"noticesSha256": "A" * 64}, {"target": "x86_64-pc-windows-gnu"}):
            with self.subTest(record=record):
                records = [linux, unrelated] + ([record] if record else [])
                self.assertEqual(unreviewed_platforms(records, targets), ["x86_64-pc-windows-gnu"])

    def test_the_layout_holds_one_review_per_line(self) -> None:
        reviews = [{"name": "ring", "reviewNotes": 'é "quoted"\n'}, {"name": "noq"}]
        lines = layout(reviews).decode().splitlines()
        self.assertEqual(lines[0], "[")
        self.assertEqual(lines[-1], "]")
        self.assertEqual([json.loads(line.removesuffix(",")) for line in lines[1:-1]], reviews)
        self.assertEqual(json.loads(layout(reviews)), reviews)
        with self.assertRaises(ValueError):
            layout([{"sha256": float("nan")}])


if __name__ == "__main__":
    unittest.main()
