#!/usr/bin/env python3
"""Fail on an em or en dash in a tracked text file: ranges read "1 to 4", asides take commas, colons or sentences.

Only the lines in ALLOWED keep the en dash that both clients show for a missing value; legal/ keeps third-party texts.
"""

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EN, EM = "\u2013", "\u2014"
# Path and stripped line of each place the missing-value glyph is defined, drawn or asserted as shown.
ALLOWED = {
    ("client/src/view/format.ts", f'export const MISSING = "{EN}";'),
    ("rust/client/src/format.rs", f'pub const MISSING: &str = "{EN}";'),
    ("rust/client/src/tui/dial.rs", f"'{EN}' | '-' => [\"   \", \"╺━╸\", \"   \"],"),
    ("rust/client/tests/report.rs", f"Loaded download  12.0 ms  {EN}      24.0 ms  0.4 ms  1 / 41 (2.4%)"),
    ("rust/client/tests/report.rs", f'assert!(text.contains("\\n↓ Download  {EN}\\n\\nLatency "), "{{text}}");'),
}


def hits(root: Path = ROOT) -> list[str]:
    """`path:line: text` of every dash in a tracked text file outside ALLOWED and legal/."""
    found = subprocess.run(["git", "grep", "-I", "-n", "-z", "-F", "-e", EN, "-e", EM],
                           cwd=root, capture_output=True, text=True)
    if found.returncode > 1:
        sys.exit(found.stderr)
    lines = [record.split("\0", 2) for record in found.stdout.split("\n") if record]
    return [f"{path}:{number}: {text.strip()}" for path, number, text in lines
            if not path.startswith("legal/") and (path, text.strip()) not in ALLOWED]


if __name__ == "__main__":
    if found := hits():
        print("\n".join(found))
        sys.exit("Write ranges as 'A to B' and asides with commas, colons or sentences, not em or en dashes")
