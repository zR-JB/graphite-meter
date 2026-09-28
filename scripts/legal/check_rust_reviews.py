"""Keep legal/rust-reviewed-components.json to the crates the shipped Rust binaries compile.

    python3 -m scripts.legal.check_rust_reviews [--prune]

Each shipped package and target is resolved as its release build is (cargo tree over normal and
build edges, with that target's features), and every review must name one of those crates by
exact name, version and source. --prune drops the reviews that name none.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tomllib
from pathlib import Path

from .model import marshal

REPO = Path(__file__).resolve().parents[2]
REVIEWS = REPO / 'legal/rust-reviewed-components.json'
Crate = tuple[str, str, str]


def shipped(targets: str) -> list[tuple[str, str]]:
    """The TUI ships every listed target; the server image ships the Linux ones on glibc."""
    pairs = []
    for line in targets.splitlines():
        platform, target = line.split()
        pairs.append(('graphite-meter-client', target))
        if platform.startswith('linux/'):
            pairs.append(('graphite-meter-server', target.removesuffix('musl') + 'gnu'))
    return pairs


def compiled(package: str, target: str, sources: dict[tuple[str, str], set[str]]) -> set[Crate]:
    tree = subprocess.run(
        ['cargo', 'tree', '--locked', '--edges', 'normal,build', '--target', target,
         '--prefix', 'none', '--format', '{p}', '--package', package],
        cwd=REPO / 'rust', check=True, stdout=subprocess.PIPE, text=True,
    ).stdout
    crates = {(name, version.removeprefix('v')) for name, version, *_ in map(str.split, tree.splitlines())}
    # Workspace members carry no lock source and need no review.
    return {(name, version, source) for name, version in crates for source in sources.get((name, version), ())}


def unused(reviews: list[dict], used: set[Crate]) -> list[dict]:
    return [review for review in reviews
            if (review['name'], review['reviewedVersion'], review['upstream']) not in used]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--prune', action='store_true', help='remove the reviews no shipped binary compiles')
    args = parser.parse_args()
    sources: dict[tuple[str, str], set[str]] = {}
    for package in tomllib.loads((REPO / 'rust/Cargo.lock').read_text())['package']:
        if 'source' in package:
            sources.setdefault((package['name'], package['version']), set()).add(package['source'])
    used: set[Crate] = set()
    for package, target in shipped((REPO / 'scripts/tui-targets.txt').read_text()):
        used |= compiled(package, target, sources)
    reviews = json.loads(REVIEWS.read_text())
    stale = unused(reviews, used)
    if stale and args.prune:
        REVIEWS.write_bytes(marshal([review for review in reviews if review not in stale]))
        print(f'pruned {len(stale)} of {len(reviews)} Rust legal reviews')
    elif stale:
        sys.exit('legal/rust-reviewed-components.json reviews crates no shipped Rust binary compiles '
                 '(run with --prune):\n' + '\n'.join(
                     f"  {review['name']} {review['reviewedVersion']} {review['upstream']}" for review in stale))
    else:
        print(f'{len(reviews)} Rust legal reviews, each for a crate a shipped binary compiles')


if __name__ == '__main__':
    main()
