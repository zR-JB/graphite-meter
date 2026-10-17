from __future__ import annotations

import json
import unittest
from pathlib import Path

from scripts.ci.workspace import Workspace
from scripts.legal.check_rust_reviews import BUDGET, layout, over_budget, review_problems, shipped, unreviewed_platforms

REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


def review(name: str, version: str, upstream: str = REGISTRY, decision: str = "approved") -> dict:
    return {"name": name, "reviewedVersion": version, "upstream": upstream, "reviewDecision": decision}


class ReviewTests(unittest.TestCase):
    def test_the_client_ships_on_tui_platforms_and_the_server_on_server_platforms(self) -> None:
        linux, windows = {"linux/amd64": "x86_64-unknown-linux-musl"}, {"windows/amd64": "x86_64-pc-windows-gnu"}
        workspace = Workspace((), linux | windows, linux, Path("legal/platform.json"))
        self.assertEqual(shipped(workspace), [
            ("graphite-meter-client", "x86_64-unknown-linux-musl"), ("graphite-meter-client", "x86_64-pc-windows-gnu"),
            ("graphite-meter-server", "x86_64-unknown-linux-musl")])

    def test_each_review_names_exactly_one_compiled_crate(self) -> None:
        other = "registry+https://example.invalid/index"
        fork, refork = (f"git+https://example.invalid/noq?rev={rev * 40}#{rev * 40}" for rev in "ab")
        reviews = [review("ring", "0.17.14"), review("noq", "0.1.0", fork)]
        used = {("ring", "0.17.14", REGISTRY), ("noq", "0.1.0", fork)}
        self.assertEqual(review_problems(reviews, used), [])
        for extra, crates, problems in (
                ([], {("ring", "0.17.14", other)}, [f"no approved review names ring 0.17.14 {other}"]),
                ([], {("noq", "0.1.0", refork)}, [f"no approved review names noq 0.1.0 {refork}"]),
                ([review("dlmalloc", "0.2.14", decision="pending")], {("dlmalloc", "0.2.14", REGISTRY)},
                 [f"no approved review names dlmalloc 0.2.14 {REGISTRY}"]),
                ([review("hmac", "0.13.0")], set(), [f"the review of hmac 0.13.0 {REGISTRY} names no compiled crate"]),
                ([review("libc", "0.2.189")], {("libc", "0.2.190", REGISTRY)},
                 [f"no approved review names libc 0.2.190 {REGISTRY}",
                  f"the review of libc 0.2.189 {REGISTRY} names no compiled crate"])):
            with self.subTest(extra=extra, crates=crates):
                self.assertEqual(review_problems(reviews + extra, used | crates), problems)

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
        reviews = [review("ring", "0.17.14") | {"reviewNotes": 'é "quoted"\n'}, review("noq", "0.1.0")]
        lines = layout(reviews).decode().splitlines()
        self.assertEqual(lines[0], "[")
        self.assertEqual(lines[-1], "]")
        self.assertEqual([json.loads(line.removesuffix(",")) for line in lines[1:-1]], reviews)
        self.assertEqual(json.loads(layout(reviews)), reviews)
        with self.assertRaises(ValueError):
            layout([{"sha256": float("nan")}])


if __name__ == "__main__":
    unittest.main()
