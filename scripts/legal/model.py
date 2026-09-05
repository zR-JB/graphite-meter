"""Legal data and JSON boundaries; source paths never enter shipped inventories."""
from __future__ import annotations

import hashlib
import json
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import NoReturn, TypeAlias, cast

Json: TypeAlias = str | int | float | bool | None | list['Json'] | dict[str, 'Json']


class LegalError(ValueError):
    pass


def read_json(path: Path) -> Json:
    def record(items: list[tuple[str, Json]]) -> dict[str, Json]:
        result: dict[str, Json] = {}
        for key, value in items:
            if key in result:
                raise LegalError(f"duplicate JSON field {key!r} in {path}")
            result[key] = value
        return result

    def nonfinite(value: str) -> NoReturn:
        raise LegalError(f"invalid JSON number {value} in {path}")

    return cast(Json, json.loads(path.read_bytes(), object_pairs_hook=record, parse_constant=nonfinite))


def obj(value: Json) -> dict[str, Json]:
    if not isinstance(value, dict):
        raise LegalError('expected a JSON object')
    return value


def array(value: Json) -> list[Json]:
    if not isinstance(value, list):
        raise LegalError('expected a JSON array')
    return value


def string(value: Json) -> str:
    if not isinstance(value, str):
        raise LegalError('expected a JSON string')
    return value


def text(value: dict[str, Json], key: str) -> str:
    return string(value.get(key, ''))


def boolean(value: dict[str, Json], key: str) -> bool:
    item = value.get(key, False)
    if not isinstance(item, bool):
        raise LegalError(f'{key} must be a boolean')
    return item


def strings(value: dict[str, Json], key: str) -> list[str]:
    return [string(item) for item in array(value.get(key, []))]


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def marshal(value: object) -> bytes:
    # Callers provide ordered records. Unlike generic map serialization, record
    # field order is part of the existing generated-file contract.
    return (json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + '\n').encode()


@dataclass
class LegalFile:
    name: str = ''
    sha256: str = ''
    text: str = ''
    kind: str = ''

    @classmethod
    def parse(cls, value: Json) -> LegalFile:
        data = obj(value)
        return cls(*(text(data, key) for key in ('name', 'sha256', 'text', 'kind')))

    def json(self) -> dict[str, Json]:
        result: dict[str, Json] = {'name': self.name, 'sha256': self.sha256}
        if self.text:
            result['text'] = self.text
        if self.kind:
            result['kind'] = self.kind
        return result


@dataclass
class Project:
    schemaVersion: int = 0
    name: str = ''
    copyrightHolder: str = ''
    copyrightYears: str = ''
    licenseExpression: str = ''
    repository: str = ''

    @classmethod
    def read(cls, repo: Path) -> Project:
        data = obj(read_json(repo / 'legal/project.json'))
        schema = data.get('schemaVersion', 0)
        if not isinstance(schema, int) or isinstance(schema, bool):
            raise LegalError('schemaVersion must be an integer')
        project = cls(schema, *(text(data, key) for key in (
            'name', 'copyrightHolder', 'copyrightYears', 'licenseExpression', 'repository')))
        if not project.name or not project.repository or not project.licenseExpression:
            raise LegalError('legal/project.json is incomplete')
        return project

    def json(self) -> dict[str, Json]:
        return cast(dict[str, Json], asdict(self))


@dataclass
class Review:
    ecosystem: str = ''
    name: str = ''
    reviewedVersion: str = ''
    upstream: str = ''
    declaredLicenseExpression: str = ''
    selectedLicenseExpression: str = ''
    legalFiles: list[LegalFile] = field(default_factory=list[LegalFile])
    modified: bool = False
    artifactScopes: list[str] = field(default_factory=list[str])
    reviewDecision: str = ''
    reviewNotes: str = ''

    @classmethod
    def parse(cls, value: Json) -> Review:
        data = obj(value)
        return cls(
            text(data, 'ecosystem'), text(data, 'name'), text(data, 'reviewedVersion'), text(data, 'upstream'),
            text(data, 'declaredLicenseExpression'), text(data, 'selectedLicenseExpression'),
            [LegalFile.parse(item) for item in array(data.get('legalFiles', []))],
            boolean(data, 'modified'), strings(data, 'artifactScopes'),
            text(data, 'reviewDecision'), text(data, 'reviewNotes'),
        )

    def json(self) -> dict[str, Json]:
        result = cast(dict[str, Json], asdict(self))
        result['legalFiles'] = [item.json() for item in self.legalFiles]
        if not self.artifactScopes:
            del result['artifactScopes']
        return result


@dataclass
class LocalArtifact:
    path: str
    sha256: str


@dataclass
class Provenance:
    ecosystem: str = ''
    name: str = ''
    version: str = ''
    upstream: str = ''
    upstreamRevision: str = ''
    licenseExpression: str = ''
    modified: bool = False
    modificationNote: str = ''
    modificationDate: str = ''
    artifactScopes: list[str] = field(default_factory=list[str])
    localPaths: list[str] = field(default_factory=list[str])
    localArtifacts: list[LocalArtifact] = field(default_factory=list[LocalArtifact])
    localLegalFiles: list[LegalFile] = field(default_factory=list[LegalFile])
    correspondingSource: str = ''
    reviewNotes: str = ''

    @classmethod
    def parse(cls, value: Json) -> Provenance:
        data = obj(value)
        return cls(
            text(data, 'ecosystem'), text(data, 'name'), text(data, 'version'), text(data, 'upstream'),
            text(data, 'upstreamRevision'), text(data, 'licenseExpression'),
            boolean(data, 'modified'), text(data, 'modificationNote'), text(data, 'modificationDate'),
            strings(data, 'artifactScopes'), strings(data, 'localPaths'),
            [LocalArtifact(text(obj(item), 'path'), text(obj(item), 'sha256'))
                for item in array(data.get('localArtifacts', []))],
            [LegalFile.parse(item) for item in array(data.get('localLegalFiles', []))],
            text(data, 'correspondingSource'), text(data, 'reviewNotes'),
        )

    def json(self) -> dict[str, Json]:
        result = cast(dict[str, Json], asdict(self))
        result['localLegalFiles'] = [item.json() for item in self.localLegalFiles]
        for key in ('modificationNote', 'modificationDate', 'localArtifacts', 'correspondingSource'):
            if not result[key]:
                del result[key]
        return result


@dataclass
class Component:
    name: str = ''
    version: str = ''
    ecosystem: str = ''
    source: str = ''
    declaredLicenseExpression: str = ''
    selectedLicenseExpression: str = ''
    modified: bool = False
    legalTexts: list[LegalFile] = field(default_factory=list[LegalFile])
    notices: list[LegalFile] = field(default_factory=list[LegalFile])
    source_path: Path | None = None

    def json(self) -> dict[str, Json]:
        return {
            'name': self.name, 'version': self.version, 'ecosystem': self.ecosystem,
            'source': self.source, 'declaredLicenseExpression': self.declaredLicenseExpression,
            'selectedLicenseExpression': self.selectedLicenseExpression, 'modified': self.modified,
            'legalTexts': [item.json() for item in self.legalTexts],
            'notices': [item.json() for item in self.notices],
        }
