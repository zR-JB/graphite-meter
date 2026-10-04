"""Require an approved review for every crate a shipped Rust binary compiles.

The client ships on every TUI platform and the server on every server platform of rust/Cargo.toml's
workspace metadata; cargo tree over normal and build edges lists the crates each build compiles. A
registry review covers its crate name and source across versions, a git review the exact version and
revision; unused approved reviews may remain. Every shipped target also needs an approved platform
record, and the static x86_64 Linux binaries stay within their crate budgets.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib

from ..ci.rust_workspace import ROOT, Workspace, load

REVIEWS = ROOT / "legal/rust-reviewed-components.json"
FINGERPRINT = re.compile(r"[0-9a-f]{64}")
# Crates each static Linux binary compiles, its own and build-time crates included.
BUDGET = {("graphite-meter-server", "x86_64-unknown-linux-musl"): 143,
          ("graphite-meter-client", "x86_64-unknown-linux-musl"): 150}
Crate = tuple[str, str, str]  # name, version, Cargo.lock source


def layout(reviews: list[dict]) -> bytes:
    """The reviews file's layout: one review per line."""
    lines = ",\n".join("  " + json.dumps(review, ensure_ascii=False, allow_nan=False) for review in reviews)
    return f"[\n{lines}\n]\n".encode()


def shipped(workspace: Workspace) -> list[tuple[str, str]]:
    return ([("graphite-meter-client", target) for target in workspace.tui.values()]
            + [("graphite-meter-server", target) for target in workspace.server.values()])


def compiled(package: str, target: str) -> set[tuple[str, str]]:
    """The name and version of every crate a build of `package` for `target` compiles."""
    tree = subprocess.run(["cargo", "tree", "--locked", "--edges", "normal,build", "--target", target,
                           "--prefix", "none", "--format", "{p}", "--package", package],
                          cwd=ROOT / "rust", check=True, stdout=subprocess.PIPE, text=True).stdout
    return {(name, version.removeprefix("v")) for name, version, *_ in map(str.split, tree.splitlines())}


def unreviewed(reviews: list[dict], used: set[Crate]) -> list[Crate]:
    """The compiled crates without an approved review of their registry family or exact git revision."""
    return sorted(crate for crate in used if not any(
        review.get("reviewDecision") == "approved"
        and (review["name"], review["upstream"]) == (crate[0], crate[2])
        and (crate[2].startswith("registry+") or review["reviewedVersion"] == crate[1])
        for review in reviews))


def unreviewed_platforms(records: list[dict], targets: set[str]) -> list[str]:
    """The targets without an approved, reasoned platform record that fingerprints its notices."""
    approved = {record["target"] for record in records
                if record.get("reviewDecision") == "approved" and record.get("reviewNotes")
                and FINGERPRINT.fullmatch(record.get("noticesSha256", ""))}
    return sorted(targets - approved)


def over_budget(trees: dict[tuple[str, str], set[tuple[str, str]]]) -> list[str]:
    return [f"{package} compiles {len(trees.get((package, target), ()))} crates for {target}, budget {limit}"
            for (package, target), limit in BUDGET.items()
            if not 0 < len(trees.get((package, target), ())) <= limit]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--format", action="store_true", help="rewrite the reviews file in its layout")
    args = parser.parse_args()
    reviews = json.loads(REVIEWS.read_text(encoding="utf-8"))
    if args.format:
        REVIEWS.write_bytes(layout(reviews))
    elif REVIEWS.read_bytes() != layout(reviews):
        sys.exit("legal/rust-reviewed-components.json must hold one review per line (run with --format)")
    workspace = load()
    builds = shipped(workspace)
    records = json.loads((ROOT / workspace.platform_record).read_text(encoding="utf-8"))
    problems = [f"{workspace.platform_record} has no approved record for {target}"
                for target in unreviewed_platforms(records, {target for _, target in builds})]
    sources: dict[tuple[str, str], set[str]] = {}
    for package in tomllib.loads((ROOT / "rust/Cargo.lock").read_text(encoding="utf-8"))["package"]:
        if "source" in package:
            sources.setdefault((package["name"], package["version"]), set()).add(package["source"])
    trees = {build: compiled(*build) for build in builds}
    # Workspace members have no lock source and need no review.
    used = {(name, version, source) for crates in trees.values() for name, version in crates
            for source in sources.get((name, version), ())}
    problems += [f"no approved review names {name} {version} {source}"
                 for name, version, source in unreviewed(reviews, used)] + over_budget(trees)
    if problems:
        sys.exit("The shipped Rust builds fail the review policy:\n" + "\n".join(f"  {item}" for item in problems))
    counts = ", ".join(f"{package} [{target}] {len(crates)}" for (package, target), crates in trees.items())
    print(f"{len(used)} shipped crates covered by approved reviews; compiled crate counts: {counts}")


if __name__ == "__main__":
    main()
