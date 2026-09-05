"""Prepare pinned Python development tools or run the offline strict type gate."""
from __future__ import annotations

import argparse
import hashlib
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ENVIRONMENT = ROOT / '.tools/python'
PYTHON = ENVIRONMENT / ('Scripts/python.exe' if os.name == 'nt' else 'bin/python')
REQUIREMENTS = ROOT / 'scripts/requirements-dev.txt'
MARKER = ENVIRONMENT / '.requirements-sha256'


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('setup', 'check'))
    args = parser.parse_args()
    digest = hashlib.sha256(REQUIREMENTS.read_bytes()).hexdigest()
    prepared = PYTHON.is_file() and MARKER.is_file() and MARKER.read_text() == digest
    if args.command == 'setup':
        if not prepared:
            subprocess.run([sys.executable, '-m', 'venv', str(ENVIRONMENT)], check=True)
            subprocess.run([str(PYTHON), '-m', 'pip', 'install', '--disable-pip-version-check',
                            '--require-hashes', '-r', str(REQUIREMENTS)], check=True)
            MARKER.write_text(digest)
    elif not prepared:
        parser.error('pinned Python tools are missing or stale; run just python-setup')
    else:
        subprocess.run([str(PYTHON), '-m', 'mypy', '--config-file', str(ROOT / 'mypy.ini')], cwd=ROOT, check=True)


if __name__ == '__main__':
    main()
