#!/usr/bin/env python3
"""Print the mise checks that a commit of the staged paths needs; CI runs the Rust workspace checks."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

TOOLING = (".github/", ".githooks/", "scripts/", "container/", "legal/")
# The client's checks cover the sign-in page assets it generates.
CLIENT_INPUTS = ("client/", "rust/server/src/auth/assets/")


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
    if touched(*TOOLING):
        recipes += ["workflow-check", "pipeline-test"]
    if touched(*CLIENT_INPUTS):
        recipes.append("client-ci")
    return tuple(recipes)


if __name__ == "__main__":
    print(" ".join(plan_checks(staged_paths())))
