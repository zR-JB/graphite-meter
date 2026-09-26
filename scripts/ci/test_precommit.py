from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from precommit import plan_checks, staged_paths


class PlanTests(unittest.TestCase):
    def test_changed_paths_select_component_gates(self) -> None:
        go = "go/internal/server/listeners.go"
        for paths, plan in (
            (("api/routes.txt",), ("check",)),
            (("mise.lock", go), ("check",)),
            ((go,), ("server-check", "server-test", "legal-check")),
            ((".github/workflows/ci.yml",), ("workflow-check", "pipeline-test")),
            (("scripts/legal/model.py",), ("workflow-check", "pipeline-test", "legal-check")),
            (("client/src/app.css",), ("client-ci", "legal-check")),
            (("go/internal/auth/assets/login.tmpl",),
             ("server-check", "server-test", "client-ci", "legal-check")),
            (("docs/DEVELOPMENT.md",), ()),
        ):
            with self.subTest(paths=paths):
                self.assertEqual(plan_checks(paths), plan)

    def test_deleted_and_renamed_paths_still_select_checks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)

            def git(*args: str) -> None:
                subprocess.run(["git", "-c", "user.name=CI", "-c", "user.email=ci@example.invalid",
                                *args], cwd=repo, check=True, capture_output=True)

            git("init", "-q")
            for name in ("go/main.go", ".github/workflows/old.yml", ".github/workflows/ci.yml"):
                (repo / name).parent.mkdir(parents=True, exist_ok=True)
                (repo / name).write_text(name)
            git("add", ".")
            git("commit", "-qm", "base")
            (repo / "go/main.go").write_text("changed")
            git("add", "go/main.go")
            git("rm", "-q", ".github/workflows/old.yml")
            (repo / "docs").mkdir()
            git("mv", ".github/workflows/ci.yml", "docs/ci-example.yml")
            self.assertEqual(sorted(staged_paths(repo)), [
                ".github/workflows/ci.yml", ".github/workflows/old.yml", "docs/ci-example.yml",
                "go/main.go",
            ])


if __name__ == "__main__":
    unittest.main()
