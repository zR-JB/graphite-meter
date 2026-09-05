"""Exercise mise at the staged-index trust boundary, without downloading tools."""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from typing import Mapping, Sequence
from unittest.mock import patch

import precommit

ROOT = pathlib.Path(__file__).resolve().parents[2]
MISE = shutil.which("mise")
CONFIG = '[tools]\npython = "3.14.7"\n"aqua:gitleaks/gitleaks" = "8.30.1"\n'


def git(repo: pathlib.Path, *args: str) -> None:
    subprocess.run(("git", *args), cwd=repo, check=True, capture_output=True)


def repository(parent: pathlib.Path) -> pathlib.Path:
    repo = parent / "repo"
    repo.mkdir()
    git(repo, "init", "-q", "-b", "test/staged-mise")
    (repo / "mise.toml").write_text(CONFIG)
    (repo / "mise.lock").write_text("lockfile_version = 1\n")
    (repo / "tracked.txt").write_text("committed")
    git(repo, "add", ".")
    git(repo, "-c", "user.name=CI", "-c", "user.email=ci@example.invalid", "commit", "-qm", "base")
    return repo


class StagedMiseTests(unittest.TestCase):
    def test_environment_keeps_cache_locations_but_not_config_or_version_overrides(self) -> None:
        original = {
            "PATH": os.environ["PATH"],
            "MISE_DATA_DIR": "/shared/installs",
            "MISE_CACHE_DIR": "/shared/downloads",
            "MISE_CONFIG_FILE": "/unstaged/mise.toml",
            "MISE_PYTHON_VERSION": "0.0.0",
            "MISE_ENV": "local",
            "MISE_TASK_SKIP": "check",
            "MISE_LOCKED": "false",
            "__MISE_DIFF": "inherited activation",
            "GIT_INDEX_FILE": ".git/index",
        }
        with patch.dict(os.environ, original), patch.object(precommit, "git_text", return_value="GIT_INDEX_FILE"):
            env = precommit.staged_mise_environment(pathlib.Path("/source"), pathlib.Path("/snapshot/staged"))
            self.assertEqual(os.environ["GIT_INDEX_FILE"], ".git/index")
        self.assertEqual(env["MISE_DATA_DIR"], original["MISE_DATA_DIR"])
        self.assertEqual(env["MISE_CACHE_DIR"], original["MISE_CACHE_DIR"])
        for key in ("GIT_INDEX_FILE", "MISE_CONFIG_FILE", "MISE_PYTHON_VERSION", "MISE_TASK_SKIP", "__MISE_DIFF"):
            self.assertNotIn(key, env)
        self.assertEqual(env["MISE_CONFIG_DIR"], "/snapshot/config")
        self.assertEqual(env["MISE_SYSTEM_CONFIG_DIR"], "/snapshot/system")
        self.assertEqual(env["MISE_CEILING_PATHS"], "/snapshot")
        self.assertEqual(env["MISE_OVERRIDE_CONFIG_FILENAMES"], "mise.toml")
        self.assertEqual(env["MISE_OVERRIDE_TOOL_VERSIONS_FILENAMES"], "none")
        self.assertEqual(env["MISE_LOCKED"], "true")
        self.assertEqual(env["MISE_ENV"], "")

    @unittest.skipUnless(MISE, "mise is needed for the configuration-resolution integration check")
    def test_actual_mise_ignores_parent_local_global_and_system_configuration(self) -> None:
        assert MISE is not None
        with tempfile.TemporaryDirectory() as td:
            base = pathlib.Path(td)
            staged = base / "staged"
            staged.mkdir()
            (staged / "mise.toml").write_text('[env]\nCONFIG_SCOPE = "staged"\n')
            (staged / "mise.local.toml").write_text('[env]\nCONFIG_SCOPE = "unstaged"\n')
            (staged / ".tool-versions").write_text("python 0.0.0\n")
            (base / "mise.toml").write_text('[env]\nPARENT_POISON = "yes"\n')
            (base / ".miserc.toml").write_text('override_config_filenames = ["poison.toml"]\n')
            for name in ("global", "system-global"):
                directory = base / name
                directory.mkdir()
                (directory / "config.toml").write_text('[env]\nGLOBAL_POISON = "yes"\n')
            poison = {
                "MISE_CONFIG_DIR": str(base / "global"),
                "MISE_SYSTEM_CONFIG_DIR": str(base / "system-global"),
                "MISE_DATA_DIR": str(base / "data"),
                "MISE_CACHE_DIR": str(base / "cache"),
                "MISE_ENV": "evil",
                "__MISE_DIFF": "not a valid activation",
            }
            with patch.dict(os.environ, poison), patch.object(precommit, "git_text", return_value=""):
                env = precommit.staged_mise_environment(base, staged)
            configs = subprocess.run(
                (MISE, "config", "ls", "--json"), cwd=staged, env=env,
                check=True, capture_output=True, text=True,
            )
            self.assertEqual([entry["path"] for entry in json.loads(configs.stdout)], [str(staged / "mise.toml")])
            activated = subprocess.run(
                (MISE, "env", "--json"), cwd=staged, env=env,
                check=True, capture_output=True, text=True,
            )
            values = json.loads(activated.stdout)
            self.assertEqual(values["CONFIG_SCOPE"], "staged")
            self.assertNotIn("PARENT_POISON", values)
            self.assertNotIn("GLOBAL_POISON", values)

    def test_hook_bootstraps_only_staged_python_and_executes_the_staged_implementation(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            repo = repository(pathlib.Path(td))
            implementation = repo / "scripts/ci/precommit.py"
            implementation.parent.mkdir(parents=True)
            implementation.write_text("import sys\nassert sys.flags.isolated\nprint('staged hook')\n")
            git(repo, "add", "scripts/ci/precommit.py")
            implementation.write_text("raise SystemExit('unstaged implementation ran')\n")
            (repo / "mise.toml").write_text('[tools]\npython = "99.0.0"\n')
            (repo / "mise.lock").write_text("unstaged lockfile")
            commands = repo / "commands"
            commands.mkdir()
            log = repo / "bootstrap.jsonl"
            fake = commands / "mise"
            fake.write_text(f'''#!{sys.executable} -I
import json, os, pathlib, sys
args = sys.argv[1:]
assert args[0] == "--cd", args
scope = pathlib.Path(args[1])
assert (scope / "mise.toml").read_text() == {CONFIG!r}
assert (scope / "mise.lock").read_text() == "lockfile_version = 1\\n"
assert os.environ["MISE_DATA_DIR"] == {str(repo / "data")!r}
assert os.environ["MISE_CACHE_DIR"] == {str(repo / "cache")!r}
assert os.environ["MISE_LOCKED"] == "true"
assert "MISE_CONFIG_FILE" not in os.environ
assert "MISE_PYTHON_VERSION" not in os.environ
assert "__MISE_DIFF" not in os.environ
with pathlib.Path({str(log)!r}).open("a") as output:
    output.write(json.dumps(args[2:]) + "\\n")
if args[2:4] == ["config", "get"]:
    print("3.14.7")
elif args[2:] == ["install", "python"]:
    pass
elif args[2:] == ["which", "python3", "--tool", "python@3.14.7"]:
    print({sys.executable!r})
else:
    raise SystemExit("unexpected mise operation")
''')
            fake.chmod(0o755)
            env = dict(
                os.environ, PATH=f"{commands}{os.pathsep}{os.environ['PATH']}",
                MISE_DATA_DIR=str(repo / "data"), MISE_CACHE_DIR=str(repo / "cache"),
                MISE_CONFIG_FILE=str(repo / "mise.toml"), MISE_PYTHON_VERSION="0.0.0",
                MISE_LOCKED="false", __MISE_DIFF="poisoned activation",
            )
            result = subprocess.run(
                (str(ROOT / ".githooks/pre-commit"),), cwd=repo, env=env,
                check=True, capture_output=True, text=True,
            )
            self.assertEqual(result.stdout.strip(), "staged hook")
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(calls[1], ["install", "python"])
            self.assertEqual(calls[2], ["which", "python3", "--tool", "python@3.14.7"])

    def test_gitleaks_resolves_staged_pin_but_scans_the_original_index(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            staged = root / "staged"
            staged.mkdir()
            (staged / "mise.toml").write_text(CONFIG)
            (root / "mise.toml").write_text("unstaged invalid TOML")
            env = {"MISE_LOCKED": "true"}
            original = {"GIT_INDEX_FILE": "alternate-index"}
            calls: list[tuple[Sequence[str], pathlib.Path, Mapping[str, str] | None]] = []

            def command(
                args: Sequence[str], *, cwd: pathlib.Path, capture: bool = False,
                check: bool = True, env: Mapping[str, str] | None = None,
            ) -> subprocess.CompletedProcess[bytes]:
                calls.append((args, cwd, env))
                if args[0] != "mise":
                    self.assertEqual(os.environ["GIT_INDEX_FILE"], "alternate-index")
                return subprocess.CompletedProcess(args, 0, stdout=b"/resolved/gitleaks\n")

            with patch.dict(os.environ, original), patch.object(precommit, "command", side_effect=command):
                precommit.run_gitleaks(root, staged, env=env)
            self.assertEqual(calls, [
                (("mise", "install", "aqua:gitleaks/gitleaks"), staged, env),
                (("mise", "which", "gitleaks", "--tool", "aqua:gitleaks/gitleaks@8.30.1"), staged, env),
                (("/resolved/gitleaks", "protect", "--staged", "--redact", "-v"), root, None),
            ])
            for version in ("latest", "../../different-program", "v8.30.1", {"version": "8.30.1"}):
                with self.subTest(version=version):
                    text = '"' + version + '"' if isinstance(version, str) else '{ version = "8.30.1" }'
                    (staged / "mise.toml").write_text(f'[tools]\n"aqua:gitleaks/gitleaks" = {text}\n')
                    with patch.object(precommit, "command") as run:
                        with self.assertRaises(precommit.PrecommitError):
                            precommit.run_gitleaks(root, staged, env=env)
                    run.assert_not_called()

    def test_gate_uses_staged_lock_fresh_dependencies_and_cleans_up_after_failure(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            repo = repository(pathlib.Path(td))
            (repo / "tracked.txt").write_text("staged payload")
            (repo / "client").mkdir()
            (repo / "client/bun.lock").write_text("staged dependency lock")
            git(repo, "add", ".")
            (repo / "tracked.txt").write_text("unstaged fix")
            (repo / "client/bun.lock").write_text("unstaged dependency lock")
            (repo / "client/node_modules").mkdir()
            (repo / ".tools").mkdir()
            real_command = precommit.command
            checked: list[pathlib.Path] = []

            def command(
                args: Sequence[str], *, cwd: pathlib.Path, capture: bool = False,
                check: bool = True, env: Mapping[str, str] | None = None,
            ) -> subprocess.CompletedProcess[bytes]:
                if args[0] != "mise":
                    return real_command(args, cwd=cwd, capture=capture, check=check, env=env)
                assert env is not None
                self.assertNotIn("GIT_INDEX_FILE", env)
                self.assertEqual(env["MISE_LOCKED"], "true")
                if args[1] == "exec":
                    self.assertEqual(args, ("mise", "exec", "--", "bun", "install", "--frozen-lockfile", "--prefer-offline"))
                    self.assertEqual((cwd / "bun.lock").read_text(), "staged dependency lock")
                    self.assertFalse((cwd / "node_modules").exists())
                    (cwd / "node_modules").mkdir()
                    return subprocess.CompletedProcess(args, 0)
                self.assertEqual(args, ("mise", "run", "client-ci"))
                self.assertEqual((cwd / "tracked.txt").read_text(), "staged payload")
                self.assertFalse((cwd / ".tools").exists())
                self.assertTrue((cwd / "client/node_modules").is_dir())
                checked.append(cwd)
                raise precommit.PrecommitError("staged gate failed")

            with patch.dict(os.environ, {"GIT_INDEX_FILE": ".git/index"}), patch.object(precommit, "command", side_effect=command), patch.object(precommit, "run_gitleaks") as scan:
                with self.assertRaisesRegex(precommit.PrecommitError, "staged gate failed"):
                    precommit.run_staged_checks(repo, precommit.CheckPlan(pipeline=False, recipes=("client-ci",)))
            scan.assert_called_once()
            self.assertEqual(len(checked), 1)
            self.assertFalse(checked[0].exists())
            self.assertEqual((repo / "tracked.txt").read_text(), "unstaged fix")
            self.assertEqual((repo / "client/bun.lock").read_text(), "unstaged dependency lock")

    def test_security_rejections_happen_before_snapshot_tools_run(self) -> None:
        cases = (
            ("test/staged-mise", "cert.pem", b"not a certificate", "TLS certificate/key paths"),
            ("test/staged-mise", "ordinary.txt", b"-----BEGIN " + b"PRIVATE KEY-----", "PEM certificate/private-key"),
            ("test/staged-mise", "large.txt", b"x" * (precommit.MAX_STAGED_BYTES + 1), "exceeds 1 MiB"),
            ("main", "ordinary.txt", b"safe", "directly to main"),
        )
        for branch, name, content, message in cases:
            with self.subTest(name=name, branch=branch), tempfile.TemporaryDirectory() as td:
                repo = repository(pathlib.Path(td))
                if branch == "main":
                    git(repo, "branch", "-m", "main")
                (repo / name).write_bytes(content)
                git(repo, "add", name)
                with patch.object(precommit, "repository_root", return_value=repo), patch.object(precommit, "run_staged_checks") as checks:
                    with self.assertRaisesRegex(SystemExit, message):
                        precommit.main()
                checks.assert_not_called()


if __name__ == "__main__":
    unittest.main()
