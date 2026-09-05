"""Exercise real mise ordering and argument transport without installing tools."""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
from typing import TypedDict
import unittest

ROOT = Path(__file__).resolve().parents[2]


class Invocation(TypedDict):
    tool: str
    args: list[str]
    env: dict[str, str]


@unittest.skipUnless(os.name == "posix", "Task shell fixture uses POSIX executable scripts")
class MiseTaskBehaviorTests(unittest.TestCase):
    def setUp(self) -> None:
        mise = shutil.which("mise")
        if mise is None:
            self.fail("mise must be on PATH to verify task behavior")
        self.mise = mise
        directory = tempfile.TemporaryDirectory(prefix="graphite-task-test-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        # Test the actual definitions; tool installation/lock policy has its own
        # boundary tests. Omitting tool declarations keeps this fixture offline.
        source = (ROOT / "mise.toml").read_text()
        source = re.sub(r"(?ms)^\[(?:tools|tool_config)\]\n.*?(?=^\[|\Z)", "", source)
        (self.root / "mise.toml").write_text(source)
        for name in ("client", "go", "bin", "config", "state", "data", "cache"):
            (self.root / name).mkdir()
        self.trace = self.root / "trace.jsonl"
        keys = ("VERSION", "GM_CLIENT_REVISION", "GM_CLIENT_BUILD_PROFILE", "GM_CLIENT_ALLOW_DUMMY", "GM_BENCH_FILTER")
        spy = f'''#!{sys.executable}
import json, os, sys
from pathlib import Path
with open(os.environ["GM_TASK_TRACE"], "a") as output:
    output.write(json.dumps({{"tool": Path(sys.argv[0]).name, "args": sys.argv[1:], "env": {{key: os.environ[key] for key in {keys!r} if key in os.environ}}}}) + "\\n")
'''
        for name in ("bun", "go", "python3"):
            executable = self.root / "bin" / name
            executable.write_text(spy)
            executable.chmod(0o755)
        (self.root / "bin" / "mise").symlink_to(self.mise)
        global_config = self.root / "global.toml"
        global_config.write_text("")
        self.env = {
            key: value for key, value in os.environ.items()
            if not key.startswith(("MISE_", "usage_", "GM_CLIENT_")) and key != "VERSION"
        }
        self.env.update(
            PATH=f"{self.root / 'bin'}{os.pathsep}{os.environ['PATH']}",
            MISE_CONFIG_DIR=str(self.root / "config"),
            MISE_GLOBAL_CONFIG_FILE=str(global_config),
            MISE_STATE_DIR=str(self.root / "state"),
            MISE_DATA_DIR=str(self.root / "data"),
            MISE_CACHE_DIR=str(self.root / "cache"),
            MISE_TRUSTED_CONFIG_PATHS=str(self.root),
            MISE_AUTO_INSTALL="0",
            MISE_TASK_RUN_AUTO_INSTALL="false",
            GM_TASK_TRACE=str(self.trace),
        )

    def run_task(self, task: str, *args: str, env: dict[str, str] | None = None, status: int = 0) -> list[Invocation]:
        self.trace.unlink(missing_ok=True)
        result = subprocess.run(
            [self.mise, "run", task, *args], cwd=self.root,
            env=self.env | (env or {}), text=True, capture_output=True, timeout=15,
        )
        self.assertEqual(result.returncode, status, result.stdout + result.stderr)
        return [json.loads(line) for line in self.trace.read_text().splitlines()] if self.trace.exists() else []

    def test_release_version_reaches_ordered_build_embed_and_go(self) -> None:
        calls = self.run_task("release-build", "0.7.0-rc.1", env={"VERSION": "outer-version"})
        self.assertEqual([call["tool"] for call in calls], ["bun", "bun", "go"])
        self.assertEqual(calls[0]["args"], ["run", "build"])
        self.assertEqual(calls[0]["env"]["GM_CLIENT_BUILD_PROFILE"], "prod")
        self.assertEqual(calls[0]["env"]["GM_CLIENT_ALLOW_DUMMY"], "0")
        self.assertEqual(calls[0]["env"]["VERSION"], "0.7.0-rc.1")
        self.assertEqual(calls[1]["args"][0], "-e")
        self.assertIn("fs.cpSync('client/dist'", calls[1]["args"][1])
        self.assertIn("EngineVersion=0.7.0-rc.1", " ".join(calls[2]["args"]))

    def test_development_clears_release_version_and_preserves_revision(self) -> None:
        calls = self.run_task("server-build-dev", env={"VERSION": "must-not-release", "GM_CLIENT_REVISION": "revision"})
        self.assertEqual([call["tool"] for call in calls], ["bun", "bun", "go"])
        self.assertNotIn("VERSION", calls[0]["env"])
        self.assertEqual(calls[0]["env"]["GM_CLIENT_REVISION"], "revision")
        self.assertEqual(calls[0]["env"]["GM_CLIENT_BUILD_PROFILE"], "dev")
        self.assertNotIn("EngineVersion=", " ".join(calls[2]["args"]))

    def test_legal_defaults_do_not_invent_a_release(self) -> None:
        calls = self.run_task("legal-check", env={"VERSION": ""})
        self.assertEqual([call["tool"] for call in calls], ["bun", "python3"])
        self.assertEqual(calls[-1]["args"], ["-m", "scripts.legal", "--mode", "check"])
        self.assertEqual(calls[-1]["env"]["VERSION"], "development")

    def test_arguments_and_environment_are_data_not_shell_source(self) -> None:
        canary = self.root / "injected"
        payload = f"quoted'\"; $(touch {canary}); `touch {canary}`"
        calls = self.run_task("auth-preview", payload, "false")
        self.assertEqual(calls[0]["args"][-2:], [payload, "--oidc-ready=false"])
        calls = self.run_task("bench-throughput", payload)
        self.assertEqual(calls[0]["env"]["GM_BENCH_FILTER"], payload)
        calls = self.run_task("client-build-prod", env={"VERSION": payload, "GM_CLIENT_REVISION": payload})
        self.assertEqual(calls[0]["env"]["VERSION"], payload)
        self.assertEqual(calls[0]["env"]["GM_CLIENT_REVISION"], payload)
        self.run_task("goclient-build", env={"VERSION": payload}, status=2)
        self.assertFalse(canary.exists())
