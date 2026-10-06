"""The crates a Rust binary build compiles and their reviewed notices, from Cargo's locked metadata."""
from __future__ import annotations

import json
import subprocess
from dataclasses import replace
from pathlib import Path
from typing import Any

from ..ci.rust_workspace import ROOT
from .check_rust_reviews import compiled as tree
from .model import Component, LegalError, Provenance, Review, array, obj, read_json, strings, text
from .review import add_provenance, component_legal_files, validate_review

Metadata = dict[str, Any]
Units = dict[str, list[Any]]  # the compiled targets and native libraries of one package
Crate = tuple[str, str, str]  # name, version, Cargo source


def cargo_metadata(host: str, target: str) -> Metadata:
    """Locked metadata of every package a build for `target` may compile, host build dependencies included."""
    filters = [f"--filter-platform={platform}" for platform in dict.fromkeys((target, host))]
    return json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version=1", *filters],
                                              cwd=ROOT / "rust", text=True))


def selected(metadata: Metadata, crates: set[tuple[str, str]]) -> set[str]:
    """The package ids of `crates`, each a name and version that must name exactly one package."""
    ids: dict[tuple[str, str], list[str]] = {}
    for item in metadata["packages"]:
        ids.setdefault((item["name"], item["version"]), []).append(item["id"])
    if unresolved := sorted(crate for crate in crates if len(ids.get(crate, [])) != 1):
        raise LegalError(f"Cargo package identity is missing or ambiguous: {unresolved}")
    return {ids[crate][0] for crate in crates}


def prepared(metadata: Metadata, package: str, host: str, target: str) -> set[str]:
    """Every package a build of `package` for `target` may compile: Cargo's trees for the target and the host."""
    return selected(metadata, set().union(*(tree(package, platform) for platform in dict.fromkeys((target, host)))))


def compiled(messages: list[dict[str, Any]], root: str, binary: str) -> dict[str, Units]:
    """The packages a successful `cargo rustc --message-format=json` build of `binary` compiled."""
    if not messages or messages[-1] != {"reason": "build-finished", "success": True}:
        raise LegalError("Cargo did not report a successful completed build")
    collected: dict[str, Units] = {}
    executable = False
    for message in messages:
        reason = message.get("reason")
        if reason not in ("compiler-artifact", "build-script-executed"):
            continue
        entry = collected.setdefault(message["package_id"], {"units": [], "nativeLibraries": []})
        if reason == "build-script-executed":
            entry["nativeLibraries"] = sorted(set(entry["nativeLibraries"]) | set(message["linked_libs"]))
            continue
        target = message["target"]
        if message["profile"]["test"] or {"test", "bench", "example"} & set(target["kind"]):
            raise LegalError("the notice build includes tests, examples or benchmarks")
        unit = {"name": target["name"], "kinds": sorted(target["kind"]), "features": sorted(message["features"])}
        if unit not in entry["units"]:
            entry["units"].append(unit)
        if message["package_id"] == root and target["name"] == binary and target["kind"] == ["bin"]:
            executable = bool(message["executable"])
    if not executable:
        raise LegalError("Cargo did not produce the requested executable")
    for entry in collected.values():
        entry["units"].sort(key=lambda unit: (unit["name"], unit["kinds"], unit["features"]))
    return collected


def components(metadata: Metadata, builds: dict[str, Units], reviews: list[Review], provenance: list[Provenance]
               ) -> tuple[list[Component], list[dict[str, Any]], dict[Crate, str]]:
    """The third-party components of `builds` with their inventory entries, and why any of them lacks review."""
    packages = {item["id"]: item for item in metadata["packages"]}
    forks = {(text(fork, "fork"), text(fork, "rev")): strings(fork, "modifiedPackages")
             for fork in map(obj, array(read_json(ROOT / "legal/rust-forks.json")))}
    found, inventory, failures = [], [], {}
    for identity in sorted(builds.keys() - set(metadata["workspace_members"])):
        item = packages.get(identity)
        if item is None:
            raise LegalError(f"compiled package is absent from the locked metadata: {identity}")
        directory = Path(item["manifest_path"]).resolve().parent
        source = item["source"]
        if source is None:
            raise LegalError(f"unrecognized local dependency: {directory}")
        modified, revision = False, ""
        if source.startswith("git+"):
            location, _, pinned = source.removeprefix("git+").partition("?rev=")
            revision = pinned.partition("#")[0]
            if (location, revision) not in forks:
                raise LegalError(f"unreviewed Cargo git source: {identity}")
            modified = item["name"] in forks[location, revision]
        elif (vcs := directory / ".cargo_vcs_info.json").exists():
            revision = json.loads(vcs.read_text()).get("git", {}).get("sha1", "")
        component = Component(item["name"], item["version"], "cargo", source, item.get("license") or "",
                              modified=modified, source_path=directory)
        try:
            review(component, directory, item.get("license_file"), reviews, provenance)
        except LegalError as error:
            failures[component.name, component.version, source] = str(error)
        found.append(component)
        inventory.append({"component": component.json(), "repository": item.get("repository") or "",
                          "upstreamRevision": revision, **builds[identity]})
    return found, inventory, failures


def review(component: Component, directory: Path, license_file: str | None, reviews: list[Review],
           provenance: list[Provenance]) -> None:
    """Fill `component`'s legal files from `directory` or manual provenance and check them against its review.

    A registry review covers its crate across versions, a git review only its exact version; an exact one wins."""
    matching = [item for item in reviews if (item.ecosystem, item.name, item.upstream) == ("cargo", component.name,
                                                                                           component.source)
                and (component.source.startswith("registry+") or item.reviewedVersion == component.version)]
    matching = [item for item in matching if item.reviewedVersion == component.version] or matching
    if len(matching) > 1:
        raise LegalError("ambiguous review")
    manual = [entry for entry in provenance if (entry.ecosystem, entry.name, entry.version, entry.upstream)
              == ("cargo", component.name, component.version, component.source)]
    if len(manual) > 1:
        raise LegalError("duplicate manual provenance")
    if manual:
        entry = add_provenance(ROOT, [], manual, "rust")[0]
        if (entry.declaredLicenseExpression, entry.modified) != (component.declaredLicenseExpression,
                                                                 component.modified):
            raise LegalError("manual provenance disagrees with the package")
        files = entry.legalTexts + entry.notices
    else:
        files = component_legal_files(directory, "cargo", component.name, matching)
    # A manifest may name a license file outside the conventional names.
    if license_file:
        named = (directory / license_file).resolve()
        if not named.is_relative_to(directory) or named.relative_to(directory).as_posix() not in {
                file.name for file in files}:
            raise LegalError(f"license_file needs explicit review: {license_file}")
    component.legalTexts = [file for file in files if file.kind != "notice"]
    component.notices = [file for file in files if file.kind == "notice"]
    validate_review(component, matching)
    component.selectedLicenseExpression = matching[0].selectedLicenseExpression


def candidates(found: list[Component], failures: dict[Crate, str]) -> list[dict[str, Any]]:
    """Pending reviews of the components that lack one, with their legal-file fingerprints."""
    return [Review("cargo", item.name, item.version, item.source, item.declaredLicenseExpression,
                   legalFiles=[replace(file, text="") for file in item.legalTexts + item.notices],
                   modified=item.modified, reviewDecision="pending").json()
            for item in found if (item.name, item.version, item.source) in failures]
