from __future__ import annotations

import io
import os
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts import rust_build
from scripts.legal.test_rust import Scratch


class BrowserCacheTests(Scratch):
    def test_the_browser_build_is_reused_until_a_browser_input_or_its_output_changes(self) -> None:
        self.write("client/src/app.ts", "app")
        builds: list[str] = []

        def build(command: list[str], cwd: Path, check: bool, env: dict[str, str]) -> None:
            builds.append(env["GM_CLIENT_BUILD_PROFILE"])
            (Path(env["GM_LEGAL_SCAN_DIR"]) / "assets").mkdir(parents=True)
            (Path(env["GM_LEGAL_SCAN_DIR"]) / "index.html").write_text(env.get("VERSION", "unversioned"))
            Path(env["GM_LEGAL_SCAN_OUT"]).write_text("[]")

        def output(command: list[str], **_: object) -> str | bytes:
            if command[:2] == ["git", "ls-files"]:
                return b"client/src/app.ts\0client/removed.ts\0"
            return "abc1234\n" if command[0] == "git" else "1.4.2\n"

        self.enterContext(patch.object(rust_build, "ROOT", self.root))
        self.enterContext(patch("subprocess.run", side_effect=build))
        self.enterContext(patch("subprocess.check_output", side_effect=output))
        self.enterContext(patch.dict(os.environ, {"VERSION": "1.2.3"}))
        self.enterContext(patch("sys.stdout", io.StringIO()))
        assets, scan = rust_build.browser("prod")
        self.assertEqual(((assets / "index.html").read_text(), scan.read_text()), ("1.2.3", "[]"))
        self.assertEqual(rust_build.browser("dev")[0].joinpath("index.html").read_text(), "unversioned")
        for change, rebuilds in (
            (lambda: None, False),
            (lambda: os.environ.update(GM_LISTEN=":7000", GM_TLS_CERT="cert.pem"), False),
            (lambda: self.write("client/src/app.ts", "changed"), True),
            (lambda: os.environ.update(VITE_FLAG="1"), True),
            (lambda: os.environ.update(VERSION="1.2.4"), True),
            (lambda: (assets / "index.html").write_text("edited"), True),
            (lambda: scan.unlink(), True),
        ):
            before = len(builds)
            change()
            rust_build.browser("prod")
            self.assertEqual(len(builds) - before, int(rebuilds))
        self.assertEqual(builds.count("dev"), 1)


if __name__ == "__main__":
    unittest.main()
