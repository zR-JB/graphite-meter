import subprocess
import tempfile
import unittest
from pathlib import Path

from screenshots import RECORD, drifted, stale, stamp


class ScreenshotRecordTests(unittest.TestCase):
    def test_a_release_takes_only_screenshots_of_its_version_and_current_sources(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            pins = root / "mise.toml"
            pins.write_text('[vars]\nbrowser_chrome = "156.0.0.0"\nother_pin = "1"\n')
            view = root / "client/src/view.ts"
            view.parent.mkdir(parents=True)
            view.write_text("export const title = 'Graphite Meter';\n")
            (root / RECORD).parent.mkdir(parents=True)
            readme = root / "README.md"
            readme.write_text("<sub>Terminal client 0.0.1 at 120 by 40 cells.</sub>\n")
            stamp("v1.2.3", root)
            self.assertEqual(readme.read_text(), "<sub>Terminal client 1.2.3 at 120 by 40 cells.</sub>\n")
            self.assertIsNone(stale("v1.2.3", root), "fresh")
            network = root / "rust/client/src/net/lanes.rs"
            network.parent.mkdir(parents=True)
            network.write_text("network implementation\n")
            pins.write_text(pins.read_text().replace('other_pin = "1"', 'other_pin = "2"'))
            self.assertIsNone(stale("v1.2.3", root), "network code and unrelated pins do not draw pixels")
            pins.write_text(pins.read_text().replace("156.0.0.0", "156.0.0.1"))
            self.assertIsNotNone(stale("v1.2.3", root), "the rendering browser changes the pixels")
            stamp("v1.2.3", root)
            terminal = root / "rust/client/src/tui/dial.rs"
            terminal.parent.mkdir(parents=True)
            terminal.write_text("terminal drawing\n")
            self.assertIsNotNone(stale("v1.2.3", root), "terminal drawing changes the pixels")
            stamp("v1.2.3", root)
            self.assertEqual(stale("v1.2.4", root),
                             "Screenshots are stale: run mise run screenshots v1.2.4 and merge it before releasing")
            readme.write_text(readme.read_text() + "not drawn\n")
            self.assertIsNone(stale("v1.2.3", root), "a file nothing draws from changes nothing")
            readme.write_text("<sub>Terminal client 1.2.2.</sub>\n")
            self.assertEqual(stale("v1.2.3", root), "README captions name 1.2.2: run mise run screenshots v1.2.3")
            stamp("v1.2.3", root)
            self.assertEqual(readme.read_text(), "<sub>Terminal client 1.2.3.</sub>\n", "a version ending the sentence")
            stamp("v1.2.3", root)
            self.assertIsNone(drifted(root), "pull requests are not warned while the sources still draw the images")
            view.write_text("export const title = 'Graphite';\n")
            self.assertIsNotNone(stale("v1.2.3", root), "a changed source")
            self.assertRegex(drifted(root) or "", r"^::warning title=Screenshots are out of date::")
            stamp("v1.2.3", root)
            (root / "client/src/new.ts").write_text("export {};\n")
            self.assertIsNotNone(stale("v1.2.3", root), "a new source not yet committed")
            (root / RECORD).unlink()
            self.assertIsNotNone(stale("v1.2.3", root), "no record")
            with self.assertRaisesRegex(ValueError, "not a release version"):
                stale("v1.2.3; rm -rf /", root)


if __name__ == "__main__":
    unittest.main()
