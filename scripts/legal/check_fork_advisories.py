#!/usr/bin/env python3
"""Give every RustSec advisory a Cargo fork inherits a reviewed verdict.

cargo-deny matches advisories to crates.io packages by name, so it never matches a fork's git packages, nor the crates
a fork derives from under another name (noq from quinn). Each legal/rust-forks.json record lists those crates in
advisoryCrates and its verdict on each of their advisories in advisories. This fails on an advisory without a verdict,
on a verdict for no current advisory and on a verdict quoting a commit subject the fork no longer carries, reading the
RustSec database cargo-deny fetched.
"""

import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RUSTSEC = "github.com/rustsec/advisory-db"


def advisories(database: Path, crate: str) -> dict[str, str]:
    """`crate`'s advisories that are not withdrawn, by ID."""
    found = {}
    for path in (database / "crates" / crate).glob("*.md"):
        advisory = tomllib.loads(path.read_text().split("```toml", 1)[1].split("```", 1)[0])["advisory"]
        if "withdrawn" not in advisory:
            found[advisory["id"]] = crate
    return found


def check(forks: list[dict], database: Path) -> list[str]:
    errors = []
    for fork in forks:
        name, verdicts = fork["fork"], fork.get("advisories", {})
        if not fork.get("advisoryCrates"):
            errors.append(f"{name}: legal/rust-forks.json names no advisoryCrates")
            continue
        current = {id: crate for crate in fork["advisoryCrates"] for id, crate in advisories(database, crate).items()}
        subjects = {commit["subject"] for commit in fork.get("commits", [])}
        errors += [f"{name}: {id} for {current[id]} has no verdict in legal/rust-forks.json"
                   for id in sorted(current.keys() - verdicts.keys())]
        errors += [f"{name}: the verdict on {id} matches no current advisory" for id in sorted(verdicts.keys() - current)]
        # A verdict that names the commit carrying its fix stands only while the fork carries that commit.
        errors += [f"{name}: the verdict on {id} quotes \"{subject}\", which is not among the fork's commits"
                   for id, verdict in sorted(verdicts.items()) for subject in re.findall(r'"([^"]+)"', verdict)
                   if subject not in subjects]
    return errors


def database(root: Path) -> Path:
    """RustSec's checkout among cargo-deny's databases under `root`, by its origin."""
    for checkout in sorted(path.parent for path in root.glob("*/crates")):
        origin = subprocess.run(["git", "-C", str(checkout), "config", "--get", "remote.origin.url"],
                                capture_output=True, text=True, check=False).stdout.strip().lower()
        if origin.removesuffix(".git").removesuffix("/").endswith(RUSTSEC):
            return checkout
    sys.exit(f"no RustSec advisory database under {root}; run cargo deny check first")


def main() -> None:
    cargo_home = Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")
    forks = json.loads((ROOT / "legal/rust-forks.json").read_text())
    if errors := check(forks, database(cargo_home / "advisory-dbs")):
        sys.exit("\n".join(errors))
    print(f"every advisory {len(forks)} Cargo forks inherit has a verdict")


if __name__ == "__main__":
    main()
