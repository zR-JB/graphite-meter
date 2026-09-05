"""Run with python3 -m scripts.legal; public commands remain `mise run legal-*`."""
from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

from .artifacts import render, review_audit, review_template, third_party_source_bundle
from .discovery import discover_browser, discover_go
from .model import LegalError, Project, Provenance, Review, array, marshal, read_json
from .review import add_provenance, prepare_scopes, refresh_reviewed_versions


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mode', choices=('check', 'generate', 'review-template', 'review-audit', 'third-party-source-bundle'), default='check')
    parser.add_argument('--repo', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--browser-scan', type=Path, default=os.environ.get('GM_LEGAL_SCAN_MODULES'))
    parser.add_argument('--version', default=os.environ.get('VERSION') or 'development')
    parser.add_argument('--out', type=Path, default=os.environ.get('LEGAL_THIRD_PARTY_SOURCE_OUT'))
    args = parser.parse_args()
    repo: Path = args.repo.resolve()
    if args.browser_scan is None:
        parser.error('browser scan output is required; run the temporary production Vite scan first')
    project = Project.read(repo)
    reviews = [Review.parse(item) for item in array(read_json(repo / 'legal/reviewed-components.json'))]
    provenance = [Provenance.parse(item) for item in array(read_json(repo / 'legal/provenance.json'))]
    server, tui = discover_go(repo, reviews, provenance)
    scopes = {
        'server/browser': add_provenance(repo, server + discover_browser(args.browser_scan, reviews), provenance, 'server/browser'),
        'tui': add_provenance(repo, tui, provenance, 'tui'),
    }
    scopes['container'] = add_provenance(repo, scopes['server/browser'], provenance, 'container')
    prepare_scopes(scopes, reviews, args.mode)
    if args.mode == 'review-template':
        sys.stdout.buffer.write(review_template(scopes))
    elif args.mode == 'review-audit':
        sys.stdout.buffer.write(review_audit(scopes, reviews))
    elif args.mode == 'third-party-source-bundle':
        output = args.out or repo / 'go/dist' / f'graphite-meter_{args.version}_third-party-source.tar.gz'
        third_party_source_bundle(repo, project, args.version, scopes, provenance, output)
    else:
        files = render(repo, project, args.version, scopes)
        if args.mode == 'check':
            for relative, data in files.items():
                if (repo / relative).read_bytes() != data:
                    raise LegalError(f'generated file is stale: {relative}')
        else:
            refresh_reviewed_versions(scopes, reviews)
            files['legal/reviewed-components.json'] = marshal([review.json() for review in reviews])
            for relative, data in files.items():
                path = repo / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
        verb = 'check passed' if args.mode == 'check' else 'generated'
        print(f'legal {verb}: ' + ' '.join(f'{name}={len(items)}' for name, items in scopes.items()))


if __name__ == '__main__':
    try:
        main()
    except (LegalError, OSError, ValueError) as error:
        sys.exit(str(error))
