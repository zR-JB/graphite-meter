"""The npm packages a browser build bundles, from its module scan."""

import os
from pathlib import Path

from .model import Component, Json, LegalError, Review, array, obj, read_json, string, text
from .review import component_from_files, component_legal_files, sort_components


def package_root(module_id: str) -> Path:
    path = Path(module_id.removeprefix("\\x00")).absolute()
    for directory in (path, *path.parents):
        if (directory / "package.json").is_file():
            return directory
    raise LegalError(f"bundled browser module has no package.json root: {module_id}")


def package_license(value: Json, licenses: Json, fallback: str) -> str:
    if isinstance(value, str) and value:
        return value
    for item in array(licenses):
        if license_type := text(obj(item), "type"):
            return license_type
    return fallback


def repository_url(value: Json) -> str:
    url = value if isinstance(value, str) else text(value, "url") if isinstance(value, dict) else ""
    return url.removeprefix("git+").removesuffix(".git")


def npm_component(root: Path, reviews: list[Review]) -> Component:
    metadata = obj(read_json(root / "package.json"))
    name = text(metadata, "name")
    files = component_legal_files(root, "npm", name, reviews)
    source = repository_url(metadata.get("repository"))
    component = component_from_files("npm", name, text(metadata, "version"), source, files)
    component.source_path = root
    # Bun patches are package-manager provenance even when emitted modules resolve through an isolated store.
    for parent in root.parents:
        manifest = parent / "package.json"
        if "node_modules" in parent.parts or not manifest.is_file():
            continue
        patches = obj(read_json(manifest)).get("patchedDependencies", {})
        component.modified = f"{name}@{component.version}" in obj(patches)
        break
    component.declaredLicenseExpression = package_license(
        metadata.get("license"), metadata.get("licenses", []), component.declaredLicenseExpression)
    component.selectedLicenseExpression = component.declaredLicenseExpression
    return component


def discover_browser(scan: Path, reviews: list[Review]) -> list[Component]:
    ids = {string(item) for item in array(read_json(scan))}
    roots = {package_root(module_id) for module_id in ids if "/node_modules/" in module_id.replace(os.sep, "/")}
    return sort_components([npm_component(root, reviews) for root in sorted(roots)])
