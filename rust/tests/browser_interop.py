"""Run the shared Chromium e2e suite against a Rust server with reviewed assets."""

import argparse
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    args = parser.parse_args()
    environment = {
        key: value for key, value in os.environ.items()
        if not key.startswith("GM_") or key == "GM_EXPECTED_CHROME_VERSION"
    }
    environment.update({
        "GM_CLIENT_BUILD_PROFILE": "prod",
        "GM_E2E_SERVER_BIN": str(args.server.resolve()),
    })

    def run(command: list[str], cwd: Path) -> None:
        subprocess.run(command, cwd=cwd, env=environment, check=True)

    run(["bun", "run", "build:e2e-harness"], ROOT / "client")
    run(["bun", "run", "scripts/e2e.ts"], ROOT / "client")


if __name__ == "__main__":
    main()
