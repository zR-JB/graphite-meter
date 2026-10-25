"""Require an approved review for every crate a shipped Rust binary compiles.

The client ships on every TUI platform and the server on every server platform of rust/Cargo.toml's
workspace metadata; cargo tree over normal and build edges lists the crates each build compiles, and each
must match its review's license expression and legal files. A review of no compiled crate fails. Every
shipped target also needs an approved platform record, and the static x86_64 Linux binaries stay within
their crate budgets. The manual license texts of legal/rust-provenance.json keep their reviewed SHA-256,
and the notice sources stay reviewed for the pinned Rust release.
"""

import argparse
import json
import re
import sys
from pathlib import Path

from ..ci.workspace import ROOT, Workspace, load
from .model import LegalError, Provenance, Review, array, read_json
from .review import add_provenance
from .rust_inventory import cargo_metadata, components, covering, selected, tree
from .rust_platform import check_notice_sources

REVIEWS = ROOT / "legal/rust-reviewed-components.json"
FINGERPRINT = re.compile(r"[0-9a-f]{64}")
# Crates each static Linux binary compiles, its own and build-time crates included.
BUDGET = {("graphite-meter-server", "x86_64-unknown-linux-musl"): 121,
          ("graphite-meter-client", "x86_64-unknown-linux-musl"): 146}
Crate = tuple[str, str, str]  # name, version, Cargo.lock source


def layout(reviews: list[dict]) -> bytes:
    """The reviews file's layout: one review per line."""
    lines = ",\n".join("  " + json.dumps(review, ensure_ascii=False, allow_nan=False) for review in reviews)
    return f"[\n{lines}\n]\n".encode()


def shipped(workspace: Workspace) -> list[tuple[str, str]]:
    return ([("graphite-meter-client", target) for target in workspace.tui.values()]
            + [("graphite-meter-server", target) for target in workspace.server.values()])


def manual_problems(root: Path) -> list[str]:
    """Incomplete or changed manual Rust provenance, or notice sources not reviewed for the pinned Rust."""
    try:
        entries = [Provenance.parse(item) for item in array(read_json(root / "legal/rust-provenance.json"))]
        foreign = [entry.name for entry in entries if entry.ecosystem != "cargo" or entry.artifactScopes != ["rust"]]
        if foreign:
            raise LegalError(f"legal/rust-provenance.json entries must be cargo crates in the rust scope: {foreign}")
        add_provenance(root, [], entries, "rust")
        check_notice_sources(root)
    except LegalError as error:
        return [str(error)]
    return []


def dead(reviews: list[Review], used: set[Crate]) -> list[Review]:
    """The reviews no compiled crate is checked against."""
    covered = [review for crate in used for review in covering(*crate, reviews)]
    return [review for review in reviews if review not in covered]


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
    reviews = json.loads(REVIEWS.read_text())
    if args.format:
        REVIEWS.write_bytes(layout(reviews))
    elif REVIEWS.read_bytes() != layout(reviews):
        sys.exit("legal/rust-reviewed-components.json must hold one review per line (run with --format)")
    workspace = load()
    builds = shipped(workspace)
    records = json.loads((ROOT / workspace.platform_record).read_text())
    problems = [f"{workspace.platform_record} has no approved record for {target}"
                for target in unreviewed_platforms(records, {target for _, target in builds})]
    problems += manual_problems(ROOT)
    trees = {build: tree(*build) for build in builds}
    metadata = cargo_metadata(*(target for _, target in builds))
    parsed = [Review.parse(review) for review in reviews]
    provenance = [Provenance.parse(item) for item in array(read_json(ROOT / "legal/rust-provenance.json"))]
    try:
        found, _, failures = components(metadata, {identity: {} for identity in selected(
            metadata, set().union(*trees.values()))}, parsed, provenance)
    except LegalError as error:
        sys.exit(f"The shipped Rust builds fail the review policy: {error}")
    used = {(item.name, item.version, item.source) for item in found}
    problems += [f"{' '.join(crate)}: {error}" for crate, error in sorted(failures.items())]
    failing = dict.fromkeys(next(build for build, crates in trees.items() if (name, version) in crates)
                            for name, version, _ in failures)
    problems += [f"pending reviews: python3 -m scripts.legal.rust --package {package} --target {target} "
                 "--out rust/target/legal --review-template" for package, target in failing]
    problems += [f"the review of {item.name} {item.reviewedVersion} {item.upstream} covers no compiled crate"
                 for item in dead(parsed, used)] + over_budget(trees)
    if problems:
        sys.exit("The shipped Rust builds fail the review policy:\n" + "\n".join(f"  {item}" for item in problems))
    counts = ", ".join(f"{package} [{target}] {len(crates)}" for (package, target), crates in trees.items())
    print(f"{len(used)} shipped crates covered by approved reviews; compiled crate counts: {counts}")


if __name__ == "__main__":
    main()
