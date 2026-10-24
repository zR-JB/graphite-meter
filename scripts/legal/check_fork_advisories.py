#!/usr/bin/env python3
"""Give every RustSec advisory a Cargo fork inherits a reviewed verdict.

cargo-deny matches advisories to crates.io packages by name, so it never matches a fork's git packages, nor the crates
a fork derives from under another name (noq from quinn). Each legal/rust-forks.json record lists those crates in
advisoryCrates and its verdict on each of their advisories in advisories. This fails on an advisory without a verdict
and on a verdict for no current advisory, reading the database cargo-deny fetched.
"""

import argparse
import json
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def advisories(database: Path, crate: str) -> set[str]:
    """The IDs of `crate`'s advisories that are not withdrawn."""
    found = set()
    for path in (database / "crates" / crate).glob("*.md"):
        advisory = tomllib.loads(path.read_text().split("```toml", 1)[1].split("```", 1)[0])["advisory"]
        if "withdrawn" not in advisory:
            found.add(advisory["id"])
    return found


def check(forks: list[dict], database: Path) -> list[str]:
    errors = []
    for fork in forks:
        name, verdicts = fork["fork"], fork.get("advisories", {})
        if not fork.get("advisoryCrates"):
            errors.append(f"{name}: legal/rust-forks.json names no advisoryCrates")
            continue
        current = set().union(*(advisories(database, crate) for crate in fork["advisoryCrates"]))
        errors += [f"{name}: {advisory} for {', '.join(fork['advisoryCrates'])} has no verdict in legal/rust-forks.json"
                   for advisory in sorted(current - verdicts.keys())]
        errors += [f"{name}: the verdict on {advisory} matches no current advisory" for advisory in
                   sorted(verdicts.keys() - current)]
    return errors


def database(root: Path) -> Path:
    """The one advisory database checkout under `root`, cargo-deny's $CARGO_HOME/advisory-dbs."""
    found = [path.parent for path in root.glob("*/crates")]
    if len(found) != 1:
        sys.exit(f"expected one advisory database under {root}, found {len(found)}; run cargo deny check first")
    return found[0]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("databases", type=Path, help="cargo-deny's advisory-dbs directory")
    forks = json.loads((ROOT / "legal/rust-forks.json").read_text())
    if errors := check(forks, database(parser.parse_args().databases)):
        sys.exit("\n".join(errors))
    print(f"every advisory {len(forks)} Cargo forks inherit has a verdict")


if __name__ == "__main__":
    main()
