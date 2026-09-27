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
KEYS = ("VERSION", "GM_CLIENT_REVISION", "GM_BENCH_FILTER")
# Records each tool's argv and the task environment variables the test inspects.
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
            for name in ("client", "go", "bin", "config", "state", "data", "cache", "scripts"):
                (root / name).mkdir()
            shutil.copy2(ROOT / "scripts/build-version.sh", root / "scripts")
            for name in ("bun", "go", "python3"):
                (root / "bin" / name).write_text(f"#!{sys.executable}\n{SPY % (KEYS,)}")
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
                return json.loads(trace.read_text().splitlines()[0]) if trace.exists() else {}

            payload = f"quoted'\"; $(touch {canary}); `touch {canary}`"
            self.assertEqual(run("auth-preview", payload, "false")["args"][-2:],
                             [payload, "--oidc-ready=false"])
            self.assertEqual(run("bench-throughput", payload)["env"], {"GM_BENCH_FILTER": payload})
            built = run("client-build-prod", VERSION=payload, GM_CLIENT_REVISION=payload)
            self.assertEqual(built["env"], {"VERSION": payload, "GM_CLIENT_REVISION": payload})
            run("goclient-build", VERSION=payload, status=2)
            self.assertFalse(canary.exists())


if __name__ == "__main__":
    unittest.main()
