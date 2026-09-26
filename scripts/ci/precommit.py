#!/usr/bin/env python3
"""Check the exact staged tree in a disposable worktree before a local commit."""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping, NoReturn, Sequence

TLS_NAME = re.compile(
    r"(^|/)(\.dev-certs|certs?|certificates?|letsencrypt)(/|$)|"
    r"\.(pem|key|crt|cer|der|csr|p12|pfx|pkcs8|jks|keystore)$",
    re.IGNORECASE,
)
PEM = re.compile(rb"-----BEGIN (?:CERTIFICATE|(?:[^ -]+ )*PRIVATE KEY)-----")
MAX_STAGED_BYTES = 1024 * 1024
LEGAL_PREFIXES = ("go/", "client/", "legal/", "container/", "scripts/legal/")
LEGAL_FILES = {"LICENSE", "COPYRIGHT", "scripts/package-tui.sh", "scripts/tui-targets.txt"}


class PrecommitError(RuntimeError):
    pass


@dataclass(frozen=True)
class StagedChange:
    path: str
    deleted: bool


def fail(message: str) -> NoReturn:
    raise PrecommitError(message)


def command(
    args: Sequence[str],
    *,
    cwd: Path,
    capture: bool = False,
    check: bool = True,
    env: Mapping[str, str] | None = None,
) -> subprocess.CompletedProcess[bytes]:
    result = subprocess.run(
        list(args),
        cwd=cwd,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        check=False,
        env=None if env is None else dict(env),
    )
    if check and result.returncode != 0:
        detail = (result.stderr or result.stdout or b"").decode(errors="replace").strip()
        fail(detail or f"command failed ({result.returncode}): {' '.join(args)}")
    return result


def git_bytes(root: Path, *args: str) -> bytes:
    return command(("git", *args), cwd=root, capture=True).stdout


def git_text(root: Path, *args: str) -> str:
    return git_bytes(root, *args).decode("utf-8", errors="strict").strip()


def repository_root() -> Path:
    result = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True)
    if result.returncode != 0:
        fail("pre-commit must run inside a Git worktree")
    return Path(os.fsdecode(result.stdout).strip()).resolve()


def parse_staged_changes(raw: bytes) -> tuple[StagedChange, ...]:
    """Parse `git diff --name-status -z`; renames also report their deleted source."""
    fields = iter(os.fsdecode(field) for field in raw.split(b"\0")[:-1])
    changes: list[StagedChange] = []
    for status in fields:
        paths = [next(fields, "") for _ in range(2 if status[:1] in ("R", "C") else 1)]
        if not status or "" in paths:
            fail(f"git returned an incomplete staged change: {status!r}")
        if status[0] == "R":
            changes.append(StagedChange(paths[0], deleted=True))
        changes.append(StagedChange(paths[-1], deleted=status[0] == "D"))
    return tuple(changes)


def validate_staged_files(root: Path, changes: tuple[StagedChange, ...]) -> None:
    existing = [change.path for change in changes if not change.deleted]
    if bad := [path for path in existing if TLS_NAME.search(path)]:
        fail("refusing staged TLS certificate/key paths:\n  " + "\n  ".join(bad))
    for path in existing:
        if int(git_text(root, "cat-file", "-s", f":{path}")) > MAX_STAGED_BYTES:
            fail(f"staged file exceeds 1 MiB: {path}")
        if PEM.search(git_bytes(root, "show", f":{path}")):
            fail(f"refusing staged PEM certificate/private-key material: {path}")


def plan_checks(paths: tuple[str, ...]) -> tuple[str, ...]:
    def touched(*prefixes: str) -> bool:
        return any(path.startswith(prefixes) for path in paths)

    if {"mise.toml", "mise.lock"} & set(paths) or touched("api/"):
        return ("check",)
    recipes: list[str] = []
    if touched(".github/", ".githooks/", "scripts/"):
        recipes += ["workflow-check", "pipeline-test"]
    if touched("go/", "client/src/auth/", "scripts/auth_assets.py"):
        recipes.append("check-generated")
    if touched("go/"):
        recipes += ["server-check", "server-test"]
    if touched("client/"):
        recipes.append("client-ci")
    if touched(*LEGAL_PREFIXES) or LEGAL_FILES & set(paths):
        recipes.append("legal-check")
    return tuple(recipes)


def staged_mise_environment(root: Path, worktree: Path) -> dict[str, str]:
    # Hooks export GIT_INDEX_FILE and friends; child Git must find the staged worktree.
    local = set(git_text(root, "rev-parse", "--local-env-vars").splitlines())
    shared = {"MISE_DATA_DIR", "MISE_CACHE_DIR"}
    env = {
        name: value for name, value in os.environ.items() if name not in local
        and (name in shared or not name.startswith(("MISE_", "__MISE_")))
    }
    return env | {
        "MISE_CONFIG_DIR": str(worktree.parent / "config"),
        "MISE_SYSTEM_CONFIG_DIR": str(worktree.parent / "system"),
        "MISE_CEILING_PATHS": str(worktree.parent),
        "MISE_TRUSTED_CONFIG_PATHS": str(worktree),
        "MISE_OVERRIDE_CONFIG_FILENAMES": "mise.toml",
        "MISE_OVERRIDE_TOOL_VERSIONS_FILENAMES": "none",
        "MISE_ENV": "",
        "MISE_AUTO_ENV": "false",
        "MISE_ENV_CACHE": "false",
        "MISE_LOCKED": "true",
    }


def run_gitleaks(root: Path, worktree: Path, *, env: Mapping[str, str]) -> None:
    try:
        tools = tomllib.loads((worktree / "mise.toml").read_text(encoding="utf-8")).get("tools")
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise PrecommitError(f"invalid staged mise.toml: {exc}") from exc
    backend = "aqua:gitleaks/gitleaks"
    version = tools.get(backend) if isinstance(tools, dict) else None
    if not isinstance(version, str) or re.fullmatch(r"\d+\.\d+\.\d+", version) is None:
        fail("staged mise.toml must pin Gitleaks to an exact version")
    command(("mise", "install", backend), cwd=worktree, env=env)
    which = ("mise", "which", "gitleaks", "--tool", f"{backend}@{version}")
    binary = command(which, cwd=worktree, env=env, capture=True).stdout.decode().strip()
    # The scanner keeps the hook's own Git environment so it reads the real index.
    command((binary, "protect", "--staged", "--redact", "-v"), cwd=root)


def run_staged_checks(root: Path, recipes: tuple[str, ...]) -> None:
    tree = git_text(root, "write-tree")
    with tempfile.TemporaryDirectory(prefix="graphite-meter-precommit-") as temp_dir:
        worktree = Path(temp_dir) / "staged"
        env = staged_mise_environment(root, worktree)
        add = ("git", "worktree", "add", "--quiet", "--detach", "--no-checkout", str(worktree))
        try:
            command((*add, "HEAD"), cwd=root)
            command(("git", "read-tree", tree), cwd=worktree, env=env)
            command(("git", "checkout-index", "-a", "-f"), cwd=worktree, env=env)
            run_gitleaks(root, worktree, env=env)
            if {"check", "client-ci", "legal-check"} & set(recipes):
                install = ("bun", "install", "--frozen-lockfile", "--prefer-offline")
                command(("mise", "exec", "--", *install), cwd=worktree / "client", env=env)
            for recipe in recipes:
                command(("mise", "run", recipe), cwd=worktree, env=env)
        finally:
            remove = ("git", "worktree", "remove", "--force", str(worktree))
            removed = command(remove, cwd=root, capture=True, check=False).returncode == 0
            if worktree.exists() and not removed:
                shutil.rmtree(worktree, ignore_errors=True)
                command(("git", "worktree", "prune"), cwd=root, check=False)


def main() -> None:
    try:
        root = repository_root()
        raw = git_bytes(root, "diff", "--cached", "--name-status", "--find-renames",
                        "--diff-filter=ACMRD", "-z")
        if not (changes := parse_staged_changes(raw)):
            return
        branch = command(("git", "symbolic-ref", "--short", "HEAD"), cwd=root, capture=True,
                         check=False).stdout.decode().strip()
        if branch == "main":
            fail("refusing to commit directly to main; create a branch first")
        validate_staged_files(root, changes)
        command(("git", "diff", "--cached", "--check"), cwd=root)
        run_staged_checks(root, plan_checks(tuple(change.path for change in changes)))
    except PrecommitError as exc:
        raise SystemExit(f"pre-commit: {exc}") from exc


if __name__ == "__main__":
    main()
