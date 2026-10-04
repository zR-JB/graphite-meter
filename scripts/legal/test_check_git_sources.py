from __future__ import annotations

import hashlib
import subprocess
import tempfile
import unittest
from pathlib import Path

from scripts.legal.check_git_sources import REGISTRY, check_fork, check_lock

REV = "a" * 40
FORK = "https://example.invalid/fork"


def git(directory: Path, *args: str) -> str:
    identity = ["-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false"]
    return subprocess.run(["git", "-C", str(directory), *identity, *args], check=True, text=True,
                          capture_output=True).stdout.strip()


def commit(directory: Path, path: str, subject: str) -> str:
    (directory / path).parent.mkdir(parents=True, exist_ok=True)
    (directory / path).write_text(subject)
    git(directory, "add", path)
    git(directory, "commit", "-q", "-m", subject)
    return git(directory, "rev-parse", "HEAD")


class LockTests(unittest.TestCase):
    forks = [{"fork": FORK, "rev": REV}]

    def lock(self, source: str) -> dict:
        return {"package": [{"name": "member"}, {"name": "registry", "source": REGISTRY},
                            {"name": "forked", "source": source}]}

    def test_only_recorded_full_sha_revisions_pass(self) -> None:
        self.assertEqual(check_lock(self.forks, self.lock(f"git+{FORK}?rev={REV}#{REV}")), [])
        for source in (f"git+{FORK}?branch=main#{REV}", f"git+{FORK}?rev={REV[:12]}#{REV[:12]}",
                       f"git+{FORK}?rev={REV}&rev={REV}#{REV}", f"git+{FORK}?rev={REV}#{'b' * 40}",
                       f"git+https://example.invalid/other?rev={REV}#{REV}",
                       "sparse+https://mirror.example.invalid/index/"):
            with self.subTest(source=source):
                self.assertTrue(check_lock(self.forks, self.lock(source)))

    def test_an_unused_record_fails(self) -> None:
        self.assertEqual(check_lock(self.forks, {"package": []}),
                         [f"unused legal/rust-forks.json record: {FORK}@{REV}"])


class ForkTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.upstream = self.root / "upstream"
        self.upstream.mkdir()
        git(self.upstream, "init", "-q")
        self.base = commit(self.upstream, "pkg/lib.rs", "upstream")
        git(self.upstream, "tag", "v1")
        self.fork = self.root / "fork"
        git(self.root, "clone", "-q", str(self.upstream), str(self.fork))
        git(self.fork, "checkout", "-q", "-b", "gm/v1")
        self.rev = commit(self.fork, "pkg/lib.rs", "fix pkg")
        self.checks = 0

    def record(self) -> dict:
        changes = git(self.fork, "diff-tree", "-r", "--no-renames", "--full-index", self.base, self.rev) + "\n"
        return {"fork": str(self.fork), "branch": "gm/v1", "rev": self.rev, "upstream": str(self.upstream),
                "baseTag": "v1", "base": self.base, "diffSha256": hashlib.sha256(changes.encode()).hexdigest(),
                "modifiedPackages": ["pkg"], "commits": [{"subject": "fix pkg"}]}

    def check(self, record: dict) -> list[str]:
        self.checks += 1
        return check_fork(record, self.root / f"check-{self.checks}")

    def test_the_reviewed_record_passes(self) -> None:
        self.assertEqual(self.check(self.record()), [])

    def test_an_unreviewed_change_outside_the_packages_fails(self) -> None:
        record = self.record()
        record["rev"] = commit(self.fork, "other/build.rs", "sneak")
        self.assertEqual(self.check(record), [
            f"{self.fork}: change set differs from the reviewed diffSha256",
            f"{self.fork}: change outside the reviewed paths: other/build.rs",
            f"{self.fork}: commits differ from the recorded list: ['fix pkg', 'sneak']"])

    def test_only_listed_metadata_and_modified_files_extend_the_packages(self) -> None:
        self.rev = commit(self.fork, ".github/workflows/fork.yml", "test fork")
        self.rev = commit(self.fork, "src/lib.rs", "fix root package")
        record = self.record()
        record["commits"] += [{"subject": "test fork"}, {"subject": "fix root package"}]
        outside = [f"{self.fork}: change outside the reviewed paths: {path}"
                   for path in (".github/workflows/fork.yml", "src/lib.rs")]
        self.assertEqual(self.check(record), outside)
        record["metadataFiles"] = [".github/workflows/fork.yml"]
        self.assertEqual(self.check(record), outside[1:])
        record["modifiedFiles"] = ["src/lib.rs"]
        self.assertEqual(self.check(record), [])

    def test_a_base_other_than_the_upstream_tag_fails(self) -> None:
        record = self.record() | {"base": "b" * 40}
        self.assertEqual(self.check(record), [
            f"{self.fork}: upstream v1 is {self.base}, not base {'b' * 40}",
            f"{self.fork}: base {'b' * 40} is not an ancestor of {self.rev}"])

    def test_an_unavailable_branch_says_why(self) -> None:
        errors = self.check(self.record() | {"branch": "missing"})
        self.assertEqual(len(errors), 1)
        self.assertIn("couldn't find remote ref refs/heads/missing", errors[0])

    def test_a_revision_off_the_fork_branch_fails(self) -> None:
        record = self.record()
        git(self.fork, "checkout", "-q", "-b", "scratch")
        record["rev"] = commit(self.fork, "pkg/lib.rs", "unreviewed")
        self.assertEqual(self.check(record), [f"{self.fork}: {record['rev']} is not on branch gm/v1"])


if __name__ == "__main__":
    unittest.main()
