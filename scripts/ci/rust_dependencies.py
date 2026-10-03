"""Warm only locked third-party Cargo builds before Docker copies the application source."""
from __future__ import annotations

import argparse
import os
import subprocess
from pathlib import Path

MEMBERS = ('client', 'server', 'core', 'net', 'http3')


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--package', choices=('graphite-meter-client', 'graphite-meter-server'), required=True)
    parser.add_argument('--target', nargs='+', required=True)
    parser.add_argument('--profile', default='release')
    args = parser.parse_args()
    root = Path('/src/rust')
    for member in MEMBERS:
        source = root / member / 'src'
        source.mkdir(parents=True, exist_ok=True)
        (source / 'lib.rs').write_text('')
        if member in ('client', 'server'):
            (source / 'main.rs').write_text('fn main() {}\n')
            (root / member / 'build.rs').write_text('fn main() {}\n')
    environment = dict(os.environ)
    cargo_home = os.environ.get('CARGO_HOME') or str(Path.home() / '.cargo')
    environment['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join(
        f'--remap-path-prefix={path}={name}' for path, name in (('/src', '/src'), (cargo_home, '/cargo')))
    subprocess.run(['rustup', 'target', 'add', *args.target], cwd=root, check=True)
    # Both application packages give their bin and lib the same normal dependencies and have
    # no bin-only required features. Only dependencies survive: omit the empty binary's link.
    for target in args.target:
        subprocess.run(['cargo', 'build', '--locked', '--package', args.package, '--lib',
                        '--target', target, '--profile', args.profile], cwd=root, env=environment, check=True)
    # Real source and build scripts are copied next; none of these placeholders may enter the cache.
    subprocess.run(['cargo', 'clean', '--profile', args.profile,
                    *[f'--target={target}' for target in args.target],
                    *[f'--package=graphite-meter-{member}' for member in MEMBERS]],
                   cwd=root, check=True)


if __name__ == '__main__':
    main()
