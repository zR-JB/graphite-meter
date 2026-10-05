"""Reviewed native platform inputs that Cargo's package graph does not show.

A record lists the linked inputs beyond the target's sysroot rlibs (`nativeInputs`), the system libraries the
executable imports and the notice texts beyond the Rust standard library's (`notices`); the rlibs and the
standard-library texts are found in the sysroot by rule. `noticesSha256` hashes the canonical listing of every
notice text; the pinned builder image and toolchain establish binary provenance separately.
"""
from __future__ import annotations

import os
import re
import subprocess
import urllib.request
from pathlib import Path

from ..ci.github_api import confined_path
from ..ci.toolchains import rust_channel
from .model import Json, LegalError, array, obj, read_json, sha256, string, strings, text

SYSROOT = "$RUST_SYSROOT/"
IMPORTS = {
    "elf": (["readelf", "--dynamic"], r"\(NEEDED\).*\[([^]]+)\]"),
    "pe": (["objdump", "-p"], r"DLL Name: (\S+)"),
}
MAX_NOTICE_BYTES = 1_000_000


def link_map(output: Path, target: str, profile: str) -> Path:
    """The linker map of `target`, which stays inside `output` whatever the target names."""
    return confined_path(output / f"{target}-{profile}.map", output)


def linked(link_map: Path, sysroot: Path, cargo: set[Path]) -> set[str]:
    """Objects and archives outside Cargo's directories with a linked archive(member) in a GNU ld or LLD map."""
    listing = link_map.read_text(errors="replace")
    paths = re.findall(r"(/[^\s():]+\.o)\b", listing) + re.findall(r"(/[^\s():]+)\([^\s()]+\)", listing)
    normalized = {Path(os.path.normpath(path)) for path in paths}
    return {SYSROOT + path.relative_to(sysroot).as_posix() if path.is_relative_to(sysroot) else str(path)
            for path in normalized if not any(path.is_relative_to(directory) for directory in cargo)}


def imports(executable: Path, target: str) -> set[str]:
    """The shared libraries `executable` names, lowercased for Windows DLLs."""
    kind = "pe" if "-windows-" in target else "elf"
    command, pattern = IMPORTS[kind]
    names = re.findall(pattern, subprocess.check_output([*command, str(executable)], text=True))
    return {name.lower() for name in names} if kind == "pe" else set(names)


def record(path: Path, target: str) -> dict[str, Json] | None:
    matching = [entry for entry in map(obj, array(read_json(path))) if entry.get("target") == target]
    if len(matching) > 1:
        raise LegalError(f"duplicate Rust platform record for {target}")
    return matching[0] if matching else None


def source(path: str, sysroot: Path) -> Path:
    return sysroot / path.removeprefix(SYSROOT) if path.startswith(SYSROOT) else Path(path)


def rlibs(sysroot: Path, target: str) -> set[str]:
    return {SYSROOT + path.relative_to(sysroot).as_posix()
            for path in (sysroot / "lib/rustlib" / target / "lib").glob("*.rlib")}


def own_notices(entry: dict[str, Json] | None) -> dict[str, str]:
    return {path: string(name) for path, name in obj((entry or {}).get("notices", {})).items()}


def check_notice_sources(repo: Path) -> dict[str, dict[str, Json]]:
    """legal/rust-notice-sources.json: HTTPS sources by SHA-256 for files under legal/manual/, reviewed for the
    pinned Rust release."""
    resources = obj(read_json(repo / "legal/rust-notice-sources.json"))
    if resources.get("rustVersion") != (version := rust_channel(repo)):
        raise LegalError(f"legal/rust-notice-sources.json needs review for Rust {version}")
    sources = {}
    for relative, value in resources.items():
        if relative == "rustVersion":
            continue
        resource = obj(value)
        if (not relative.startswith("legal/manual/") or ".." in Path(relative).parts
                or not text(resource, "url").startswith("https://")
                or re.fullmatch(r"[0-9a-f]{64}", text(resource, "sha256")) is None):
            raise LegalError(f"notice source {relative} must be a legal/manual/ path with an HTTPS URL and SHA-256")
        sources[relative] = resource
    return sources


def fetch_notices(repo: Path, entry: dict[str, Json]) -> None:
    """Materialize this platform's notices that the builder lacks, checking downloaded and cached bytes."""
    sources = check_notice_sources(repo)
    for relative in own_notices(entry).keys() & sources.keys():
        path = confined_path(repo / relative, repo / "legal/manual")
        if path.exists():
            data = path.read_bytes()
        else:
            with urllib.request.urlopen(text(sources[relative], "url"), timeout=30) as response:
                data = response.read(MAX_NOTICE_BYTES + 1)
        if len(data) > MAX_NOTICE_BYTES or sha256(data) != text(sources[relative], "sha256"):
            raise LegalError(f"runtime notice bytes differ from the reviewed source: {relative}")
        if not path.exists():
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)


def notice_names(entry: dict[str, Json] | None, sysroot: Path) -> dict[str, str]:
    """Every notice text in notice order: the Rust standard library's, named by rule, then the record's."""
    documents = sysroot / "share/doc/rust"
    names = {SYSROOT + path.relative_to(sysroot).as_posix(): "rust-standard-library/" + path.name
             for path in [documents / "COPYRIGHT-library.html", *sorted((documents / "licenses").iterdir())]}
    listed = own_notices(entry)
    if names.keys() & listed.keys() or not all(listed.values()):
        raise LegalError("record notices must name texts beyond the Rust standard library, each with a name")
    return names | listed


def fingerprint(paths: set[str], sysroot: Path) -> tuple[str, dict[str, bytes]]:
    """The notice listing, `path<TAB>sha256<LF>` sorted by path, and the exact texts to embed."""
    kept = {path: source(path, sysroot).read_bytes() for path in sorted(paths)}
    return "".join(f"{path}\t{sha256(data)}\n" for path, data in kept.items()), kept


def notice(entry: dict[str, Json] | None, *, target: str, sysroot: Path, inputs: set[str],
           libraries: set[str]) -> str:
    """The platform notices of a build that linked `inputs` and imports `libraries`, under the approved record."""
    if (entry is None or entry.get("target") != target or entry.get("reviewDecision") != "approved"
            or not entry.get("reviewNotes")):
        raise LegalError(f"Rust platform review for {target} is absent or unresolved")
    if unreviewed := sorted(inputs - rlibs(sysroot, target) - set(strings(entry, "nativeInputs"))):
        raise LegalError(f"linked native inputs lack review: {unreviewed}")
    if unreviewed := sorted(libraries - set(strings(entry, "systemLibraries"))):
        raise LegalError(f"imported system libraries lack review: {unreviewed}")
    names = notice_names(entry, sysroot)
    listing, texts = fingerprint(set(names), sysroot)
    if (digest := sha256(listing.encode())) != (reviewed := text(entry, "noticesSha256")):
        raise LegalError(f"reviewed Rust platform notices changed: noticesSha256 is {digest}, "
                         f"reviewed {reviewed or 'none'}")
    return text(entry, "description") + "\n" + "".join(
        f"\n--- {name} ---\n\n" + texts[path].decode() for path, name in names.items())


def candidate(entry: dict[str, Json] | None, *, target: str, sysroot: Path, inputs: set[str],
              libraries: set[str]) -> tuple[dict[str, object], str]:
    """This build's unreviewed record and notice listing. It keeps reviewed native inputs this build did not link
    while they exist, such as import libraries only unoptimized builds take."""
    native = (inputs - rlibs(sysroot, target)
              | {path for path in strings(entry or {}, "nativeInputs") if source(path, sysroot).exists()})
    listing, _ = fingerprint(set(notice_names(entry, sysroot)), sysroot)
    return {
        "target": target, "systemLibraries": sorted(libraries), "reviewDecision": "pending", "reviewNotes": "",
        "description": text(entry or {}, "description"), "nativeInputs": sorted(native),
        "notices": own_notices(entry), "noticesSha256": sha256(listing.encode()),
    }, listing
