import hashlib
import http.server
import json
import pathlib
import re
import shutil
import tempfile
import threading
import unittest
from unittest.mock import patch

from toolchains import (ROOT, check, check_rust_manifest, download, latest_browsers, literal_updates, load_pins,
                        write_browsers)


class ToolchainBoundaryTests(unittest.TestCase):
    def copy_pins(self) -> pathlib.Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = pathlib.Path(directory.name)
        for name in ("mise.toml", "mise.lock", "container/Dockerfile", "rust/rust-toolchain.toml"):
            target = root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, target)
        shutil.copytree(ROOT / ".github", root / ".github")
        return root

    def test_runtime_change_requires_lock_image_and_dockerfile_updates(self) -> None:
        root = self.copy_pins()
        check(root)
        path, lock = root / "mise.toml", root / "mise.lock"
        old = load_pins(root)["tools"]["bun"]
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
        self.assertIn(f"FROM --platform=$BUILDPLATFORM {image} AS browser", (root / "container/Dockerfile").read_text())

    def test_the_rust_builder_images_follow_the_python_and_rust_pins(self) -> None:
        root = self.copy_pins()
        path = root / "mise.toml"
        original = path.read_text()
        for pattern, replacement, error in (
            (r"python:[\d.]+-slim", "python:3.13.1-slim", "images.python must use the tools.python"),
            (r"rust:[\d.]+-trixie", "rust:1.0.0-trixie", "images.rust must use the rust/rust-toolchain"),
            (r"(rust:[\d.]+-trixie)@sha256:[0-9a-f]+", r"\1", "exact version or image digest"),
        ):
            with self.subTest(replacement=replacement):
                path.write_text(re.sub(pattern, replacement, original))
                with self.assertRaisesRegex(ValueError, error):
                    load_pins(root)
        path.write_text(original)
        digest = load_pins(root)["images"]["rust"].rsplit("@sha256:", 1)[1]
        dockerfile = root / "container/Dockerfile"
        dockerfile.write_text(dockerfile.read_text().replace(digest, "0" * 64))
        with self.assertRaisesRegex(ValueError, "container/Dockerfile"):
            check(root)

    def test_publication_and_builder_image_drift_is_rejected(self) -> None:
        for name, pin in ((".github/workflows/release.yml", "skopeo"),
                          (".github/actions/setup-buildx/action.yml", "buildkit")):
            with self.subTest(pin=pin):
                root = self.copy_pins()
                path = root / name
                image = load_pins(root)["images"][pin]
                path.write_text(path.read_text().replace(image, image[:-1] + ("0" if image[-1] != "0" else "1")))
                with self.assertRaisesRegex(ValueError, name):
                    check(root)

    def test_the_builder_downloads_the_pinned_llvm_mingw(self) -> None:
        root = self.copy_pins()
        mise = root / "mise.toml"
        mise.write_text(re.sub(r'(?m)^llvm_mingw_sha256 = ".*"$', f'llvm_mingw_sha256 = "{"0" * 64}"', mise.read_text()))
        with self.assertRaisesRegex(ValueError, "container/Dockerfile"):
            check(root)
        for path, content in literal_updates(root).items():
            path.write_text(content)
        check(root)

    def test_the_lock_pins_only_the_tools_mise_uses(self) -> None:
        root = self.copy_pins()
        lock = root / "mise.lock"
        lock.write_text(lock.read_text() + '\n[[tools.node]]\nversion = "24.0.0"\nbackend = "core:node"\n')
        with self.assertRaisesRegex(ValueError, "does not use: node"):
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

    def test_latest_browsers_come_only_from_exact_stable_releases(self) -> None:
        digest = "ab" * 32
        chrome = "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions.json"
        firefox = "https://product-details.mozilla.org/1.0/firefox_versions.json"
        sums = "https://ftp.mozilla.org/pub/firefox/releases/158.0/SHA256SUMS"
        released = {
            chrome: json.dumps({"channels": {"Stable": {"version": "155.0.8059.84"},
                                             "Canary": {"version": "158.0.8110.0"}}}).encode(),
            firefox: json.dumps({"LATEST_FIREFOX_VERSION": "158.0", "FIREFOX_NIGHTLY": "160.0a1"}).encode(),
            sums: (f"{'cd' * 32}  linux-x86_64/en-US/firefox-158.0.tar.bz2\n"
                   f"{digest}  linux-x86_64/en-US/firefox-158.0.tar.xz\n"
                   f"{'ef' * 32}  update/linux-x86_64/en-US/firefox-158.0.tar.xz\n").encode(),
        }
        root = self.copy_pins()
        browsers = latest_browsers(released.__getitem__)
        self.assertEqual(browsers, {"chrome": "155.0.8059.84", "firefox": "158.0", "firefox_sha256": digest})
        write_browsers(browsers, root)
        self.assertEqual(load_pins(root)["browser"], browsers)
        check(root)
        for url, body, error in (
            (chrome, b'{"channels": {"Stable": {"version": "stable"}}}', "chrome is not an exact release"),
            (chrome, b'{"channels": []}', "unexpected browser release metadata"),
            (firefox, b'{"LATEST_FIREFOX_VERSION": "../158.0"}', "firefox is not an exact release"),
            (firefox, b"<html>", "Expecting value"),
            (sums, released[sums] * 2, "must list linux-x86_64/en-US/firefox-158.0.tar.xz once"),
            (sums, released[sums].replace(digest.encode(), b"latest"), "firefox_sha256 is not an exact"),
        ):
            with self.subTest(body=body), self.assertRaisesRegex(ValueError, error):
                latest_browsers((released | {url: body}).__getitem__)

    def test_a_download_cut_short_is_retried_and_one_too_large_refused(self) -> None:
        bodies = [b"cut", b"whole body", b"cut", b"cut", b"cut"]

        class Server(http.server.BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                body = bodies.pop(0)
                self.send_response(200)
                self.send_header("Content-Length", "10")
                self.end_headers()
                self.wfile.write(body)
                self.close_connection = True

            def log_message(self, format: str, *args: object) -> None:
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Server)
        threading.Thread(target=server.serve_forever, args=(0.05,), daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        url = f"http://127.0.0.1:{server.server_port}/"
        with patch("toolchains.time.sleep"):
            self.assertEqual(download(url), b"whole body")
            with self.assertRaisesRegex(ConnectionError, "IncompleteRead"):
                download(url)
        self.assertEqual(bodies, [])
        bodies.append(b"whole body")
        with self.assertRaisesRegex(ValueError, "larger than 9 bytes"):
            download(url, limit=9)


if __name__ == "__main__":
    unittest.main()
