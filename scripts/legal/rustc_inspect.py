#!/usr/bin/env python3
"""Suppress codegen only for the explicitly selected legal-inspection library."""
from __future__ import annotations

import json
import os
import sys
from pathlib import Path

MARKER = '--cfg=graphite_meter_legal_inspection'


def inspection_arguments(arguments: list[str], package: str) -> list[str]:
    if MARKER not in arguments:
        return arguments
    if (arguments.count(MARKER) != 1 or '--test' in arguments
            or '--crate-name' not in arguments or '--crate-type' not in arguments
            or arguments[arguments.index('--crate-name') + 1] != package.replace('-', '_')
            or arguments[arguments.index('--crate-type') + 1] not in ('lib', 'rlib')):
        raise ValueError('legal inspection must select exactly the requested root library')
    rewritten = []
    tokens = iter(arguments)
    for argument in tokens:
        if argument == '--emit':
            next(tokens)
        elif argument != MARKER and not argument.startswith('--emit='):
            rewritten.append(argument)
    return [*rewritten, '--emit=dep-info,metadata']


def main() -> None:
    environment = dict(os.environ)
    previous = environment.pop('GM_RUST_INSPECT_WRAPPER', '')
    arguments = inspection_arguments(sys.argv[1:], environment.get('GM_RUST_INSPECT_PACKAGE', ''))
    if MARKER in sys.argv[1:] and (trace := environment.get('GM_RUST_BUILD_TRACE_DIR')):
        (Path(trace) / 'inspection-rustc.json').write_text(json.dumps(arguments))
    command = [previous, *arguments] if previous else arguments
    os.execvpe(command[0], command, environment)


if __name__ == '__main__':
    main()
