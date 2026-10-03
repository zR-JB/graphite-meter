"""Local Rust builds: retain matching browser assets/scan and let Cargo own compilation freshness."""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

from .legal.rust import PACKAGES, write_changed
from .legal.model import marshal, sha256

ROOT = Path(__file__).resolve().parents[1]
PROFILES = ('dev', 'ci', 'release')
BROWSER_PROFILES = ('dev', 'prod')


def fingerprint(paths: list[Path], root: Path) -> dict[str, str]:
    return {str(path.relative_to(root)): sha256(path.read_bytes()) for path in sorted(set(paths)) if path.is_file()}


def browser(profile: str, environment: dict[str, str]) -> tuple[Path, Path]:
    client = ROOT / 'client'
    output = ROOT / 'rust/target/browser' / profile
    output.mkdir(parents=True, exist_ok=True)
    assets, scan, stamp = output / 'assets', output / 'modules.json', output / 'build.json'
    # Tracked and new source files, including deletions, but never ignored build products.
    # The browser check also includes Go's auth scripts and tests importing API fixtures.
    names = subprocess.check_output(['git', 'ls-files', '-z', '-c', '-o', '--exclude-standard',
                                     'client', 'go/internal/auth/assets', 'api', 'scripts/rust_build.py'],
                                    cwd=ROOT).decode().split('\0')
    paths = [ROOT / name for name in names if name] + list(client.glob('.env*'))
    revision = environment.get('GM_CLIENT_REVISION') or subprocess.check_output(
        ['git', 'rev-parse', '--short', 'HEAD'], cwd=ROOT, text=True).strip()
    environment = environment | {'GM_CLIENT_REVISION': revision, 'GM_CLIENT_BUILD_PROFILE': profile}
    if profile == 'dev':
        environment.pop('VERSION', None)
    inputs = {'files': fingerprint(paths, ROOT),
              'bun': subprocess.check_output(['bun', '--version'], text=True).strip(),
              'environment': {key: value for key, value in sorted(environment.items())
                              if key.startswith(('GM_CLIENT_', 'VITE_', 'BUN_', 'NODE_')) or key == 'VERSION'}}
    outputs = [scan, *assets.rglob('*')]
    try:
        previous = json.loads(stamp.read_bytes())
    except (OSError, ValueError):
        previous = {}
    if (assets / 'index.html').is_file() and scan.is_file() and previous == {
            'inputs': inputs, 'outputs': fingerprint(outputs, output)}:
        print(f'Browser [{profile}]: reuse assets and module scan', flush=True)
    else:
        stamp.unlink(missing_ok=True)
        # Vite does not empty an outDir outside client/. Remove only this profile's owned products.
        if assets.exists():
            shutil.rmtree(assets)
        scan.unlink(missing_ok=True)
        print(f'Browser [{profile}]: build assets and module scan', flush=True)
        # Vite confines scan output to TMPDIR. Both products live together, separate from Go's dist.
        subprocess.run(['bun', 'run', 'build'], cwd=client, check=True,
                       env=environment | {'TMPDIR': str(output), 'GM_LEGAL_SCAN_OUT': str(scan),
                                          'GM_LEGAL_SCAN_DIR': str(assets)})
        write_changed(stamp, marshal({'inputs': inputs, 'outputs': fingerprint([scan, *assets.rglob('*')], output)}))
    return assets, scan


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--package', choices=PACKAGES, required=True)
    parser.add_argument('--profile', choices=PROFILES, required=True)
    parser.add_argument('--browser', choices=BROWSER_PROFILES)
    args = parser.parse_args()
    # Construct commands from the allowlisted constants, not the supplied argument strings.
    package = PACKAGES[PACKAGES.index(args.package)]
    profile = PROFILES[PROFILES.index(args.profile)]
    environment = dict(os.environ, CARGO_TARGET_DIR=str(ROOT / 'rust/target'))
    command = [sys.executable, '-m', 'scripts.legal.rust', '--development', '--local', '--package', package,
               '--profile', profile, '--out', f'rust/target/dev-legal/{package}-{profile}',
               '--reviews', 'legal/rust-reviewed-components.json']
    if args.browser:
        assets, scan = browser(BROWSER_PROFILES[BROWSER_PROFILES.index(args.browser)], environment)
        environment['GM_RUST_ASSET_DIR'] = str(assets)
        command += ['--browser-scan', str(scan)]
    subprocess.run(command, cwd=ROOT, env=environment, check=True)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
