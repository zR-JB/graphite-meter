from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from rust_workspace import ROOT, load


class RustWorkspaceTests(unittest.TestCase):
    def test_members_and_targets_load_from_the_workspace(self) -> None:
        workspace = load()
        self.assertEqual(workspace.members["server"], "graphite-meter-server")
        script = str(ROOT / "scripts/ci/rust_workspace.py")
        for kind, targets in (("tui", workspace.tui), ("server", workspace.server)):
            result = subprocess.run([sys.executable, script, "targets", kind], capture_output=True,
                                    text=True, check=True)
            printed = result.stdout.split()
            self.assertEqual(printed, list(targets.values()))

    def test_values_that_reach_commands_are_validated(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        shutil.copytree(ROOT / "rust", root / "rust", ignore=shutil.ignore_patterns("target"))
        manifest = root / "rust/Cargo.toml"
        original = manifest.read_text()
        for old, new, error in (
            ('"windows/amd64" = "x86_64-pc-windows-gnu"', '"windows/amd64" = "$(touch x)"', "tui must list"),
            ('"windows/amd64" = "x86_64-pc-windows-gnu"\n', "", "tui must list"),
            ('server = ["linux/amd64", "linux/arm64"]', "server = []", "server must list"),
            ('members = ["proto",', 'members = ["crates/*", "proto",', "plain directory name"),
            ('"legal/rust-platform-debian-bookworm.json"', '"../platform.json"', "JSON file in legal/"),
        ):
            with self.subTest(new=new, error=error):
                self.assertIn(old, original)
                manifest.write_text(original.replace(old, new, 1))
                with self.assertRaisesRegex(ValueError, error):
                    load(root)


if __name__ == "__main__":
    unittest.main()
