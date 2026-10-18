import subprocess
import tempfile
import unittest
from pathlib import Path

from screenshots import RECORD, stale, stamp


class ScreenshotRecordTests(unittest.TestCase):
    def test_a_release_takes_only_screenshots_of_its_version_and_current_sources(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            view = root / "client/src/view.ts"
            view.parent.mkdir(parents=True)
            view.write_text("export const title = 'Graphite Meter';\n")
            (root / RECORD).parent.mkdir(parents=True)
            readme = root / "README.md"
            readme.write_text("<sub>Terminal client 0.0.1 at 120 by 40 cells.</sub>\n")
            stamp("v1.2.3", root)
            self.assertEqual(readme.read_text(), "<sub>Terminal client 1.2.3 at 120 by 40 cells.</sub>\n")
            self.assertIsNone(stale("v1.2.3", root), "fresh")
            self.assertEqual(stale("v1.2.4", root),
                             "Screenshots are stale: run mise run screenshots v1.2.4 and merge it before releasing")
            readme.write_text(readme.read_text() + "not drawn\n")
            self.assertIsNone(stale("v1.2.3", root), "a file nothing draws from changes nothing")
            readme.write_text("<sub>Terminal client 1.2.2 at 120 by 40 cells.</sub>\n")
            self.assertEqual(stale("v1.2.3", root), "README captions name 1.2.2: run mise run screenshots v1.2.3")
            stamp("v1.2.3", root)
            view.write_text("export const title = 'Graphite';\n")
            self.assertIsNotNone(stale("v1.2.3", root), "a changed source")
            stamp("v1.2.3", root)
            (root / "client/src/new.ts").write_text("export {};\n")
            self.assertIsNotNone(stale("v1.2.3", root), "a new source not yet committed")
            (root / RECORD).unlink()
            self.assertIsNotNone(stale("v1.2.3", root), "no record")
            with self.assertRaisesRegex(ValueError, "not a release version"):
                stale("v1.2.3; rm -rf /", root)


if __name__ == "__main__":
    unittest.main()
