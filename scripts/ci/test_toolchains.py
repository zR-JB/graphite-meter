from __future__ import annotations

import pathlib
import re
import shutil
import tempfile
import unittest

from toolchains import ROOT, check, literal_updates, load_pins, runtime_pins


class ToolchainBoundaryTests(unittest.TestCase):
    def copy_pins(self) -> pathlib.Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = pathlib.Path(directory.name)
        for name in ("mise.toml", "mise.lock", "go/go.mod", "container/Dockerfile", "container/Dockerfile.rust",
                     "rust/rust-toolchain.toml"):
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
        self.assertEqual(set(updates), {root / "container/Dockerfile", root / "container/Dockerfile.rust"})
        for path, content in updates.items():
            path.write_text(content)
        check(root)
        self.assertIn(f"FROM {image} AS client", (root / "container/Dockerfile").read_text())
        self.assertIn(f"FROM --platform=$BUILDPLATFORM {image} AS browser",
                      (root / "container/Dockerfile.rust").read_text())

    def test_the_rust_image_follows_the_workspace_toolchain(self) -> None:
        root = self.copy_pins()
        path = root / "rust/rust-toolchain.toml"
        channel = load_pins(root)["images"]["rust"].split(":")[1].split("-")[0]
        path.write_text(path.read_text().replace(f'channel = "{channel}"', 'channel = "1.0.0"'))
        with self.assertRaisesRegex(ValueError, "images.rust must use"):
            load_pins(root)

    def test_rust_image_drift_is_rejected(self) -> None:
        root = self.copy_pins()
        path = root / "container/Dockerfile.rust"
        image = load_pins(root)["images"]["distroless_cc"]
        path.write_text(path.read_text().replace(image, image[:-1] + ("0" if image[-1] != "0" else "1")))
        with self.assertRaisesRegex(ValueError, "Dockerfile.rust"):
            check(root)

    def test_publication_image_drift_is_rejected(self) -> None:
        root = self.copy_pins()
        path = root / ".github/workflows/release.yml"
        image = load_pins(root)["images"]["skopeo"]
        path.write_text(path.read_text().replace(image, image[:-1] + ("0" if image[-1] != "0" else "1")))
        with self.assertRaisesRegex(ValueError, "release.yml"):
            check(root)

    def test_skopeo_tags_require_an_immutable_digest(self) -> None:
        root = self.copy_pins()
        path = root / "mise.toml"
        unpinned = 'image_skopeo = "quay.io/containers/skopeo:v1.24.1"'
        path.write_text(re.sub(r'(?s)image_skopeo = """.*?"""', unpinned, path.read_text()))
        with self.assertRaisesRegex(ValueError, "exact version or image digest"):
            load_pins(root)

    def test_tool_pins_reject_nonversions_and_unknown_entries(self) -> None:
        root = self.copy_pins()
        path = root / "mise.toml"
        original = path.read_text()
        version = load_pins(root)["tools"]["bun"]
        for value in ("latest", "1.4", "../../other", "$(touch /tmp/tool-pins)", "1.0.0;echo bad"):
            with self.subTest(value=value):
                path.write_text(original.replace(f'bun = "{version}"', f'bun = "{value}"'))
                with self.assertRaisesRegex(ValueError, "tools.bun"):
                    load_pins(root)
        path.write_text(original.replace("[tools]", '[tools]\nunexpected="1.2.3"'))
        with self.assertRaisesRegex(ValueError, "exactly"):
            load_pins(root)


if __name__ == "__main__":
    unittest.main()
