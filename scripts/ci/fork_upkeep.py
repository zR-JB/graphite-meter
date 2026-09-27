#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RELEASE = re.compile(r'(.*?)(\d+)\.(\d+)\.(\d+)')


def git(directory: Path, *args: str) -> str:
    return subprocess.run(['git', '-C', str(directory), *args], check=True, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=600).stdout.strip()


def gh(*args: str) -> str:
    return subprocess.run(['gh', *args], check=True, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=600).stdout.strip()


def ancestor(directory: Path, older: str, newer: str) -> bool:
    result = subprocess.run(['git', '-C', str(directory), 'merge-base', '--is-ancestor', older, newer],
                            check=False, timeout=60)
    if result.returncode not in (0, 1):
        raise RuntimeError('cannot establish Git ancestry')
    return result.returncode == 0


def default_branch(url: str) -> str:
    refs = git(ROOT, 'ls-remote', '--symref', url, 'HEAD')
    for line in refs.splitlines():
        if line.startswith('ref: refs/heads/') and line.endswith('\tHEAD'):
            return line.split('\t')[0].removeprefix('ref: refs/heads/')
    raise RuntimeError(f'{url}: default branch is unavailable')


def latest_release(fork: dict) -> str:
    base = RELEASE.fullmatch(fork['baseTag'])
    if not base:
        raise ValueError(f"{fork['baseTag']} is not a stable release tag")
    releases = {}
    for line in git(ROOT, 'ls-remote', '--tags', '--refs', fork['upstream']).splitlines():
        tag = line.split('\t')[1].removeprefix('refs/tags/')
        if (match := RELEASE.fullmatch(tag)) and match[1] == base[1]:
            releases[tuple(map(int, match.group(2, 3, 4)))] = tag
    return releases[max(releases)]


def carry(fork: dict, directory: Path, publish: bool) -> dict:
    git(directory.parent, 'init', '-q', str(directory))
    git(directory, 'config', 'user.name', os.environ.get('GIT_AUTHOR_NAME', 'Fork upkeep'))
    git(directory, 'config', 'user.email', os.environ.get('GIT_AUTHOR_EMAIL', 'fork-upkeep@localhost'))
    mirror = default_branch(fork['fork'])
    upstream = default_branch(fork['upstream'])
    release = latest_release(fork)
    patch = fork['branch']
    next_branch = patch + '-next'
    git(directory, 'fetch', '-q', fork['fork'],
        f'refs/heads/{mirror}:refs/remotes/fork/mirror',
        f'refs/heads/{patch}:refs/remotes/fork/patch')
    git(directory, 'fetch', '-q', fork['upstream'],
        f'refs/heads/{upstream}:refs/remotes/upstream/default',
        f'refs/tags/{release}:refs/tags/{release}')
    old_mirror = git(directory, 'rev-parse', 'refs/remotes/fork/mirror')
    fresh = git(directory, 'rev-parse', 'refs/remotes/upstream/default')
    target = git(directory, 'rev-parse', f'refs/tags/{release}^{{commit}}')
    current = git(directory, 'rev-parse', 'refs/remotes/fork/patch')
    result = {'patch': current, 'upstream': release, 'next': next_branch}
    if not ancestor(directory, old_mirror, fresh):
        return result | {'status': 'diverged'}
    if publish and old_mirror != fresh:
        git(directory, 'push', fork['fork'], f'{fresh}:refs/heads/{mirror}')
    if ancestor(directory, target, current):
        return result | {'status': 'current'}
    base = git(directory, 'merge-base', current, target)
    for merge in git(directory, 'rev-list', '--merges', '--parents', f'{base}..{current}').splitlines():
        parents = merge.split()
        if len(parents) != 3 or git(directory, 'show', '--remerge-diff', '--format=', parents[0]):
            return result | {'status': 'conflict', 'conflicts': f'Merge {parents[0]} needs manual carry-forward review.'}
    git(directory, 'checkout', '-q', '-b', 'proposal', current)
    try:
        git(directory, 'rebase', '--committer-date-is-author-date', '--onto', target, base)
    except subprocess.CalledProcessError:
        conflicts = git(directory, 'diff', '--name-only', '--diff-filter=U')
        git(directory, 'rebase', '--abort')
        if not conflicts:
            raise
        return result | {'status': 'conflict', 'conflicts': conflicts}
    candidate = git(directory, 'rev-parse', 'HEAD')
    if candidate == target:
        return result | {'status': 'upstreamed'}
    existing = git(ROOT, 'ls-remote', fork['fork'], f'refs/heads/{next_branch}')
    expected = existing.split()[0] if existing else ''
    if expected and expected != candidate:
        return result | {'status': 'pending-review', 'candidate': expected}
    if publish and not expected:
        git(directory, 'push', f'--force-with-lease=refs/heads/{next_branch}:',
            fork['fork'], f'{candidate}:refs/heads/{next_branch}')
    return result | {'status': 'proposal', 'candidate': candidate}


def github_repo(url: str) -> str:
    match = re.fullmatch(r'https://github.com/([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)', url)
    if not match:
        raise ValueError(f'not a GitHub repository: {url}')
    return match[1]


def issue(repo: str, title: str, body: str) -> None:
    existing = json.loads(gh('issue', 'list', '--repo', repo, '--state', 'all',
                              '--search', title + ' in:title', '--json', 'number,title,body,state'))
    for item in existing:
        if item['title'] == title:
            if item['body'] == body:
                return
            if item['state'] != 'OPEN':
                continue
            if item['body'] != body:
                gh('issue', 'edit', str(item['number']), '--repo', repo, '--body', body)
            return
    gh('issue', 'create', '--repo', repo, '--title', title, '--body', body)


def pull_request(repo: str, branch: str, base: str, title: str, body: str) -> None:
    existing = json.loads(gh('pr', 'list', '--repo', repo, '--state', 'all',
                              '--head', branch, '--base', base, '--json', 'number,title,body'))
    if not existing:
        gh('pr', 'create', '--repo', repo, '--head', branch, '--base', base, '--draft',
           '--title', title, '--body', body)


def pin_proposal(fork: dict, current: str, directory: Path, repository: str) -> None:
    if current == fork['rev']:
        return
    branch = 'fork-upkeep/' + github_repo(fork['fork']).split('/')[1] + '-pin-' + current
    url = 'https://github.com/' + repository
    base = default_branch(url)
    if not git(ROOT, 'ls-remote', url, f'refs/heads/{branch}'):
        git(directory.parent, 'clone', '-q', '--no-checkout', '--single-branch', '--branch', base,
            url, str(directory))
        git(directory, 'checkout', '-q', '-b', branch)
        manifest = directory / 'rust/Cargo.toml'
        text = manifest.read_text()
        pattern = re.compile(r'(git\s*=\s*"' + re.escape(fork['fork'])
                             + r'"\s*,\s*rev\s*=\s*")' + re.escape(fork['rev']) + '"')
        updated, count = pattern.subn(lambda match: match[1] + current + '"', text)
        if not count:
            raise RuntimeError(f"{fork['fork']}: reviewed pin absent from current default branch")
        manifest.write_text(updated)
        git(directory, 'config', 'user.name', os.environ['GIT_AUTHOR_NAME'])
        git(directory, 'config', 'user.email', os.environ['GIT_AUTHOR_EMAIL'])
        git(directory, 'add', 'rust/Cargo.toml')
        git(directory, 'commit', '-q', '-m', f"Propose {github_repo(fork['fork'])} pin for source review")
        git(directory, 'push', url, f'HEAD:refs/heads/{branch}')
    pull_request(repository, branch, base, f"Review {github_repo(fork['fork'])} pin {current[:12]}",
                 f"Candidate canonical patch commit: `{current}` on `{fork['branch']}`.\n\n"
                 "Pending source, license and dependency review. This draft changes only Cargo.toml. "
                 "Cargo.lock and legal/rust-forks.json remain at their reviewed values, so existing "
                 "locked-build and provenance gates intentionally block this proposal. Review the "
                 "upstream base, patch origins, diff, modified package/file scope and license inventory; "
                 "then update the reviewed provenance and lockfile and run legal-check, "
                 "scripts/legal/check_git_sources.py --verify and the Rust/fork gates before approval.")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument('--publish', choices=('forks', 'pins'))
    parser.add_argument('--repositories', action='store_true')
    args = parser.parse_args()
    if args.repositories and not all((ROOT / name).is_file()
                                     for name in ('rust/Cargo.toml', 'legal/rust-forks.json')):
        return
    forks = json.loads((ROOT / 'legal/rust-forks.json').read_text())
    repository = os.environ.get('GITHUB_REPOSITORY', '')
    if args.repositories:
        print(','.join(github_repo(fork['fork']).split('/')[1] for fork in forks))
        return
    if args.publish:
        owner = github_repo('https://github.com/' + repository).split('/')[0]
        if any(github_repo(fork['fork']).split('/')[0] != owner for fork in forks):
            raise SystemExit('all forks must belong to this repository owner')
        bot = os.environ['APP_SLUG'] + '[bot]'
        os.environ['GIT_AUTHOR_NAME'] = bot
        os.environ['GIT_AUTHOR_EMAIL'] = f"{gh('api', 'users/' + bot, '--jq', '.id')}+{bot}@users.noreply.github.com"
    failed = False
    with tempfile.TemporaryDirectory() as scratch:
        for index, fork in enumerate(forks):
            try:
                repo = github_repo(fork['fork'])
                github_repo(fork['upstream'])
                if args.publish == 'pins':
                    current = git(ROOT, 'ls-remote', '--exit-code', fork['fork'],
                                  f"refs/heads/{fork['branch']}").split()[0]
                    pin_proposal(fork, current, Path(scratch) / f'pin-{index}', repository)
                    continue
                result = carry(fork, Path(scratch) / str(index), args.publish == 'forks')
                status = result['status']
                print(f'{repo}: {status} ({result["upstream"]})')
                failed |= status in ('diverged', 'conflict')
                if args.publish and status != 'current':
                    body = (f'Canonical patch: `{result["patch"]}`\n'
                            f'Latest stable upstream release: `{result["upstream"]}`\n\n')
                    if 'candidate' in result:
                        body += (f'Review `{result["candidate"]}` on `{result["next"]}`. Once reviewed, push it as '
                                 'the new protected `graphite-meter/<crate>-v<version>` branch and update '
                                 'legal/rust-forks.json; upkeep never replaces a pending proposal.')
                    else:
                        body += result.get('conflicts', 'Review this state manually; no patch proposal was pushed.')
                    issue(repo, f'Fork upkeep: {status} on {fork["branch"]}', body)
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                failed = True
                print(f'fork inventory entry {index}: {type(error).__name__}', file=sys.stderr)
    if failed:
        raise SystemExit('fork upkeep needs owner review')


if __name__ == '__main__':
    main()
