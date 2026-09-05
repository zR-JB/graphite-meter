from __future__ import annotations

import pathlib
import shutil
import tempfile
import unittest
from unittest.mock import patch

import precommit
from toolchains import ROOT, check, literal_updates, load_pins, runtime_pins


class ToolchainBoundaryTests(unittest.TestCase):
    def copy_pins(self) -> pathlib.Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = pathlib.Path(directory.name)
        for name in ("tools.toml", ".python-version", ".bun-version", "go/go.mod", "container/Dockerfile"):
            target = root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, target)
        shutil.copytree(ROOT / ".github", root / ".github")
        return root

    def test_runtime_change_requires_matching_direct_docker_default(self) -> None:
        root = self.copy_pins()
        check(root)
        path = root / ".bun-version"
        major, minor, patch = runtime_pins(root)["bun"].split(".")
        version = f"{major}.{minor}.{int(patch) + 1}"
        path.write_text(version + "\n")
        with self.assertRaisesRegex(ValueError, "container/Dockerfile"):
            check(root)
        updates = literal_updates(root)
        self.assertEqual(set(updates), {root / "container/Dockerfile"})
        for path, content in updates.items():
            path.write_text(content)
        check(root)
        self.assertIn(f"ARG BUN_VERSION={version}", (root / "container/Dockerfile").read_text())

    def test_publication_image_drift_is_rejected(self) -> None:
        root = self.copy_pins()
        path = root / ".github/workflows/_publish-oci.yml"
        image = load_pins(root)["images"]["skopeo"]
        path.write_text(path.read_text().replace(image, image[:-1] + ("0" if image[-1] != "0" else "1")))
        with self.assertRaisesRegex(ValueError, "_publish-oci.yml"):
            check(root)

    def test_tool_pins_reject_nonversions_and_unknown_entries(self) -> None:
        root = self.copy_pins()
        path = root / "tools.toml"
        original = path.read_text()
        version = load_pins(root)["tools"]["just"]
        for value in ("latest", "../../other", "$(touch /tmp/tool-pins)", "1.0.0;echo bad"):
            with self.subTest(value=value):
                path.write_text(original.replace(f'just = "{version}"', f'just = "{value}"'))
                with self.assertRaisesRegex(ValueError, "tools.just"):
                    load_pins(root)
        path.write_text(original + '\n[unexpected]\ncommand="anything"\n')
        with self.assertRaisesRegex(ValueError, "exactly"):
            load_pins(root)

    def test_python_runtime_cannot_float_between_patch_releases(self) -> None:
        root = self.copy_pins()
        (root / ".python-version").write_text("3.14\n")
        with self.assertRaisesRegex(ValueError, "python must select an exact"):
            runtime_pins(root)

    def test_gitleaks_uses_staged_manifest_and_cannot_select_a_path(self) -> None:
        root = self.copy_pins()
        staged = (root / "tools.toml").read_text()
        version = load_pins(root)["tools"]["gitleaks"]
        binary = root / ".tools" / f"gitleaks-{version}" / "gitleaks"
        binary.parent.mkdir(parents=True)
        binary.write_text("fake binary; never executed")
        binary.chmod(0o700)
        (root / "tools.toml").write_text("unstaged invalid data")
        with patch.object(precommit, "git_text", return_value=staged) as read, patch.object(precommit, "command") as command:
            precommit.run_gitleaks(root)
            read.assert_called_once_with(root, "show", ":tools.toml")
            command.assert_called_once_with((str(binary), "protect", "--staged", "--redact", "-v"), cwd=root)
        for bad in ('[tools]\ngitleaks="../../outside"', '[tools]\ngitleaks="latest"', '[invalid'):
            with self.subTest(bad=bad), patch.object(precommit, "git_text", return_value=bad), patch.object(precommit, "command") as command:
                with self.assertRaises(precommit.PrecommitError):
                    precommit.run_gitleaks(root)
                command.assert_not_called()


if __name__ == "__main__":
    unittest.main()
