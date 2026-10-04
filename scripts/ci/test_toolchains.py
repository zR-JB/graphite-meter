from __future__ import annotations

import hashlib
import pathlib
import re
import shutil
import tempfile
import unittest

from toolchains import ROOT, check, check_rust_manifest, literal_updates, load_pins, runtime_pins


class ToolchainBoundaryTests(unittest.TestCase):
    def copy_pins(self) -> pathlib.Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = pathlib.Path(directory.name)
        for name in ("mise.toml", "mise.lock", "go/go.mod", "container/Dockerfile",
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
        self.assertEqual(set(updates), {root / "container/Dockerfile"})
        for path, content in updates.items():
            path.write_text(content)
        check(root)
        self.assertIn(f"FROM {image} AS client", (root / "container/Dockerfile").read_text())

    def test_publication_image_drift_is_rejected(self) -> None:
        root = self.copy_pins()
        path = root / ".github/workflows/release.yml"
        image = load_pins(root)["images"]["skopeo"]
        path.write_text(path.read_text().replace(image, image[:-1] + ("0" if image[-1] != "0" else "1")))
        with self.assertRaisesRegex(ValueError, "release.yml"):
            check(root)

    def test_rust_is_installed_only_from_the_pinned_channel_manifest(self) -> None:
        root = self.copy_pins()
        manifest = (b'manifest-version = "2"\n\n[pkg.rustc]\nversion = "1.99.0"\n'
                    b'git_commit_hash = "c0ffee"\n\n[pkg.rustc.target.x86_64-unknown-linux-gnu]\n'
                    b'available = true\nurl = "https://static.rust-lang.org/dist/rustc.tar.gz"\n'
                    b'hash = "' + b"1" * 64 + b'"\n')
        path = root / "mise.toml"
        digest = hashlib.sha256(manifest).hexdigest()
        path.write_text(re.sub(r'(?m)^rust_manifest_sha256 = ".*"$', f'rust_manifest_sha256 = "{digest}"',
                               path.read_text()))
        # rustup keeps a rewritten copy, with fields dropped and added but the same archives.
        installed = root / "multirust-channel-manifest.toml"
        installed.write_bytes(manifest.replace(b'git_commit_hash = "c0ffee"\n', b"") + b"components = []\n")
        check_rust_manifest(installed, manifest, root)
        with self.assertRaisesRegex(ValueError, "does not match mise.toml's rust_manifest_sha256"):
            check_rust_manifest(installed, manifest + b"\n", root)
        rewritten = installed.read_bytes()
        # rustup prefers a zst archive, so one added beside an unchanged gz entry is another toolchain.
        for tampered in (rewritten.replace(b"1" * 64, b"2" * 64), rewritten.replace(
                b"available = true\n", b'available = true\nzst_url = "https://static.rust-lang.org/dist/'
                b'rustc.tar.zst"\nzst_hash = "' + b"3" * 64 + b'"\n')):
            installed.write_bytes(tampered)
            with self.assertRaisesRegex(ValueError, "from another than the pinned manifest"):
                check_rust_manifest(installed, manifest, root)

    def test_rust_pins_require_an_exact_release_and_manifest_digest(self) -> None:
        root = self.copy_pins()
        toolchain, mise = root / "rust/rust-toolchain.toml", root / "mise.toml"
        original = toolchain.read_text()
        toolchain.write_text(re.sub(r'(?m)^channel = ".*"$', 'channel = "stable"', original))
        with self.assertRaisesRegex(ValueError, "exact Rust release"):
            load_pins(root)
        toolchain.write_text(original)
        mise.write_text(re.sub(r'(?m)^rust_manifest_sha256 = ".*"$', 'rust_manifest_sha256 = "latest"',
                               mise.read_text()))
        with self.assertRaisesRegex(ValueError, "rust_manifest_sha256 must be a SHA-256"):
            load_pins(root)

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
