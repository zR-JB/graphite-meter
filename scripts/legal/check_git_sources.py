#!/usr/bin/env python3
"""Keep Cargo git sources on reviewed fork commits.

Offline: every git package in rust/Cargo.lock is pinned by full-SHA rev to a
(fork, rev) entry in legal/rust-forks.json, and every entry is in use.
With --verify (network, release time): each rev is on its fork branch, the base
is the commit of the upstream tag and an ancestor of rev, the change set matches
diffSha256 and touches only reviewed package paths and workspace manifests, and the
commit subjects match the recorded list.
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

REPO = Path(__file__).resolve().parents[2]
SHA = re.compile(r'[0-9a-f]{40}')
REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index'
WORKSPACE_FILES = {'Cargo.toml', 'Cargo.lock'}
GIT_TIMEOUT = 600  # seconds, per network operation


def git(directory: Path, *args: str) -> str:
    return subprocess.run(['git', '-C', str(directory), *args], check=True, text=True,
                          stdout=subprocess.PIPE, timeout=GIT_TIMEOUT).stdout


def check_lock(forks: list[dict], lock: dict) -> list[str]:
    errors = []
    allowed = {(fork['fork'], fork['rev']) for fork in forks}
    used = set()
    for package in lock['package']:
        source = package.get('source')
        if source is None or source == REGISTRY:
            continue
        if not source.startswith('git+'):
            errors.append(f"{package['name']}: unexpected source {source}")
            continue
        parts = urlsplit(source[4:])
        url = parts._replace(query='', fragment='').geturl()
        query = parse_qs(parts.query)
        rev = query.get('rev', [''])[0]
        if (set(query) != {'rev'} or len(query['rev']) != 1
                or not SHA.fullmatch(rev) or parts.fragment != rev):
            errors.append(f"{package['name']}: git source must be pinned by full-SHA rev: {source}")
            continue
        if (url, rev) not in allowed:
            errors.append(f"{package['name']}: {url}@{rev} is not in legal/rust-forks.json")
        used.add((url, rev))
    errors += [f'unused legal/rust-forks.json entry: {url}@{rev}' for url, rev in sorted(allowed - used)]
    return errors


def ancestor(directory: Path, older: str, newer: str) -> bool:
    return subprocess.run(['git', '-C', str(directory), 'merge-base', '--is-ancestor', older, newer],
                          timeout=GIT_TIMEOUT).returncode == 0


def check_fork(fork: dict, directory: Path) -> list[str]:
    name, rev, base = fork['fork'], fork['rev'], fork['base']
    git(directory.parent, 'init', '-q', '--bare', str(directory))
    branch, tag = f"refs/heads/{fork['branch']}", f"refs/tags/{fork['baseTag']}"
    try:
        git(directory, 'fetch', '-q', fork['fork'], f'+{branch}:{branch}')
        git(directory, 'fetch', '-q', fork['upstream'], f'+{tag}:{tag}')
    except subprocess.CalledProcessError:
        return [f"{name}: branch {fork['branch']} or upstream tag {fork['baseTag']} is unavailable"]
    head = git(directory, 'rev-parse', branch).strip()
    tag = git(directory, 'rev-parse', tag + '^{commit}').strip()
    errors = []
    if tag != base:
        errors.append(f"{name}: upstream {fork['baseTag']} is {tag}, not base {base}")
    if subprocess.run(['git', '-C', str(directory), 'cat-file', '-e', rev + '^{commit}'],
                      stderr=subprocess.DEVNULL).returncode or not ancestor(directory, rev, head):
        return errors + [f"{name}: {rev} is not on branch {fork['branch']}"]
    if not ancestor(directory, base, rev):
        return errors + [f'{name}: base {base} is not an ancestor of {rev}']
    changes = git(directory, 'diff-tree', '-r', '--no-renames', '--full-index', base, rev)
    if hashlib.sha256(changes.encode()).hexdigest() != fork['diffSha256']:
        errors.append(f'{name}: change set differs from the reviewed diffSha256')
    for line in changes.splitlines():
        path = line.split('\t', 1)[1]
        if (path not in WORKSPACE_FILES and path not in fork.get('metadataFiles', [])
                and path not in fork.get('modifiedFiles', [])
                and path.split('/', 1)[0] not in fork['modifiedPackages']):
            errors.append(f'{name}: change outside modifiedPackages: {path}')
    subjects = git(directory, 'log', '--reverse', '--format=%s', f'{base}..{rev}').splitlines()
    if subjects != [commit['subject'] for commit in fork['commits']]:
        errors.append(f'{name}: commits differ from the recorded list: {subjects}')
    return errors


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--verify', action='store_true', help='fetch forks and upstreams and verify each change set')
    args = parser.parse_args()
    forks = json.loads((REPO / 'legal/rust-forks.json').read_text())
    errors = check_lock(forks, tomllib.loads((REPO / 'rust/Cargo.lock').read_text()))
    if args.verify:
        with tempfile.TemporaryDirectory() as scratch:
            for index, fork in enumerate(forks):
                errors += check_fork(fork, Path(scratch) / str(index))
    if errors:
        sys.exit('\n'.join(errors))
    print(f"{len(forks)} Cargo git sources pinned to reviewed fork commits{' and verified' if args.verify else ''}")


if __name__ == '__main__':
    main()
