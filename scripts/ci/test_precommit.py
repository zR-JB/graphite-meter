import subprocess
import tempfile
import unittest
from pathlib import Path

from precommit import plan_checks, staged_paths


class PlanTests(unittest.TestCase):
    def test_the_browser_client_and_its_generated_assets_select_its_gate(self) -> None:
        for paths, checks in (
            (("client/src/main.tsx",), ("client-ci",)),
            (("rust/server/src/auth/assets/login.tmpl",), ("client-ci",)),
            (("container/Dockerfile", "client/e2e/webview.ts"), ("workflow-check", "pipeline-test", "client-ci")),
            (("rust/server/src/main.rs",), ()),
        ):
            with self.subTest(paths=paths):
                self.assertEqual(plan_checks(paths), checks)

    def test_deleted_and_renamed_paths_still_select_checks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)

            def git(*args: str) -> None:
                subprocess.run(["git", "-c", "user.name=CI", "-c", "user.email=ci@example.invalid",
                                *args], cwd=repo, check=True, capture_output=True)

            git("init", "-q")
            for name in ("rust/main.rs", ".github/workflows/old.yml", ".github/workflows/ci.yml"):
                (repo / name).parent.mkdir(parents=True, exist_ok=True)
                (repo / name).write_text(name)
            git("add", ".")
            git("commit", "-qm", "base")
            (repo / "rust/main.rs").write_text("changed")
            git("add", "rust/main.rs")
            git("rm", "-q", ".github/workflows/old.yml")
            (repo / "docs").mkdir()
            git("mv", ".github/workflows/ci.yml", "docs/ci-example.yml")
            self.assertEqual(sorted(staged_paths(repo)), [
                ".github/workflows/ci.yml", ".github/workflows/old.yml", "docs/ci-example.yml",
                "rust/main.rs",
            ])


if __name__ == "__main__":
    unittest.main()
