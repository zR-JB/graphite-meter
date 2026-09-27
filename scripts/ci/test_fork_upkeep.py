from __future__ import annotations

import contextlib
import io
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import fork_upkeep as upkeep


class ForkUpkeepBoundary(unittest.TestCase):
    def test_real_git_mirrors_proposals_conflicts_and_reruns(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'source'
            upkeep.git(root, 'init', '-q', '-b', 'main', str(source))
            upkeep.git(source, 'config', 'user.name', 'Fixture')
            upkeep.git(source, 'config', 'user.email', 'fixture@localhost')
            (source / 'patched').write_text('base\n')
            upkeep.git(source, 'add', '.')
            upkeep.git(source, 'commit', '-qm', 'Base')
            base = upkeep.git(source, 'rev-parse', 'HEAD')
            upstream = root / 'upstream.git'
            fork = root / 'fork.git'
            upkeep.git(root, 'clone', '-q', '--bare', str(source), str(upstream))
            upkeep.git(root, 'clone', '-q', '--bare', str(source), str(fork))
            upkeep.git(source, 'checkout', '-qb', 'protected-patch')
            (source / 'patched').write_text('patch\n')
            upkeep.git(source, 'commit', '-qam', 'Patch')
            canonical = upkeep.git(source, 'rev-parse', 'HEAD')
            upkeep.git(source, 'push', '-q', str(fork), 'HEAD:refs/heads/protected-patch')
            upkeep.git(source, 'checkout', '-q', 'main')
            (source / 'upstream-only').write_text('new\n')
            upkeep.git(source, 'add', '.')
            upkeep.git(source, 'commit', '-qm', 'Upstream advance')
            fresh = upkeep.git(source, 'rev-parse', 'HEAD')
            upkeep.git(source, 'push', '-q', str(upstream), 'main')
            entry = {'fork': str(fork), 'upstream': str(upstream), 'branch': 'protected-patch'}
            upkeep.git(source, 'checkout', '-qb', 'mirror-race', base)
            (source / 'raced').write_text('concurrent remote change\n')
            upkeep.git(source, 'add', '.')
            upkeep.git(source, 'commit', '-qm', 'Concurrent mirror update')
            raced = upkeep.git(source, 'rev-parse', 'HEAD')
            upkeep.git(source, 'checkout', '-q', 'main')
            real_git = upkeep.git

            def concurrent_push(directory: Path, *args: str) -> str:
                if args == ('push', str(fork), f'{fresh}:refs/heads/main'):
                    real_git(source, 'push', '-q', str(fork), f'{raced}:refs/heads/main')
                return real_git(directory, *args)

            with patch.object(upkeep, 'git', concurrent_push):
                with self.assertRaises(subprocess.CalledProcessError):
                    upkeep.carry(entry, root / 'race', True)
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'main'), raced)
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'protected-patch'), canonical)
            upkeep.git(fork, 'update-ref', 'refs/heads/main', base, raced)
            clean = upkeep.carry(entry, root / 'clean', True)
            self.assertEqual(clean['status'], 'proposal')
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'main'), fresh)
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'protected-patch'), canonical)
            proposed = upkeep.git(fork, 'rev-parse', 'protected-patch-next')
            self.assertTrue(upkeep.ancestor(fork, fresh, proposed))
            self.assertEqual(upkeep.git(fork, 'show', proposed + ':patched'), 'patch')
            self.assertEqual(upkeep.git(fork, 'show', proposed + ':upstream-only'), 'new')
            with patch.dict(os.environ, {'GIT_COMMITTER_DATE': '2001-01-01T00:00:00+0000'}):
                self.assertEqual(upkeep.carry(entry, root / 'rerun', True)['candidate'], proposed)
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'protected-patch-next'), proposed)

            upkeep.git(source, 'fetch', '-q', str(fork), 'protected-patch-next')
            upkeep.git(source, 'checkout', '-qb', 'reviewer-next', 'FETCH_HEAD')
            (source / 'reviewed').write_text('reviewer changes must survive\n')
            upkeep.git(source, 'add', '.')
            upkeep.git(source, 'commit', '-qm', 'Reviewer change')
            reviewed = upkeep.git(source, 'rev-parse', 'HEAD')
            upkeep.git(source, 'push', '-q', str(fork), 'HEAD:refs/heads/protected-patch-next')
            upkeep.git(source, 'checkout', '-q', 'main')
            (source / 'upstream-only').write_text('newer upstream\n')
            upkeep.git(source, 'commit', '-qam', 'Next upstream update')
            upkeep.git(source, 'push', '-q', str(upstream), 'main')
            self.assertEqual(upkeep.carry(entry, root / 'pending', True)['status'], 'pending-review')
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'protected-patch-next'), reviewed)
            self.assertEqual(upkeep.git(fork, 'show', reviewed + ':reviewed'), 'reviewer changes must survive')
            upkeep.git(source, 'checkout', '-q', 'protected-patch')
            upkeep.git(source, 'merge', '-q', '--no-edit', 'reviewer-next')
            canonical = upkeep.git(source, 'rev-parse', 'HEAD')
            upkeep.git(source, 'push', '-q', str(fork), 'HEAD:refs/heads/protected-patch')
            merged = upkeep.carry(entry, root / 'merged', True)
            self.assertEqual(merged['status'], 'proposal')
            proposed = merged['candidate']
            self.assertEqual(upkeep.git(fork, 'show', proposed + ':reviewed'), 'reviewer changes must survive')
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'protected-patch'), canonical)
            upkeep.git(source, 'checkout', '-q', 'main')
            (source / 'patched').write_text('upstream conflict\n')
            upkeep.git(source, 'commit', '-qam', 'Conflicting upstream change')
            upkeep.git(source, 'push', '-q', str(upstream), 'main')
            conflict = upkeep.carry(entry, root / 'conflict', True)
            self.assertEqual(conflict['status'], 'conflict')
            self.assertEqual(conflict['conflicts'], 'patched')
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'protected-patch-next'), proposed)
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'protected-patch'), canonical)
            upkeep.git(source, 'checkout', '-qb', 'mirror-divergence')
            (source / 'fork-only').write_text('must survive\n')
            upkeep.git(source, 'add', '.')
            upkeep.git(source, 'commit', '-qm', 'Diverged mirror')
            divergent = upkeep.git(source, 'rev-parse', 'HEAD')
            upkeep.git(source, 'push', '-q', str(fork), 'HEAD:refs/heads/main')
            self.assertEqual(upkeep.carry(entry, root / 'diverged', True)['status'], 'diverged')
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'main'), divergent)
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'protected-patch'), canonical)
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'protected-patch-next'), proposed)

            gh = root / 'gh'
            gh.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
state = Path(os.environ['FIXTURE_GH_STATE'])
data = json.loads(state.read_text()) if state.exists() else {'issues': [], 'prs': [], 'writes': 0}
args = sys.argv[1:]
collection = data['issues' if args[0] == 'issue' else 'prs']
if args[1] == 'list':
    print(json.dumps([item for item in collection if item['repo'] == args[args.index('--repo')+1] and ('--head' not in args or item['head'] == args[args.index('--head')+1])]))
elif args[1] == 'create':
    data['writes'] += 1
    collection.append({'state': 'OPEN', 'repo': args[args.index('--repo')+1], 'head': args[args.index('--head')+1] if '--head' in args else '', 'number': len(collection)+1, 'title': args[args.index('--title')+1], 'body': args[args.index('--body')+1]})
    state.write_text(json.dumps(data))
else:
    raise SystemExit('unexpected API mutation')
''')
            gh.chmod(0o755)
            state = root / 'api-state.json'
            with patch.dict(os.environ, {'PATH': str(root) + os.pathsep + os.environ['PATH'],
                                         'FIXTURE_GH_STATE': str(state)}):
                for _ in range(2):
                    upkeep.issue('fixture/fork', 'Conflict on protected-patch', conflict['conflicts'])
                    upkeep.pull_request('fixture/fork', 'protected-patch-next', 'protected-patch',
                                        'Carry patches', proposed)
            data = json.loads(state.read_text())
            self.assertEqual(data['writes'], 2)
            data['issues'][0]['state'] = 'CLOSED'
            state.write_text(json.dumps(data))
            with patch.dict(os.environ, {'PATH': str(root) + os.pathsep + os.environ['PATH'],
                                         'FIXTURE_GH_STATE': str(state)}):
                upkeep.issue('fixture/fork', 'Conflict on protected-patch', conflict['conflicts'])
            self.assertEqual(json.loads(state.read_text())['writes'], 2)
            project = root / 'project'
            upkeep.git(root, 'init', '-q', '-b', 'main', str(project))
            upkeep.git(project, 'config', 'user.name', 'Fixture')
            upkeep.git(project, 'config', 'user.email', 'fixture@localhost')
            (project / 'rust').mkdir()
            old_pin = 'a' * 40
            fork_url = 'https://github.com/fixture/fork'
            (project / 'rust/Cargo.toml').write_text(
                f'package = {{ git = "{fork_url}", rev = "{old_pin}" }}\n')
            (project / 'rust/Cargo.lock').write_text('reviewed lockfile\n')
            (project / 'legal').mkdir()
            (project / 'legal/rust-forks.json').write_text('reviewed source inventory\n')
            upkeep.git(project, 'add', '.')
            upkeep.git(project, 'commit', '-qm', 'Reviewed project')
            project_remote = root / 'project.git'
            upkeep.git(root, 'clone', '-q', '--bare', str(project), str(project_remote))
            config = root / 'gitconfig'
            config.write_text(f'[url "{project_remote}"]\n    insteadOf = https://github.com/fixture/project\n')
            with patch.dict(os.environ, {
                'PATH': str(root) + os.pathsep + os.environ['PATH'],
                'FIXTURE_GH_STATE': str(state), 'GIT_CONFIG_GLOBAL': str(config),
                'GIT_AUTHOR_NAME': 'Fixture', 'GIT_AUTHOR_EMAIL': 'fixture@localhost',
            }):
                for attempt in range(2):
                    upkeep.pin_proposal({'fork': fork_url, 'rev': old_pin, 'branch': 'protected-patch'},
                                        canonical, root / f'pin-{attempt}', 'fixture/project')
                    pin = upkeep.git(project_remote, 'rev-parse', 'fork-upkeep/fork-pin-' + canonical)
                    self.assertIn(canonical, upkeep.git(project_remote, 'show', pin + ':rust/Cargo.toml'))
                    self.assertEqual(upkeep.git(project_remote, 'show', pin + ':rust/Cargo.lock'),
                                     'reviewed lockfile' if attempt == 0 else 'reviewed new lockfile')
                    self.assertEqual(upkeep.git(project_remote, 'show', pin + ':legal/rust-forks.json'),
                                     'reviewed source inventory' if attempt == 0 else 'reviewed new provenance')
                    self.assertEqual(upkeep.git(project_remote, 'rev-parse', 'main'),
                                     upkeep.git(project, 'rev-parse', 'main'))
                    if attempt == 0:
                        reviewer = root / 'reviewer-pin'
                        upkeep.git(root, 'clone', '-q', '--branch', 'fork-upkeep/fork-pin-' + canonical,
                                   str(project_remote), str(reviewer))
                        upkeep.git(reviewer, 'config', 'user.name', 'Reviewer')
                        upkeep.git(reviewer, 'config', 'user.email', 'reviewer@localhost')
                        (reviewer / 'rust/Cargo.lock').write_text('reviewed new lockfile\n')
                        (reviewer / 'legal/rust-forks.json').write_text('reviewed new provenance\n')
                        upkeep.git(reviewer, 'commit', '-qam', 'Review candidate source')
                        upkeep.git(reviewer, 'push', '-q', 'origin', 'HEAD')
                        first_pin = upkeep.git(project_remote, 'rev-parse', 'fork-upkeep/fork-pin-' + canonical)
                    else:
                        self.assertEqual(pin, first_pin)
            self.assertEqual(json.loads(state.read_text())['writes'], 3)
            inventory = [
                {'fork': 'https://github.com/fixture/missing', 'upstream': 'https://github.com/fixture/upstream', 'branch': 'protected-patch'},
                {'fork': fork_url, 'upstream': 'https://github.com/fixture/upstream', 'branch': 'protected-patch'},
            ]
            (project / 'legal/rust-forks.json').write_text(json.dumps(inventory))
            with config.open('a') as settings:
                settings.write(f'[url "{fork}"]\n    insteadOf = {fork_url}\n'
                               f'[url "{upstream}"]\n    insteadOf = https://github.com/fixture/upstream\n'
                               f'[url "{root / "missing.git"}"]\n    insteadOf = https://github.com/fixture/missing\n')
            output, errors = io.StringIO(), io.StringIO()
            with patch.object(upkeep, 'ROOT', project), patch('sys.argv', ['fork-upkeep']), \
                    patch.dict(os.environ, {'GIT_CONFIG_GLOBAL': str(config)}), \
                    contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                with self.assertRaises(SystemExit):
                    upkeep.main()
            self.assertIn('entry 0: CalledProcessError', errors.getvalue())
            self.assertNotIn(str(root), errors.getvalue())
            self.assertIn('fixture/fork: diverged', output.getvalue())
            upstream_head = upkeep.git(upstream, 'rev-parse', 'main')
            upkeep.git(fork, 'update-ref', 'refs/heads/main', upstream_head, divergent)
            upkeep.git(source, 'checkout', '-q', 'protected-patch')
            upkeep.git(source, 'checkout', '-qb', 'merge-resolution')
            (source / 'merge-input').write_text('ordinary merge input\n')
            upkeep.git(source, 'add', '.')
            upkeep.git(source, 'commit', '-qm', 'Other reviewed branch')
            upkeep.git(source, 'checkout', '-q', 'protected-patch')
            upkeep.git(source, 'merge', '-q', '--no-ff', '--no-commit', 'merge-resolution')
            (source / 'merge-only').write_text('reviewed merge-only change\n')
            upkeep.git(source, 'add', '.')
            upkeep.git(source, 'commit', '-qm', 'Reviewed merge resolution')
            merged_owner = upkeep.git(source, 'rev-parse', 'HEAD')
            upkeep.git(source, 'push', '-q', str(fork), 'HEAD:refs/heads/protected-patch')
            resolution = upkeep.carry(entry, root / 'resolution', True)
            self.assertEqual(resolution['status'], 'conflict')
            self.assertIn(merged_owner, resolution['conflicts'])
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'protected-patch'), merged_owner)
            self.assertEqual(upkeep.git(fork, 'show', merged_owner + ':merge-only'),
                             'reviewed merge-only change')
            self.assertEqual(upkeep.git(fork, 'rev-parse', 'protected-patch-next'), proposed)





if __name__ == '__main__':
    unittest.main()
