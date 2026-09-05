"""Exercise staged-index tool resolution, bootstrap, and gate isolation."""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import tomllib
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
                "MISE_CONFIG_FILE": str(base / "mise.toml"),
                "MISE_PYTHON_VERSION": "0.0.0",
                "MISE_TASK_SKIP": "check",
                "MISE_LOCKED": "false",
                "__MISE_DIFF": "not a valid activation",
            }
            with patch.dict(os.environ, poison), patch.object(precommit, "git_text", return_value=""):
                env = precommit.staged_mise_environment(base, staged)
            for key in ("MISE_DATA_DIR", "MISE_CACHE_DIR"):
                self.assertEqual(env[key], poison[key])
            self.assertNotIn("MISE_TASK_SKIP", env)
            self.assertEqual(env["MISE_LOCKED"], "true")
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

    @unittest.skipUnless(MISE, "mise is needed for the staged Python bootstrap integration check")
    def test_hook_executes_staged_implementation_with_staged_isolated_python(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            repo = repository(pathlib.Path(td))
            for name in ("mise.toml", "mise.lock"):
                shutil.copy2(ROOT / name, repo / name)
            version = tomllib.loads((repo / "mise.toml").read_text())["tools"]["python"]
            caches = {key: os.environ[key] for key in ("MISE_DATA_DIR", "MISE_CACHE_DIR") if key in os.environ}
            implementation = repo / "scripts/ci/precommit.py"
            implementation.parent.mkdir(parents=True)
            implementation.write_text(f"""import os, sys
assert sys.flags.isolated
assert '.'.join(map(str, sys.version_info[:3])) == {version!r}
assert os.environ['GIT_INDEX_FILE'] == '.git/index'
assert all(os.environ[key] == value for key, value in {caches!r}.items())
print('staged hook')
""")
            git(repo, "add", "scripts/ci/precommit.py", "mise.toml", "mise.lock")
            implementation.write_text("raise SystemExit('unstaged implementation ran')\n")
            (repo / "mise.toml").write_text('[tools]\npython = "99.0.0"\n')
            (repo / "mise.lock").write_text("unstaged lockfile")
            env = dict(
                os.environ, GIT_INDEX_FILE=".git/index", PYTHONPATH=str(repo),
                XDG_STATE_HOME=str(pathlib.Path(td) / "state"),
                MISE_CONFIG_FILE=str(repo / "mise.toml"), MISE_PYTHON_VERSION="0.0.0",
                MISE_LOCKED="false", MISE_ENV="evil", __MISE_DIFF="poisoned activation",
            )
            result = subprocess.run(
                (str(ROOT / ".githooks/pre-commit"),), cwd=repo, env=env,
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "staged hook")

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
            git_bindings = {"GIT_DIR": ".git", "GIT_WORK_TREE": ".", "GIT_INDEX_FILE": ".git/index"}

            def command(
                args: Sequence[str], *, cwd: pathlib.Path, capture: bool = False,
                check: bool = True, env: Mapping[str, str] | None = None,
            ) -> subprocess.CompletedProcess[bytes]:
                if args[0] != "mise":
                    return real_command(args, cwd=cwd, capture=capture, check=check, env=env)
                assert env is not None
                self.assertTrue(set(git_bindings).isdisjoint(env))
                self.assertEqual(env["PATH"], os.environ["PATH"])
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

            with patch.dict(os.environ, git_bindings), patch.object(precommit, "command", side_effect=command), patch.object(precommit, "run_gitleaks") as scan:
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
