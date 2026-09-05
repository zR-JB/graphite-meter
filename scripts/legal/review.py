"""Explicit approval and fingerprint policy, independent of package managers."""
from __future__ import annotations

import os
from dataclasses import replace
from pathlib import Path

from .model import Component, LegalError, LegalFile, Provenance, Review, sha256

CANDIDATE_PREFIXES = ('LICENSE', 'LICENCE', 'COPYING', 'NOTICE', 'COPYRIGHT', 'PATENTS', 'THIRD_PARTY')


def file_kind(name: str) -> str:
    return 'notice' if Path(name).name.upper().startswith(('NOTICE', 'COPYRIGHT', 'PATENTS', 'THIRD_PARTY')) else 'license'


def read_legal_files(root: Path, *, recursive: bool = True) -> list[LegalFile]:
    candidates: list[Path] = []
    if recursive:
        def fail(error: OSError) -> None:
            raise error
        for directory, _, names in os.walk(root, onerror=fail):
            candidates.extend(Path(directory) / name for name in names)
    else:
        candidates = [path for path in root.iterdir() if not path.is_dir()]
    files: list[LegalFile] = []
    for path in candidates:
        upper = path.name.upper()
        if not any(upper == prefix or upper.startswith(tuple(prefix + c for c in '.-_'))
                   for prefix in CANDIDATE_PREFIXES):
            continue
        data = path.read_bytes()
        files.append(LegalFile(path.relative_to(root).as_posix(), sha256(data), data.decode(), file_kind(path.name)))
    if not files:
        raise LegalError(f'no legal candidate file in {root}')
    return sorted(files, key=lambda item: item.name.lower())


def find_review(ecosystem: str, name: str, reviews: list[Review]) -> Review | None:
    return next((review for review in reviews if (review.ecosystem, review.name) == (ecosystem, name)), None)


def validate_review(component: Component, reviews: list[Review]) -> None:
    expression = component.declaredLicenseExpression.upper()
    if not expression or 'UNKNOWN' in expression or 'NOASSERTION' in expression or expression == 'UNLICENSED':
        raise LegalError(f'component {component.name} has missing or unknown licensing information')
    # Match the original approval lookup: the last record for an identity wins.
    review = {(item.ecosystem, item.name): item for item in reviews}.get((component.ecosystem, component.name))
    if review is None:
        raise LegalError(f'LEGAL REVIEW REQUIRED: new component has no review record: {component.name}')
    if review.reviewDecision != 'approved':
        raise LegalError(f'LEGAL REVIEW REQUIRED: review for {component.name} is unresolved')
    if review.declaredLicenseExpression != component.declaredLicenseExpression or not review.selectedLicenseExpression:
        raise LegalError(f'LEGAL REVIEW REQUIRED: declared license changed for {component.name}')
    if component.modified != review.modified:
        raise LegalError(f'LEGAL REVIEW REQUIRED: modification status changed for {component.name}')
    current_files = component.legalTexts + component.notices
    current = {item.name.lower(): item.sha256 for item in current_files}
    reviewed = {item.name.lower(): item.sha256 for item in review.legalFiles}
    names: dict[str, str] = {}
    for item in current_files + review.legalFiles:
        names.setdefault(item.name.lower(), item.name)
    for name in sorted(current.keys() | reviewed.keys()):
        expected, actual = reviewed.get(name, '<missing>'), current.get(name, '<missing>')
        if expected != actual:
            raise LegalError(f'LEGAL REVIEW REQUIRED: legal fingerprint changed for {component.name}:\n'
                             f'  {names[name]}\n  expected: {expected}\n  actual:   {actual}')


def reviewed_legal_files(root: Path, ecosystem: str, name: str, reviews: list[Review]) -> list[LegalFile]:
    review = find_review(ecosystem, name, reviews)
    if review is None or not review.legalFiles:
        raise LegalError('no explicit reviewed legal-file override')
    files: list[LegalFile] = []
    for item in review.legalFiles:
        relative = Path(os.path.normpath(item.name))
        if relative.is_absolute() or str(relative) == '.' or '..' in relative.parts:
            raise LegalError(f'reviewed legal path must stay inside component: {item.name}')
        try:
            data = (root / relative).read_bytes()
        except FileNotFoundError:
            # Bun's isolated linker keeps dependencies beside the package root.
            if relative.parts[0] != 'node_modules':
                raise
            data = (root.parent / Path(*relative.parts[1:])).read_bytes()
        files.append(replace(item, sha256=sha256(data), text=data.decode(), kind=item.kind or file_kind(item.name)))
    return files


def component_legal_files(root: Path, ecosystem: str, name: str, reviews: list[Review]) -> list[LegalFile]:
    try:
        discovered = read_legal_files(root)
    except LegalError:
        if find_review(ecosystem, name, reviews) is None:
            raise
        return reviewed_legal_files(root, ecosystem, name, reviews)
    review = find_review(ecosystem, name, reviews)
    if review is None:
        return discovered
    files = reviewed_legal_files(root, ecosystem, name, reviews)
    reviewed_names = {item.name.lower() for item in review.legalFiles}
    for item in discovered:
        if item.name.lower() not in reviewed_names:
            raise LegalError(f'LEGAL REVIEW REQUIRED: new legal file for {name}: {item.name}')
    return files


def infer_license(files: list[LegalFile]) -> str:
    for item in files:
        upper = item.text.upper()
        for expression, phrases in (
            ('Apache-2.0', ('APACHE LICENSE',)),
            ('MIT', ('MIT LICENSE', 'PERMISSION IS HEREBY GRANTED, FREE OF CHARGE')),
            ('ISC', ('PERMISSION TO USE, COPY, MODIFY, AND DISTRIBUTE', 'ISC LICENSE')),
            ('BSD-3-Clause', ('BSD 3-CLAUSE', 'REDISTRIBUTION AND USE IN SOURCE AND BINARY FORMS')),
            ('BSD-2-Clause', ('BSD 2-CLAUSE',)),
        ):
            if any(phrase in upper for phrase in phrases):
                return expression
    return 'UNKNOWN'


def component_from_files(ecosystem: str, name: str, version: str, source: str, files: list[LegalFile]) -> Component:
    expression = infer_license(files)
    return Component(name, version, ecosystem, source, expression, expression,
                     legalTexts=[item for item in files if item.kind != 'notice'],
                     notices=[item for item in files if item.kind == 'notice'])


def component_key(component: Component) -> tuple[str, str, str]:
    return component.ecosystem, component.name, component.version


def sort_components(components: list[Component]) -> list[Component]:
    unique = {component_key(item): item for item in components}
    return [unique[key] for key in sorted(unique)]


def add_provenance(repo: Path, components: list[Component], entries: list[Provenance], scope: str) -> list[Component]:
    result = list(components)
    for entry in entries:
        if scope not in entry.artifactScopes and not (scope == 'server/browser' and 'server' in entry.artifactScopes):
            continue
        if (not entry.name or not entry.version or not entry.licenseExpression
                or 'UNKNOWN' in entry.licenseExpression.upper() or not entry.reviewNotes):
            raise LegalError(f'provenance entry {entry.name!r} is incomplete or unresolved')

        def read(path: Path, display: str, expected: str, label: str) -> bytes:
            try:
                data = path.read_bytes()
            except OSError as error:
                raise LegalError(f'provenance {entry.name} {label} {display}: {error}') from error
            if expected != sha256(data):
                raise LegalError(f'provenance {entry.name} {label} hash changed: {display}')
            return data

        for artifact in entry.localArtifacts:
            if not artifact.path or not artifact.sha256:
                raise LegalError(f'provenance {entry.name} has an incomplete local artifact')
            read(repo / artifact.path, artifact.path, artifact.sha256, 'local artifact')
        files = [replace(item, name=Path(item.name).name,
                         text=read(repo / item.name, item.name, item.sha256, 'legal file').decode(),
                         kind=item.kind or 'license') for item in entry.localLegalFiles]
        component = component_from_files(entry.ecosystem, entry.name, entry.version, entry.upstream, files)
        component.declaredLicenseExpression = component.selectedLicenseExpression = entry.licenseExpression
        component.modified = entry.modified
        result.append(component)
    return sort_components(result)


def prepare_scopes(scopes: dict[str, list[Component]], reviews: list[Review], mode: str) -> None:
    for scope, components in scopes.items():
        for component in components:
            try:
                validate_review(component, reviews)
            except LegalError as error:
                if mode not in ('review-template', 'review-audit'):
                    raise LegalError(f'{scope}: {error}') from error
        scopes[scope] = [replace(component, selectedLicenseExpression=review.selectedLicenseExpression)
                         if (review := find_review(component.ecosystem, component.name, reviews)) else replace(component)
                         for component in components]


def refresh_reviewed_versions(scopes: dict[str, list[Component]], reviews: list[Review]) -> None:
    versions = {(item.ecosystem, item.name): item.version for items in scopes.values() for item in items}
    for review in reviews:
        if version := versions.get((review.ecosystem, review.name)):
            review.reviewedVersion = version
