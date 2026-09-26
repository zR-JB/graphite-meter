from __future__ import annotations

import pathlib
import re
import shutil
import tempfile
import unittest

from toolchains import ROOT, check, literal_updates, load_pins, runtime_pins, skopeo_version


class ToolchainBoundaryTests(unittest.TestCase):
    def copy_pins(self) -> pathlib.Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = pathlib.Path(directory.name)
        for name in ("mise.toml", "mise.lock", "go/go.mod", "container/Dockerfile"):
            target = root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, target)
        shutil.copytree(ROOT / ".github", root / ".github")
        return root

    def test_runtime_change_requires_lock_image_and_dockerfile_updates(self) -> None:
        root = self.copy_pins()
        check(root)
        path, lock = root / "mise.toml", root / "mise.lock"
        old = runtime_pins(root)["bun"]
        major, minor, patch = old.split(".")
        new = f"{major}.{minor}.{int(patch) + 1}"
        path.write_text(path.read_text().replace(f'bun = "{old}"', f'bun = "{new}"'))
        with self.assertRaisesRegex(ValueError, "mise.lock"):
            check(root)
        lock.write_text(lock.read_text().replace(f'version = "{old}"', f'version = "{new}"'))
        with self.assertRaisesRegex(ValueError, "images.bun must use the tools.bun version"):
            check(root)
        image = f"docker.io/oven/bun:{new}@sha256:" + "a" * 64
        path.write_text(re.sub(r"docker\.io/oven/bun:[^\"]+", image, path.read_text()))
        with self.assertRaisesRegex(ValueError, "container/Dockerfile"):
            check(root)
        updates = literal_updates(root)
        self.assertEqual(set(updates), {root / "container/Dockerfile"})
        for path, content in updates.items():
            path.write_text(content)
        check(root)
        self.assertIn(f"FROM {image} AS client", (root / "container/Dockerfile").read_text())

    def test_publication_image_drift_is_rejected(self) -> None:
        root = self.copy_pins()
        path = root / ".github/workflows/_publish-oci.yml"
        image = load_pins(root)["images"]["skopeo"]
        path.write_text(path.read_text().replace(image, image[:-1] + ("0" if image[-1] != "0" else "1")))
        with self.assertRaisesRegex(ValueError, "_publish-oci.yml"):
            check(root)

    def test_skopeo_tags_require_an_immutable_digest(self) -> None:
        root = self.copy_pins()
        path = root / "mise.toml"
        original = path.read_text()
        current = load_pins(root)["images"]["skopeo"]
        for tag in ("v1.24.1", "v1.24.1-immutable"):
            image = f"quay.io/containers/skopeo:{tag}@sha256:" + "a" * 64
            path.write_text(original.replace(current, image))
            self.assertEqual(skopeo_version(root), "1.24.1")
        path.write_text(original.replace(current, "quay.io/containers/skopeo:v1.24.1"))
        with self.assertRaisesRegex(ValueError, "exact version or image digest"):
            load_pins(root)

    def test_tool_pins_reject_nonversions_and_unknown_entries(self) -> None:
        root = self.copy_pins()
        path = root / "mise.toml"
        original = path.read_text()
        version = load_pins(root)["tools"]["bun"]
        for value in ("latest", "../../other", "$(touch /tmp/tool-pins)", "1.0.0;echo bad"):
            with self.subTest(value=value):
                path.write_text(original.replace(f'bun = "{version}"', f'bun = "{value}"'))
                with self.assertRaisesRegex(ValueError, "tools.bun"):
                    load_pins(root)
        path.write_text(original.replace('[tools]', '[tools]\nunexpected="1.2.3"'))
        with self.assertRaisesRegex(ValueError, "exactly"):
            load_pins(root)

    def test_python_runtime_cannot_float_between_patch_releases(self) -> None:
        root = self.copy_pins()
        path = root / "mise.toml"
        path.write_text(path.read_text().replace(f'python = "{runtime_pins(root)["python"]}"', 'python = "3.14"'))
        with self.assertRaisesRegex(ValueError, "python must select an exact"):
            runtime_pins(root)


if __name__ == "__main__":
    unittest.main()
