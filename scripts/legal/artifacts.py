"""Notices, the browser's legal index and deterministic archive members."""
from __future__ import annotations

import io
import tarfile
from pathlib import Path
from urllib.parse import parse_qs, quote, urlsplit, urlunsplit

from .model import Component, Json, LegalError, Project, array, marshal, obj, read_json, text

def notices(components: list[Component]) -> str:
    output = ["THIRD-PARTY SOFTWARE NOTICES\n\nGraphite Meter includes third-party software described below.\n\n"]
    for component in components:
        modified = "yes" if component.modified else "no"
        output.append(f'Component: {component.name}\nVersion: {component.version}\nSource: {component.source}\n'
                      f'License: {component.selectedLicenseExpression}\nModified by Graphite Meter: {modified}\n\n')
        output.extend(f'--- {item.name} ---\n\n{item.text}\n' for item in component.legalTexts + component.notices)
        output.append("============================================================\n\n")
    return "".join(output)


def copyright_notice(project: Project) -> str:
    return (f'{project.name}\nCopyright © {project.copyrightYears} {project.copyrightHolder}\n\n'
            f'{project.name} is free software licensed under {project.licenseExpression}.\n'
            "See LICENSE for the complete GNU Affero General Public License version 3 text.\n")


def legal_header(project: Project, source_url: str, license_text: bytes) -> bytes:
    """How a native binary's legal report starts: copyright, source and LICENSE."""
    header = copyright_notice(project).encode() + f"\nSource code: {source_url}\n\nLICENSE\n\n".encode() + license_text
    return header if license_text.endswith(b"\n") else header + b"\n"


def links(component: Component, forks: dict[tuple[str, str], dict[str, Json]], patches: dict[str, Json],
          blob: str) -> list[Json]:
    """A component's web pages: its source at the shipped version, and for a fork or a patched package its upstream
    and the project's changes. `source` stays the review identity, which for a crate is not a web address."""
    source, upstream, changes = component.source, "", ""
    if component.ecosystem == "cargo" and source.startswith("registry+"):
        source = f"https://crates.io/crates/{quote(component.name)}/{quote(component.version)}"
    elif source.startswith("git+"):
        location = urlsplit(source.removeprefix("git+"))
        repository = urlunsplit((location.scheme, location.netloc, location.path.removesuffix(".git"), "", ""))
        revision = location.fragment or parse_qs(location.query).get("rev", [""])[0]
        source = repository + (f"/tree/{quote(revision)}" if revision else "")
        if fork := forks.get((repository, revision)):
            upstream = f"{text(fork, 'upstream')}/tree/{text(fork, 'base')}"
            if component.modified:
                changes = f"{repository}/compare/{text(fork, 'base')}...{revision}"
    if component.modified and component.ecosystem == "npm":
        if patch := text(patches, f"{component.name}@{component.version}"):
            changes = f"{blob}/client/{quote(patch)}"
    return [{"label": label, "url": url} for label, url in (("Source", source), ("Upstream", upstream),
                                                            ("Changes", changes))
            if url.startswith(("https://", "http://"))]


def about(repo: Path, project: Project, version: str, source_url: str, components: list[Component]) -> bytes:
    """The browser's legal/about.json."""
    forks = {(text(fork, "fork"), text(fork, "rev")): fork
             for fork in map(obj, array(read_json(repo / "legal/rust-forks.json")))}
    # A patched bundled package links its patch, which the client's package.json names.
    patched = any(component.modified and component.ecosystem == "npm" for component in components)
    patches = obj(obj(read_json(repo / "client/package.json")).get("patchedDependencies", {})) if patched else {}
    # The client's patches as the named source holds them: at a release's tag, otherwise on the default branch.
    blob = source_url.replace("/tree/", "/blob/", 1) if "/tree/" in source_url else source_url + "/blob/HEAD"
    listed: list[Json] = []
    for component in components:
        value = component.json()
        del value["legalTexts"], value["notices"]
        value["links"] = links(component, forks, patches, blob)
        listed.append(value)
    return marshal({
        "schemaVersion": 2, "project": project.json(), "sourceVersion": version, "sourceURL": source_url,
        "licenseURL": "legal/LICENSE.txt", "noticesURL": "legal/THIRD_PARTY_NOTICES.txt", "components": listed,
    })


def add_bytes(archive: tarfile.TarFile, name: str, data: bytes) -> None:
    header = tarfile.TarInfo(name)
    header.mode = 0o644
    header.size = len(data)
    header.mtime = 0
    archive.addfile(header, io.BytesIO(data))


def add_tree(archive: tarfile.TarFile, root: Path, destination: str, boundary: Path) -> None:
    # Lexical depth-first order; file links are dereferenced only inside `boundary`.
    if root.is_symlink() and not root.resolve().is_relative_to(boundary.resolve()):
        raise LegalError(f"{root} links outside {boundary}")
    if root.is_symlink() or not root.is_dir():
        add_bytes(archive, destination, root.read_bytes())
        return
    for path in sorted(root.iterdir()):
        add_tree(archive, path, destination + "/" + path.name, boundary)
