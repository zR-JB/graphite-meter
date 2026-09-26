#!/usr/bin/env python3
"""Print the mise checks that a commit of the staged paths needs."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

LEGAL_PREFIXES = ("go/", "client/", "legal/", "container/", "scripts/legal/")
LEGAL_FILES = {"LICENSE", "COPYRIGHT", "scripts/package-tui.sh", "scripts/tui-targets.txt"}


def staged_paths(cwd: Path | None = None) -> tuple[str, ...]:
    """Every staged path; a rename reports both its source and its target."""
    listed = subprocess.run(["git", "diff", "--cached", "--name-only", "--no-renames", "-z"],
                            cwd=cwd, capture_output=True, check=True).stdout
    return tuple(os.fsdecode(path) for path in listed.split(b"\0") if path)


def plan_checks(paths: tuple[str, ...]) -> tuple[str, ...]:
    def touched(*prefixes: str) -> bool:
        return any(path.startswith(prefixes) for path in paths)

    if {"mise.toml", "mise.lock"} & set(paths) or touched("api/"):
        return ("check",)
    recipes: list[str] = []
    if touched(".github/", ".githooks/", "scripts/"):
        recipes += ["workflow-check", "pipeline-test"]
    if touched("go/"):
        recipes += ["server-check", "server-test"]
    if touched("client/", "go/internal/auth/assets/"):
        recipes.append("client-ci")
    if touched(*LEGAL_PREFIXES) or LEGAL_FILES & set(paths):
        recipes.append("legal-check")
    return tuple(recipes)


if __name__ == "__main__":
    print(" ".join(plan_checks(staged_paths())))
