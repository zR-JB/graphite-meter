"""Run the shared Chromium e2e suite against a Rust server with reviewed assets."""

import argparse
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
GROUP_FLAGS = {"heavy": "--group=heavy", "rest": "--group=rest"}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--group", choices=GROUP_FLAGS, help="run complementary groups of the full suite")
    args = parser.parse_args()
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith("GM_") or key == "GM_EXPECTED_CHROME_VERSION"}
    environment |= {"GM_CLIENT_BUILD_PROFILE": "prod", "GM_E2E_SERVER_BIN": str(args.server.resolve())}
    for command in (["bun", "run", "build:e2e-harness"],
                    ["bun", "run", "scripts/e2e.ts", *([GROUP_FLAGS[args.group]] if args.group else [])]):
        subprocess.run(command, cwd=ROOT / "client", env=environment, check=True)

if __name__ == "__main__":
    main()
