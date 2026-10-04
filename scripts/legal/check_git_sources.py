#!/usr/bin/env python3
"""Keep Cargo git sources on reviewed fork commits.

Offline, every git package in rust/Cargo.lock is pinned by full-SHA rev to a record in
legal/rust-forks.json, and every record is in use. --verify (network) fetches each fork branch and
upstream tag and checks that the tag is the record's base, that rev is on the branch and descends
from base, and that the change set matches diffSha256, stays inside the reviewed paths and consists
of the recorded commits.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

ROOT = Path(__file__).resolve().parents[2]
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
SHA = re.compile(r"[0-9a-f]{40}")
WORKSPACE_FILES = {"Cargo.toml", "Cargo.lock"}


def git(directory: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(directory), *args], check=True, text=True,
                          capture_output=True, timeout=600).stdout


def ancestor(directory: Path, older: str, newer: str) -> bool:
    """False also when Git lacks either commit."""
    return subprocess.run(["git", "-C", str(directory), "merge-base", "--is-ancestor", older, newer],
                          capture_output=True, timeout=600).returncode == 0


def check_lock(forks: list[dict], lock: dict) -> list[str]:
    allowed = {(fork["fork"], fork["rev"]) for fork in forks}
    used: set[tuple[str, str]] = set()
    errors = []
    for package in lock["package"]:
        source = package.get("source", REGISTRY)
        if source == REGISTRY:
            continue
        parts = urlsplit(source.removeprefix("git+"))
        rev = parts.fragment
        if (not source.startswith("git+") or not SHA.fullmatch(rev)
                or parse_qs(parts.query) != {"rev": [rev]}):
            errors.append(f"{package['name']}: {source} is neither crates.io nor a git rev pinned by full SHA")
            continue
        url = parts._replace(query="", fragment="").geturl()
        if (url, rev) not in allowed:
            errors.append(f"{package['name']}: {url}@{rev} is not in legal/rust-forks.json")
        used.add((url, rev))
    return errors + [f"unused legal/rust-forks.json record: {url}@{rev}" for url, rev in sorted(allowed - used)]


def check_fork(fork: dict, directory: Path) -> list[str]:
    name, rev, base = fork["fork"], fork["rev"], fork["base"]
    branch, tag = f"refs/heads/{fork['branch']}", f"refs/tags/{fork['baseTag']}"
    git(directory.parent, "init", "-q", "--bare", str(directory))
    try:
        git(directory, "fetch", "-q", fork["fork"], f"+{branch}:{branch}")
        git(directory, "fetch", "-q", fork["upstream"], f"+{tag}:{tag}")
    except subprocess.CalledProcessError as error:
        return [f"{name}: branch {fork['branch']} or upstream tag {fork['baseTag']} is unavailable: "
                + error.stderr.strip()]
    errors = []
    if (tagged := git(directory, "rev-parse", tag + "^{commit}").strip()) != base:
        errors.append(f"{name}: upstream {fork['baseTag']} is {tagged}, not base {base}")
    if not ancestor(directory, rev, branch):
        return errors + [f"{name}: {rev} is not on branch {fork['branch']}"]
    if not ancestor(directory, base, rev):
        return errors + [f"{name}: base {base} is not an ancestor of {rev}"]
    changes = git(directory, "diff-tree", "-r", "--no-renames", "--full-index", base, rev)
    if hashlib.sha256(changes.encode()).hexdigest() != fork["diffSha256"]:
        errors.append(f"{name}: change set differs from the reviewed diffSha256")
    files = WORKSPACE_FILES.union(fork.get("metadataFiles", []), fork.get("modifiedFiles", []))
    for line in changes.splitlines():
        path = line.split("\t", 1)[1]
        if path not in files and path.split("/", 1)[0] not in fork["modifiedPackages"]:
            errors.append(f"{name}: change outside the reviewed paths: {path}")
    subjects = git(directory, "log", "--reverse", "--format=%s", f"{base}..{rev}").splitlines()
    if subjects != [commit["subject"] for commit in fork["commits"]]:
        errors.append(f"{name}: commits differ from the recorded list: {subjects}")
    return errors


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--verify", action="store_true", help="fetch each fork and upstream and verify its change set")
    args = parser.parse_args()
    forks = json.loads((ROOT / "legal/rust-forks.json").read_text(encoding="utf-8"))
    errors = check_lock(forks, tomllib.loads((ROOT / "rust/Cargo.lock").read_text(encoding="utf-8")))
    if args.verify:
        with tempfile.TemporaryDirectory() as scratch:
            for index, fork in enumerate(forks):
                errors += check_fork(fork, Path(scratch) / str(index))
    if errors:
        sys.exit("\n".join(errors))
    print(f"{len(forks)} Cargo git sources pinned to reviewed fork commits" + (" and verified" if args.verify else ""))


if __name__ == "__main__":
    main()
