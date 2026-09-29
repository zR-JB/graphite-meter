from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
KEYS = ("VERSION", "GM_CLIENT_REVISION", "GM_BENCH_FILTER", "GM_ENGINE_VERSION", "GM_RUST_ASSET_DIR",
        "GM_RUST_LEGAL_DIR", "GM_LEGAL_SCAN_OUT", "DEVELOPER_DIR")
# Records each tool's argv and the task environment variables the test inspects; the last call wins.
SPY = """import json, os, sys
with open(os.environ["GM_TASK_TRACE"], "a") as output:
    env = {key: os.environ[key] for key in %r if key in os.environ}
    output.write(json.dumps({"args": sys.argv[1:], "env": env}) + "\\n")
"""


@unittest.skipUnless(os.name == "posix", "Task shell fixture uses POSIX executable scripts")
class MiseTaskTests(unittest.TestCase):
    def test_arguments_and_environment_are_data_not_shell_source(self) -> None:
        mise = shutil.which("mise")
        if mise is None:
            self.fail("mise must be on PATH to verify task behavior")
        with tempfile.TemporaryDirectory(prefix="graphite-task-test-") as directory:
            root = Path(directory)
            # The real task definitions, without tools, so the fixture stays offline.
            source = (ROOT / "mise.toml").read_text()
            source = re.sub(r"(?ms)^\[(?:tools|tool_config)\]\n.*?(?=^\[|\Z)", "", source)
            (root / "mise.toml").write_text(source)
            for name in ("client/dist", "go/internal/static", "bin", "config", "state", "data",
                         "cache", "scripts", "rust"):
                (root / name).mkdir(parents=True)
            shutil.copy2(ROOT / "scripts/build-version.sh", root / "scripts")
            for name in ("bun", "go", "python3", "cargo"):
                # Each tool also prints FAKE_OUTPUT, as a test would on its standard error.
                (root / "bin" / name).write_text(
                    f"#!{sys.executable}\n{SPY % (KEYS,)}print(os.environ.get('FAKE_OUTPUT', ''), file=sys.stderr)\n")
                (root / "bin" / name).chmod(0o755)
            trace, canary = root / "trace.jsonl", root / "injected"
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith(("MISE_", "usage_", "GM_CLIENT_")) and key != "VERSION"}
            env |= {
                "PATH": f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                "MISE_CONFIG_DIR": str(root / "config"), "MISE_STATE_DIR": str(root / "state"),
                "MISE_DATA_DIR": str(root / "data"), "MISE_CACHE_DIR": str(root / "cache"),
                "MISE_TRUSTED_CONFIG_PATHS": str(root), "MISE_AUTO_INSTALL": "0",
                "MISE_TASK_RUN_AUTO_INSTALL": "false", "GM_TASK_TRACE": str(trace),
            }

            def run(task: str, *args: str, status: int = 0, **extra: str) -> dict[str, Any]:
                trace.unlink(missing_ok=True)
                result = subprocess.run([mise, "run", task, *args], cwd=root, env=env | extra,
                                        text=True, capture_output=True, timeout=15)
                self.assertEqual(result.returncode, status, result.stdout + result.stderr)
                return json.loads(trace.read_text().splitlines()[-1]) if trace.exists() else {}

            payload = f"quoted'\"; $(touch {canary}); `touch {canary}`"
            self.assertEqual(run("auth-preview", payload, "false")["args"][-2:],
                             [payload, "--oidc-ready=false"])
            self.assertEqual(run("bench-throughput", payload)["env"], {"GM_BENCH_FILTER": payload})
            built = run("client-build-prod", VERSION=payload, GM_CLIENT_REVISION=payload)
            self.assertEqual(built["env"], {"VERSION": payload, "GM_CLIENT_REVISION": payload})
            run("goclient-build", VERSION=payload, status=2)

            def development(package: str, profile: str, *browser: str) -> list[str]:
                return ["-m", "scripts.legal.rust", "--development", "--package", package, "--profile", profile,
                        "--out", f"rust/target/dev-legal/{package}-{profile}",
                        "--reviews", "legal/rust-reviewed-components.json", *browser]

            # Development builds embed the notices the legal pipeline generates for them on any host: the
            # browser build hands the server's its module scan, and cargo builds with them.
            server = run("rust-server-build", VERSION=payload)
            client, pipeline, _ = map(json.loads, trace.read_text().splitlines())
            scan = client["env"].pop("GM_LEGAL_SCAN_OUT")
            self.assertEqual(pipeline["args"], development("graphite-meter-server", "release", "--browser-scan", scan))
            self.assertEqual(Path(pipeline["env"].pop("GM_RUST_ASSET_DIR")).resolve(), (root / "client/dist").resolve())
            self.assertEqual(server["args"], ["build", "--release", "--locked", "-p", "graphite-meter-server"])
            legal = Path(server["env"].pop("GM_RUST_LEGAL_DIR"))
            self.assertEqual(legal.resolve(), (root / "rust/target/dev-legal/graphite-meter-server-release").resolve())
            self.assertEqual(server["env"].pop("GM_RUST_ASSET_DIR"), str(legal / "browser-assets"))
            for call in (pipeline, server):
                self.assertEqual(call["env"], {"VERSION": payload, "GM_ENGINE_VERSION": f"{payload}-rust"})
            served = run("rust-server-run", "--listen", payload)
            self.assertEqual(served["args"][-3:], ["--", "--listen", payload])
            self.assertEqual(Path(served["env"]["GM_RUST_LEGAL_DIR"]).resolve(),
                             (root / "rust/target/dev-legal/graphite-meter-server-dev").resolve())
            tui = run("tui", "-legal", GM_IMPLEMENTATION="rust")
            pipeline = json.loads(trace.read_text().splitlines()[0])
            self.assertEqual(pipeline["args"], development("graphite-meter-client", "dev"))
            self.assertEqual(tui["args"], ["run", "--locked", "-p", "graphite-meter-client", "--", "-legal"])
            self.assertEqual(Path(tui["env"]["GM_RUST_LEGAL_DIR"]).resolve(),
                             (root / "rust/target/dev-legal/graphite-meter-client-dev").resolve())
            # Release requests and CI package the macOS TUIs with one task, on the reviewed Xcode.
            darwin = run("rust-darwin-package", payload, RELEASE_DIST=str(root / "dist"))
            self.assertEqual(darwin["args"], [
                "-m", "scripts.package_rust", payload, "--os", "darwin", "--output", str(root / "dist"),
                "--supplement", "legal/rust-platform-macos.json", "--checksums"])
            self.assertEqual(darwin["env"], {"DEVELOPER_DIR": "/Applications/Xcode_16.4.app/Contents/Developer"})
            self.assertFalse(canary.exists())

            # The delayed-download gate runs exactly its one ignored test, fails when that matches
            # nothing, and fails when the test passes without measuring.
            gate = run("rust-delayed-downloads")["args"]
            for option in ("--run-ignored only", "--no-tests=fail",
                           "-E test(=quic_downloads_exceed_the_old_window_limit)"):
                self.assertIn(option, " ".join(gate))
            run("rust-delayed-downloads", status=1,
                FAKE_OUTPUT="inconclusive: webtransport=true, loopback below 671 Mbit/s")


if __name__ == "__main__":
    unittest.main()
