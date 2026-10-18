import subprocess
import tempfile
import unittest
from pathlib import Path

from dashes import EM, EN, hits


class DashTests(unittest.TestCase):
    def test_only_the_missing_value_glyph_and_legal_texts_keep_dashes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files = {
                "docs/ranges.md": f"Ports\n7247{EN}7249 {EM} all of them\n",
                "client/src/view/format.ts": f'  export const MISSING = "{EN}";\nconst range = "1{EN}4";\n',
                "legal/NOTICE.txt": f"Copyright {EM} a third party\n",
                "docs/image.bin": f"\0{EN}",
            }
            for name, text in files.items():
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_text(text)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            self.assertEqual(hits(root), [f'client/src/view/format.ts:2: const range = "1{EN}4";',
                                          f"docs/ranges.md:2: 7247{EN}7249 {EM} all of them"])


if __name__ == "__main__":
    unittest.main()
