#!/usr/bin/env python3
"""Validate the exact staged Git tree before a local commit.

The hook itself runs from the developer working tree, but component checks run
in a temporary Git worktree whose index and files are replaced with the exact
`git write-tree` snapshot. Unstaged fixes therefore cannot make a broken staged
commit pass. Client dependencies are installed from that snapshot's lockfile;
mise shares installed tools and download caches, but only staged configuration
selects their versions. Generated outputs and test artifacts stay there.
"""

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
FULL_GATE_FILES = {"mise.toml", "mise.lock"}
PIPELINE_PREFIXES = (".github/", ".githooks/", "scripts/")
LEGAL_PREFIXES = ("go/", "client/", "legal/", "container/")
LEGAL_FILES = {"LICENSE", "COPYRIGHT", "scripts/package-tui.sh", "scripts/tui-targets.txt"}


class PrecommitError(RuntimeError):
    pass


@dataclass(frozen=True)
class CheckPlan:
    pipeline: bool
    recipes: tuple[str, ...]


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
        if capture:
            detail = (result.stderr or result.stdout).decode("utf-8", errors="replace").strip()
            fail(detail or f"command failed: {' '.join(args)}")
        fail(f"command failed ({result.returncode}): {' '.join(args)}")
    return result


def git_bytes(root: Path, *args: str) -> bytes:
    result = command(("git", *args), cwd=root, capture=True)
    return result.stdout


def git_text(root: Path, *args: str) -> str:
    return git_bytes(root, *args).decode("utf-8", errors="strict").strip()


def staged_worktree_environment(root: Path) -> dict[str, str]:
    """Return an environment that cannot bind child Git to the parent worktree.

    Git exports repository-local variables to hooks, notably GIT_INDEX_FILE.
    A linked worktree uses a `.git` *file*, so inheriting a relative value such
    as `.git/index` makes Git try to create `.git/index.lock` beneath that file.
    More importantly, any later tool that invokes Git could accidentally keep
    addressing the developer worktree instead of the staged snapshot.

    Git documents `rev-parse --local-env-vars` as the set to clear before
    operating on another repository/worktree. Keep all non-repository process
    environment intact and let Git rediscover the linked worktree from cwd.
    """
    local_names = git_text(root, "rev-parse", "--local-env-vars").splitlines()
    env = os.environ.copy()
    for name in local_names:
        env.pop(name, None)
    return env


def repository_root() -> Path:
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        fail("pre-commit must run inside a Git worktree")
    return Path(result.stdout.decode("utf-8", errors="strict").strip()).resolve()


def parse_staged_changes(raw: bytes) -> tuple[StagedChange, ...]:
    """Parse `git diff --name-status -z`, retaining deleted sides of renames.

    Planning from only paths that still exist in the index is unsafe: deleting
    or renaming a workflow could otherwise avoid the pipeline checks that would
    reject the resulting staged tree.
    """
    fields = raw.split(b"\0")
    if fields and fields[-1] == b"":
        fields.pop()
    changes: list[StagedChange] = []
    index = 0
    while index < len(fields):
        status = os.fsdecode(fields[index])
        index += 1
        if not status:
            fail("git returned an empty staged change status")
        kind = status[0]
        if kind in {"R", "C"}:
            if index + 1 >= len(fields):
                fail(f"git returned an incomplete {status} staged path pair")
            old_path = os.fsdecode(fields[index])
            new_path = os.fsdecode(fields[index + 1])
            index += 2
            if kind == "R":
                changes.append(StagedChange(old_path, deleted=True))
            changes.append(StagedChange(new_path, deleted=False))
            continue
        if index >= len(fields):
            fail(f"git returned an incomplete {status} staged path")
        path = os.fsdecode(fields[index])
        index += 1
        changes.append(StagedChange(path, deleted=kind == "D"))
    return tuple(changes)


def staged_changes(root: Path) -> tuple[StagedChange, ...]:
    raw = git_bytes(
        root,
        "diff",
        "--cached",
        "--name-status",
        "--find-renames",
        "--diff-filter=ACMRD",
        "-z",
    )
    return parse_staged_changes(raw)


def is_tls_path(path: str) -> bool:
    return TLS_NAME.search(path) is not None


def staged_blob(root: Path, path: str) -> bytes:
    return git_bytes(root, "show", f":{path}")


def validate_staged_files(root: Path, changes: tuple[StagedChange, ...]) -> None:
    existing = tuple(change.path for change in changes if not change.deleted)
    bad_names = [path for path in existing if is_tls_path(path)]
    if bad_names:
        fail("refusing staged TLS certificate/key paths:\n  " + "\n  ".join(bad_names))

    for path in existing:
        size_text = git_text(root, "cat-file", "-s", f":{path}")
        try:
            size = int(size_text)
        except ValueError as exc:
            raise PrecommitError(f"cannot determine staged size for {path!r}") from exc
        if size > MAX_STAGED_BYTES:
            fail(f"staged file exceeds 1 MiB: {path}")
        if PEM.search(staged_blob(root, path)) is not None:
            fail(f"refusing staged PEM certificate/private-key material: {path}")


def plan_checks(paths: tuple[str, ...]) -> CheckPlan:
    path_set = set(paths)
    if path_set & FULL_GATE_FILES or any(path.startswith("api/") for path in paths):
        return CheckPlan(pipeline=False, recipes=("check",))

    pipeline = any(path.startswith(PIPELINE_PREFIXES) for path in paths)
    recipes: list[str] = []
    if any(path.startswith("go/") or path.startswith("client/src/auth/") or path == "scripts/auth_assets.py" for path in paths):
        recipes.append("check-generated")
    if any(path.startswith("go/") for path in paths):
        recipes.extend(("server-check", "server-test"))
    if any(path.startswith("client/") for path in paths):
        recipes.append("client-ci")
    if any(path.startswith(LEGAL_PREFIXES) or path.startswith("scripts/legal/") or path in LEGAL_FILES for path in paths):
        recipes.append("legal-check")
    return CheckPlan(pipeline=pipeline, recipes=tuple(dict.fromkeys(recipes)))


def staged_mise_environment(root: Path, worktree: Path) -> dict[str, str]:
    """Use staged mise configuration while sharing only installation/download data."""
    env = staged_worktree_environment(root)
    for name in tuple(env):
        if name.startswith(("MISE_", "__MISE_")) and name not in {"MISE_DATA_DIR", "MISE_CACHE_DIR"}:
            del env[name]
    env.update({
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
    })
    return env


def prepare_staged_worktree(
    root: Path,
    tree: str,
    worktree: Path,
    *,
    env: Mapping[str, str],
) -> None:
    command(
        ("git", "worktree", "add", "--quiet", "--detach", "--no-checkout", str(worktree), "HEAD"),
        cwd=root,
    )
    command(("git", "read-tree", tree), cwd=worktree, env=env)
    command(("git", "checkout-index", "-a", "-f"), cwd=worktree, env=env)


def remove_worktree(root: Path, worktree: Path) -> None:
    if not worktree.exists():
        return
    result = command(
        ("git", "worktree", "remove", "--force", str(worktree)),
        cwd=root,
        capture=True,
        check=False,
    )
    if result.returncode != 0:
        # Best-effort filesystem cleanup is safe here; the temporary worktree
        # has no ref and exists only for this hook invocation.
        shutil.rmtree(worktree, ignore_errors=True)
        command(("git", "worktree", "prune"), cwd=root, check=False)


def run_pipeline_checks(worktree: Path, *, env: Mapping[str, str]) -> None:
    command(("mise", "run", "workflow-check"), cwd=worktree, env=env)
    command(("mise", "run", "pipeline-test"), cwd=worktree, env=env)


def run_gitleaks(root: Path, worktree: Path, *, env: Mapping[str, str]) -> None:
    try:
        manifest = tomllib.loads((worktree / "mise.toml").read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise PrecommitError(f"invalid staged mise.toml: {exc}") from exc
    tools = manifest.get("tools")
    backend = "aqua:gitleaks/gitleaks"
    version = tools.get(backend) if isinstance(tools, dict) else None
    if not isinstance(version, str) or re.fullmatch(r"\d+\.\d+\.\d+", version) is None:
        fail("staged mise.toml must pin Gitleaks to an exact version")
    command(("mise", "install", backend), cwd=worktree, env=env)
    resolved = command(
        ("mise", "which", "gitleaks", "--tool", f"{backend}@{version}"),
        cwd=worktree, env=env, capture=True,
    )
    binary = resolved.stdout.decode("utf-8", errors="strict").strip()
    # The scanner must retain the original hook's Git environment and index.
    command((binary, "protect", "--staged", "--redact", "-v"), cwd=root)


def run_staged_checks(root: Path, plan: CheckPlan) -> None:
    tree = git_text(root, "write-tree")
    with tempfile.TemporaryDirectory(prefix="graphite-meter-precommit-") as temp_dir:
        worktree = Path(temp_dir) / "staged"
        worktree_env = staged_mise_environment(root, worktree)
        try:
            prepare_staged_worktree(root, tree, worktree, env=worktree_env)
            run_gitleaks(root, worktree, env=worktree_env)
            if set(plan.recipes) & {"check", "client-ci", "legal-check"}:
                command(
                    ("mise", "exec", "--", "bun", "install", "--frozen-lockfile", "--prefer-offline"),
                    cwd=worktree / "client", env=worktree_env,
                )
            if plan.pipeline:
                run_pipeline_checks(worktree, env=worktree_env)
            for recipe in plan.recipes:
                command(("mise", "run", recipe), cwd=worktree, env=worktree_env)
        finally:
            remove_worktree(root, worktree)


def main() -> None:
    try:
        root = repository_root()
        changes = staged_changes(root)
        if not changes:
            return
        branch_result = command(
            ("git", "symbolic-ref", "--short", "HEAD"),
            cwd=root,
            capture=True,
            check=False,
        )
        branch = (branch_result.stdout or b"HEAD").decode("utf-8", errors="strict").strip()
        if branch == "main":
            fail("refusing to commit directly to main; create a branch first")

        validate_staged_files(root, changes)
        command(("git", "diff", "--cached", "--check"), cwd=root)
        run_staged_checks(root, plan_checks(tuple(change.path for change in changes)))
    except PrecommitError as exc:
        raise SystemExit(f"pre-commit: {exc}") from exc


if __name__ == "__main__":
    main()
