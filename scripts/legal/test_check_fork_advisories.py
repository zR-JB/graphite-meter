import tempfile
import unittest
from pathlib import Path

from scripts.legal.check_fork_advisories import check

FORK = "https://example.invalid/fork"


def advisory(database: Path, crate: str, id: str, withdrawn: bool = False) -> None:
    path = database / "crates" / crate / f"{id}.md"
    path.parent.mkdir(parents=True, exist_ok=True)
    flag = 'withdrawn = "2026-01-02"\n' if withdrawn else ""
    path.write_text(f'```toml\n[advisory]\nid = "{id}"\npackage = "{crate}"\ndate = "2026-01-01"\n{flag}\n'
                    f'[versions]\npatched = [">= 1.0.0"]\n```\n\n# Title\n')


class ForkAdvisoryTests(unittest.TestCase):
    def setUp(self) -> None:
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.database = Path(scratch.name)
        advisory(self.database, "upstream-proto", "RUSTSEC-2026-0001")
        advisory(self.database, "upstream-proto", "RUSTSEC-2026-0002", withdrawn=True)
        advisory(self.database, "unrelated", "RUSTSEC-2026-0003")

    def fork(self, **verdicts: str) -> dict:
        return {"fork": FORK, "advisoryCrates": ["upstream-proto", "fork-proto"], "advisories": verdicts}

    def test_an_advisory_of_a_derived_crate_needs_a_verdict(self) -> None:
        self.assertEqual(check([self.fork(**{"RUSTSEC-2026-0001": "fixed by fork commit 1"})], self.database), [])
        self.assertEqual(check([self.fork()], self.database),
                         [f"{FORK}: RUSTSEC-2026-0001 for upstream-proto, fork-proto has no verdict in "
                          "legal/rust-forks.json"])

    def test_a_verdict_must_match_a_current_advisory(self) -> None:
        verdicts = {"RUSTSEC-2026-0001": "not affected", "RUSTSEC-2026-0002": "withdrawn upstream"}
        self.assertEqual(check([self.fork(**verdicts)], self.database),
                         [f"{FORK}: the verdict on RUSTSEC-2026-0002 matches no current advisory"])

    def test_a_record_must_name_the_crates_it_derives_from(self) -> None:
        self.assertEqual(check([{"fork": FORK}], self.database),
                         [f"{FORK}: legal/rust-forks.json names no advisoryCrates"])


if __name__ == "__main__":
    unittest.main()
