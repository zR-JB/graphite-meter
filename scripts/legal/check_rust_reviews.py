"""Keep legal/rust-reviewed-components.json to exactly the crates the shipped Rust binaries compile.

    python3 -m scripts.legal.check_rust_reviews [--prune] [--format]

Each supported Linux/Windows package and target is resolved as its release build is (cargo tree over normal and
build edges, with that target's features), so crates only a release build compiles, such as the
Windows TUI's, count as well. Every such crate needs an approved review of its exact name,
version and source, and every review must name one of them; --prune drops the reviews that name
none. The file keeps the layout the legal tools write, so hand edits cannot drift; --format
rewrites it in that layout. The static Linux binaries may compile at most BUDGET crates.

Every shipped target also needs an approved platform record in the --supplement file its builder
reads, so a release request cannot reach a target nobody reviewed, and a release or host builder must read every
legal/rust-platform-*.json file.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

from ..ci.toolchains import rust_channel
from .model import marshal

REPO = Path(__file__).resolve().parents[2]
REVIEWS = REPO / 'legal/rust-reviewed-components.json'
Crate = tuple[str, str, str]
# Crates each static Linux binary compiles, its own and build-time crates included.
BUDGET = {('graphite-meter-server', 'x86_64-unknown-linux-musl'): 141,
          ('graphite-meter-client', 'x86_64-unknown-linux-musl'): 170}


def shipped(targets: str) -> list[tuple[str, str]]:
    """Rust ships Linux/Windows TUIs and Linux servers; Go retains the macOS targets."""
    pairs = []
    for line in targets.splitlines():
        platform, target = line.split()
        if not platform.startswith(('linux/', 'windows/')):
            continue
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


# The pinned builder image packages every distributed Rust target.
BUILDERS = {
    'container/Dockerfile.rust': lambda platform: platform.startswith(('linux/', 'windows/')),
}


def unreviewed_platforms(repo: Path, targets: str) -> list[str]:
    """Shipped targets whose builder's --supplement file holds no approved record for this toolchain,
    and platform record files that no builder reads."""
    channel = rust_channel(repo)
    problems, read = [], set()
    for builder, builds in BUILDERS.items():
        supplements = set(re.findall(r'--supplement (legal/\S+\.json)', (repo / builder).read_text()))
        read |= supplements
        if len(supplements) != 1:
            problems.append(f'{builder} must read exactly one --supplement file')
            continue
        name = supplements.pop()
        records = json.loads((repo / name).read_text()) if (repo / name).exists() else []
        approved = {record['target'] for record in records
                    if record.get('reviewDecision') == 'approved' and record.get('reviewNotes')
                    and f'\nrelease: {channel}\n' in record.get('rustc', '')}
        for line in targets.splitlines():
            platform, target = line.split()
            if builds(platform) and target not in approved:
                problems.append(f'{name} has no approved record for {target} on Rust {channel}, which {builder} builds')
    host_builds = '--host' in (mise := (repo / 'mise.toml').read_text()) and 'python3 -m scripts.legal.rust' in mise
    for path in sorted((repo / 'legal').glob('rust-platform-*.json')):
        name = path.relative_to(repo).as_posix()
        if name in read:
            continue
        records = json.loads(path.read_text())
        if not (host_builds and records and all(
                record.get('reviewDecision') == 'approved' and record.get('reviewNotes')
                and f"\nhost: {record.get('target')}\n" in record.get('rustc', '')
                and f'\nrelease: {channel}\n' in record.get('rustc', '') for record in records)):
            problems.append(f'{name} is read by no builder')
    return problems


def unused(reviews: list[dict], used: set[Crate]) -> list[dict]:
    return [review for review in reviews
            if (review['name'], review['reviewedVersion'], review['upstream']) not in used]


def unreviewed(reviews: list[dict], used: set[Crate]) -> list[Crate]:
    """The compiled crates that no approved review names exactly."""
    return sorted(used - {(review['name'], review['reviewedVersion'], review['upstream'])
                          for review in reviews if review.get('reviewDecision') == 'approved'})


def over_budget(trees: dict[tuple[str, str], set[tuple[str, str]]]) -> list[str]:
    return [f'{package} compiles {len(trees.get((package, target), ()))} crates for {target}, budget {limit}'
            for (package, target), limit in BUDGET.items()
            if not 0 < len(trees.get((package, target), ())) <= limit]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--prune', action='store_true', help='remove the reviews no shipped binary compiles')
    parser.add_argument('--format', action='store_true', help='rewrite the reviews in the layout the legal tools write')
    args = parser.parse_args()
    reviews = json.loads(REVIEWS.read_text())
    if args.format:
        REVIEWS.write_bytes(marshal(reviews))
    elif REVIEWS.read_bytes() != marshal(reviews):
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
    if stale := unused(reviews, used):
        if args.prune:
            REVIEWS.write_bytes(marshal([review for review in reviews if review not in stale]))
            print(f'pruned {len(stale)} of {len(reviews)} Rust legal reviews')
        else:
            problems += [f"{review['name']} {review['reviewedVersion']} {review['upstream']} is reviewed but no "
                         'shipped binary compiles it (run with --prune)' for review in stale]
    if problems:
        sys.exit('legal/rust-reviewed-components.json does not match the shipped Rust binaries:\n'
                 + '\n'.join(f'  {problem}' for problem in problems))
    budget = ', '.join(f'{package} {len(trees[package, target])}/{limit}' for (package, target), limit in BUDGET.items())
    print(f'{len(used)} Rust legal reviews, one for each crate a shipped binary compiles; '
          f'Linux crate budget: {budget}')


if __name__ == '__main__':
    main()
