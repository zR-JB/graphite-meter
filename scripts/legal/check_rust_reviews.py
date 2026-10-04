"""Check review coverage for the shipped Rust targets.

Each supported Linux/Windows package and target is resolved as its release build is (cargo tree over normal
and build edges, with that target's features), so crates only a release build compiles, such as the Windows
TUI's, count as well. Registry reviews cover the same name and source across versions; the artifact collector
still checks actual license expressions, modifications and legal-file bytes. Git reviews bind the exact
version and revision. Unused approved reviews may remain. The Linux crate budget is a separate project
dependency policy, not a license requirement.

Every shipped target also needs an approved platform record in the --supplement file its builder
reads, so a release request cannot reach a target nobody reviewed. Local development needs no
platform approval and cannot produce distributable artifacts.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

from .model import marshal_reviews

REPO = Path(__file__).resolve().parents[2]
REVIEWS = REPO / 'legal/rust-reviewed-components.json'
Crate = tuple[str, str, str]
# Crates each static Linux binary compiles, its own and build-time crates included.
BUDGET = {('graphite-meter-server', 'x86_64-unknown-linux-musl'): 143,
          ('graphite-meter-client', 'x86_64-unknown-linux-musl'): 150}


def shipped(targets: str) -> list[tuple[str, str]]:
    """Rust ships Linux/Windows TUIs and Linux servers; Go retains the macOS targets."""
    pairs = []
    for platform, target in map(str.split, targets.splitlines()):
        if platform.startswith(('linux/', 'windows/')):
            pairs.append(('graphite-meter-client', target))
        if platform.startswith('linux/'):
            pairs.append(('graphite-meter-server', target))
    return pairs


def compiled(package: str, target: str) -> set[tuple[str, str]]:
    """The name and version of every crate a release build of `package` for `target` compiles."""
    tree = subprocess.run(
        ['cargo', 'tree', '--locked', '--edges', 'normal,build', '--target', target,
         '--prefix', 'none', '--format', '{p}', '--package', package],
        cwd=REPO / 'rust', check=True, stdout=subprocess.PIPE, text=True,
    ).stdout
    return {(name, version.removeprefix('v')) for name, version, *_ in map(str.split, tree.splitlines())}


def unreviewed_platforms(repo: Path, targets: str) -> list[str]:
    """Every shipped target needs an approved notice record in the pinned builder's supplement."""
    builder = 'container/Dockerfile.rust'
    supplements = set(re.findall(r'--supplement (legal/\S+\.json)', (repo / builder).read_text()))
    if len(supplements) != 1:
        return [f'{builder} must read exactly one --supplement file']
    name = supplements.pop()
    records = json.loads((repo / name).read_text()) if (repo / name).exists() else []
    approved = {record['target'] for record in records
                if record.get('reviewDecision') == 'approved' and record.get('reviewNotes')
                and re.fullmatch(r'[0-9a-f]{64}', record.get('noticesSha256', ''))}
    return [f'{name} has no approved record for {target}, which {builder} builds'
            for target in sorted({target for _, target in shipped(targets)} - approved)]


def unreviewed(reviews: list[dict], used: set[Crate]) -> list[Crate]:
    """The compiled identities without an approved registry family or exact Git review."""
    return sorted(crate for crate in used if not any(
        review.get('reviewDecision') == 'approved'
        and (review['name'], review['upstream']) == (crate[0], crate[2])
        and (crate[2].startswith('registry+') or review['reviewedVersion'] == crate[1])
        for review in reviews))


def over_budget(trees: dict[tuple[str, str], set[tuple[str, str]]]) -> list[str]:
    return [f'{package} compiles {len(trees.get((package, target), ()))} crates for {target}, budget {limit}'
            for (package, target), limit in BUDGET.items()
            if not 0 < len(trees.get((package, target), ())) <= limit]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--format', action='store_true', help='rewrite the reviews in the layout the legal tools write')
    args = parser.parse_args()
    reviews = json.loads(REVIEWS.read_text())
    if args.format:
        REVIEWS.write_bytes(marshal_reviews(reviews))
    elif REVIEWS.read_bytes() != marshal_reviews(reviews):
        sys.exit('legal/rust-reviewed-components.json is not in the layout the legal tools write (run with --format)')
    sources: dict[tuple[str, str], set[str]] = {}
    for package in tomllib.loads((REPO / 'rust/Cargo.lock').read_text())['package']:
        if 'source' in package:
            sources.setdefault((package['name'], package['version']), set()).add(package['source'])
    targets = (REPO / 'scripts/tui-targets.txt').read_text()
    if problems := unreviewed_platforms(REPO, targets):
        sys.exit('Rust platform records are missing:\n' + '\n'.join(f'  {problem}' for problem in problems))
    trees = {pair: compiled(*pair) for pair in shipped(targets)}
    # Workspace members carry no lock source and need no review.
    used = {(name, version, source) for crates in trees.values() for name, version in crates
            for source in sources.get((name, version), ())}
    problems = [f'no approved review names {name} {version} {source}, which a shipped binary compiles'
                for name, version, source in unreviewed(reviews, used)] + over_budget(trees)
    if problems:
        sys.exit('legal/rust-reviewed-components.json does not match the shipped Rust binaries:\n'
                 + '\n'.join(f'  {problem}' for problem in problems))
    counts = ', '.join(f'{package} [{target}] {len(crates)}' for (package, target), crates in trees.items())
    print(f'{len(used)} shipped crate identities covered by approved reviews; compiled crate counts: {counts}')


if __name__ == '__main__':
    main()
