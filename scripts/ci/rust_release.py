#!/usr/bin/env python3
"""Stage the Rust image, its server source offers and the Rust TUI archives, and verify them as a release does.

    python3 scripts/ci/rust_release.py stage-image   # VERSION, OCI_ARCHIVE, SERVER_EXPORT, OUT_DIR
    python3 scripts/ci/rust_release.py stage-tui     # VERSION, TUI_EXPORT, OUT_DIR
    python3 scripts/ci/rust_release.py check         # VERSION, REVISION, REPOSITORY, SKOPEO_IMAGE, IMAGE_DIR,
                                                     # TUI_DIR, SERVER_PROFILE

Staging copies exactly the expected files out of BuildKit exports. Verification reads the staged files as data and
never runs them: the image as verify_oci does, each source offer against this checkout, each TUI archive's layout,
executable format and notices, and each SOURCE.txt against the offer it names. A prerelease, as Go's, ships only the
image, whose SOURCE.txt names the repository. CI stages and checks its own builds as a release request stages and the
release verifies them. Every path lies inside RUNNER_TEMP.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import shutil
import tarfile
import tempfile
import zipfile
from pathlib import Path

import verify_oci
from github_api import (
    TLS_NAME, ControlPlaneError, JsonObject, confined_path, decode_json, expect_array, expect_object, fail,
    file_sha256, int_field, object_field, runner_path, str_field,
)
from rust_workspace import ROOT, load, offer_name, source_url, tui_archive
from trust import SEMVER_NUMBER, env, env_sha, exact_files
from verify_release_assets import TUI_FILES, archive_names, require_same

N = SEMVER_NUMBER
VERSION = re.compile(rf"{N}\.{N}\.{N}(?:-[0-9A-Za-z]+(?:\.[0-9A-Za-z]+)*)?")
PRERELEASE = re.compile(rf"{N}\.{N}\.{N}-(?:alpha|beta|rc)\.{N}")
OCI = "graphite-meter-rust.oci.tar"
# The Dockerfile and target that build the Rust image.
IMAGE_STAGE = ("container/Dockerfile.rust", "server")
OCI_LIMIT = 1024 * 1024 * 1024
FILE_LIMIT = 16 * 1024 * 1024
EXECUTABLE_LIMIT = 128 * 1024 * 1024
SERVER, CLIENT = "graphite-meter-server", "graphite-meter-client"
PROFILES = ("ci", "release")
# A reviewed build's executable carries its notices' SHA-256 after this (rust/legal/src/build.rs).
REVIEWED = "graphite-meter reviewed notices sha256:"
# ELF e_machine and PE machine of each target's architecture.
MACHINES = {"x86_64": (62, 0x8664), "aarch64": (183, 0xAA64)}


def release_version(value: str) -> str:
    if VERSION.fullmatch(value) is None:
        fail(f"{value!r} is not a release version")
    return value


def prerelease(version: str) -> bool:
    return PRERELEASE.fullmatch(version) is not None


def server_offers(version: str) -> dict[str, tuple[str, str]]:
    """Each server source offer with its image platform and Rust target; a prerelease, as Go's, publishes none."""
    if prerelease(version):
        return {}
    return {offer_name(SERVER, version, platform): (platform, target) for platform, target in load().server.items()}


def image_files(version: str) -> set[str]:
    return {OCI, f"{OCI}.sha256", *server_offers(version)}


def tui_files(version: str) -> set[str]:
    """The archive and source offer of each TUI platform of the workspace; macOS has none."""
    return {name for platform in load().tui for name in (tui_archive(version, platform)[0],
                                                         offer_name(CLIENT, version, platform))}


def stage(export: Path, out: Path, names: set[str]) -> None:
    """Copy exactly `names` out of a BuildKit export, wherever its platform directories hold them."""
    found: dict[str, Path] = {}
    for path in sorted(export.rglob("*")):
        if path.name in names and not path.is_symlink() and path.is_file():
            if path.name in found:
                fail(f"the export holds {path.name} twice")
            found[path.name] = path
    if missing := sorted(names - found.keys()):
        fail(f"the export lacks {missing}")
    out.mkdir(parents=True, exist_ok=True)
    for name, path in found.items():
        shutil.copyfile(confined_path(path, export), confined_path(out / name, out))


def read(path: Path, name: str, limit: int = FILE_LIMIT) -> bytes:
    """One regular member of a .tar.gz or .zip archive, bounded by `limit`."""
    try:
        if path.suffix == ".zip":
            with zipfile.ZipFile(path) as archive:
                info = archive.getinfo(name)
                if info.is_dir() or info.file_size > limit:
                    fail(f"{path.name}: {name} is not a bounded regular file")
                return archive.read(info)
        with tarfile.open(path, "r:gz") as archive:
            entry = archive.getmember(name)
            handle = archive.extractfile(entry) if entry.isfile() and entry.size <= limit else None
            if handle is None:
                fail(f"{path.name}: {name} is not a bounded regular file")
            return handle.read()
    except (KeyError, OSError, tarfile.TarError, zipfile.BadZipFile) as exc:
        raise ControlPlaneError(f"cannot read {name} from {path.name}: {exc}") from exc


def text(path: Path, name: str) -> str:
    try:
        return read(path, name).decode()
    except UnicodeDecodeError as exc:
        raise ControlPlaneError(f"{path.name}: {name} is not UTF-8") from exc


def build_source(version: str, root: Path = ROOT) -> str:
    """The source a Rust build of `version` names, as the collector does: legal/project.json's repository, at the
    release tag of a stable version."""
    project = expect_object(decode_json((root / "legal/project.json").read_text(encoding="utf-8"), "project.json"),
                            "project.json")
    return source_url(str_field(project, "repository", "project.json"), version)


def check_source_txt(content: str, where: str, version: str, offer: str, target: str, root: Path = ROOT) -> None:
    """A SOURCE.txt names the release source, the release, the source offer beside it and its Rust target."""
    expected = (f"Graphite Meter source: {build_source(version, root)}\nMatching release: v{version}\n"
                f"Dependency source archive: {offer}\nRust target: {target}\n")
    if content != expected:
        fail(f"{where} SOURCE.txt does not name {offer} for {target} at {build_source(version, root)}")


def verify_offer(path: Path, package: str, target: str, profile: str, root: Path = ROOT) -> str:
    """Check a source offer and return its notices. Under its own directory, as Go's, it holds the reviewed notices
    and inventory of a `profile` build of `package` for `target` against this checkout's Cargo.lock, the source of
    each inventoried crate and, for the server, of each inventoried browser package, and otherwise only files equal
    to this checkout's."""
    top = path.name.removesuffix(".tar.gz")
    names = archive_names(path)
    if outside := sorted(name for name in names if name != top and not name.startswith(top + "/")):
        fail(f"{path.name} holds files outside {top}/: {outside[:5]}")
    inventory = expect_object(decode_json(text(path, f"{top}/inventory.json"), path.name), path.name)
    identity = {"package": package, "target": target, "profile": profile,
                "cargoLockSha256": file_sha256(root / "rust/Cargo.lock")}
    if int_field(inventory, "schemaVersion", path.name) != 1 or any(
            inventory.get(key) != value for key, value in identity.items()):
        fail(f"{path.name} does not inventory a {profile} build of {package} for {target} from this Cargo.lock")

    def tree(ecosystem: str, component: JsonObject) -> str:
        return (f"{top}/third_party/{ecosystem}/{str_field(component, 'name', path.name)}-"
                f"{str_field(component, 'version', path.name)}/")

    trees = {tree("cargo", object_field(expect_object(item, path.name), "component", path.name))
             for item in expect_array(inventory.get("components"), f"{path.name} components")}
    browser = {tree("npm", expect_object(item, path.name))
               for item in expect_array(inventory.get("browser"), f"{path.name} browser")}
    if (package == SERVER) != bool(browser):
        fail(f"{path.name} must inventory browser packages exactly when it offers the server's source")
    trees |= browser
    try:
        with tarfile.open(path, "r:gz") as archive:
            files = [entry.name for entry in archive if not entry.isdir()]
    except (OSError, tarfile.TarError) as exc:
        raise ControlPlaneError(f"cannot read {path.name}: {exc}") from exc
    for name in files:
        relative = name.removeprefix(top + "/")
        if name.startswith(tuple(trees)) or relative in ("inventory.json", "LEGAL.txt"):
            continue
        if TLS_NAME.search(relative):
            fail(f"{path.name} holds certificate or key material outside dependency source: {relative}")
        local = confined_path(root / relative, root)
        if not local.is_file() or read(path, name) != local.read_bytes():
            fail(f"{path.name} holds {relative}, which is neither dependency source nor this checkout's file")
    if missing := sorted(tree for tree in trees if not any(name.startswith(tree) for name in files)):
        fail(f"{path.name} lacks the source of {missing[:5]}")
    notices = text(path, f"{top}/LEGAL.txt")
    if not notices.strip() or verify_oci.DEVELOPMENT in notices:
        fail(f"{path.name} lacks reviewed notices")
    return notices


def reviewed_digests(executable: bytes) -> set[str]:
    """The notices' SHA-256 digests that `executable` names as a reviewed build's."""
    return {digest.decode() for digest in re.findall(re.escape(REVIEWED.encode()) + rb"([0-9a-f]{64})", executable)}


def native_executable(data: bytes, target: str) -> bool:
    """Whether `data` is a 64-bit little-endian executable for `target`: PE for Windows, otherwise ELF."""
    elf, pe = MACHINES.get(target.split("-", 1)[0], (0, 0))

    def field(offset: int, size: int) -> int:
        return int.from_bytes(data[offset:offset + size], "little")

    if "-windows-" in target:
        start = field(0x3C, 4)
        return data[:2] == b"MZ" and data[start:start + 4] == b"PE\0\0" and field(start + 4, 2) == pe
    return data[:6] == b"\x7fELF\x02\x01" and field(16, 2) in (2, 3) and field(18, 2) == elf


def verify_tui(directory: Path, version: str, platform: str, target: str, root: Path = ROOT) -> None:
    """A TUI archive holds exactly Go's layout: a reviewed executable for `target`, this checkout's license
    files, its offer's notices and the SOURCE.txt that names that offer."""
    archive, base, binary = tui_archive(version, platform)
    offer = offer_name(CLIENT, version, platform)
    path = directory / archive
    require_same(archive, {f"{base}/{name}" for name in (binary, *TUI_FILES)}, archive_names(path) - {base})
    executable = read(path, f"{base}/{binary}", EXECUTABLE_LIMIT)
    if not native_executable(executable, target):
        fail(f"{archive} does not hold a {target} executable")
    if verify_oci.DEVELOPMENT.encode() in executable:
        fail(f"{archive} holds an unreviewed development build")
    for name in ("LICENSE", "COPYRIGHT"):
        if read(path, f"{base}/{name}") != (root / name).read_bytes():
            fail(f"{archive}: {name} differs from this checkout's")
    notices = verify_offer(directory / offer, CLIENT, target, "release", root)
    if text(path, f"{base}/THIRD_PARTY_NOTICES.txt") != notices:
        fail(f"{archive}'s notices differ from its source offer's")
    if reviewed_digests(executable) != {hashlib.sha256(notices.encode()).hexdigest()}:
        fail(f"{archive} does not hold the reviewed build of its notices")
    check_source_txt(text(path, f"{base}/SOURCE.txt"), archive, version, offer, target, root)


def verify_image(directory: Path, version: str, revision: str, profile: str, root: Path = ROOT) -> tuple[str, str]:
    """Verify the staged image and its server source offers; return the archive's SHA-256 and manifest digest."""
    path = directory / OCI
    if path.stat().st_size > OCI_LIMIT:
        fail(f"{OCI} exceeds {OCI_LIMIT} bytes")
    digest = file_sha256(path)
    if (directory / f"{OCI}.sha256").read_text(encoding="utf-8") != f"{digest}  {OCI}\n":
        fail(f"{OCI} does not match its checksum")
    offers = {platform: (name, verify_offer(directory / name, SERVER, target, profile, root))
              for name, (platform, target) in server_offers(version).items()}

    def check_files(arch: str, content: str, server: bytes) -> None:
        platform = f"linux/{arch}"
        digests = reviewed_digests(server)
        if prerelease(version):
            if content != f"{build_source(version, root)}\n":
                fail(f"the image's {platform} SOURCE.txt does not name the repository as Go's prerelease image does")
            if len(digests) != 1:
                fail(f"the image's {platform} server is not a reviewed build")
            return
        if platform not in offers:
            fail(f"the image's linux/{arch} server has no source offer")
        offer, notices = offers[platform]
        check_source_txt(content, f"the image's {platform}", version, offer, load().server[platform], root)
        if digests != {hashlib.sha256(notices.encode()).hexdigest()}:
            fail(f"the image's {platform} server is not the reviewed build of its source offer's notices")

    return digest, verify_oci.verify(f"{version}-rust", revision, path, check_files, IMAGE_STAGE)


def verify(version: str, revision: str, image: Path, tui: Path | None, assets: Path, server_profile: str = "release",
           root: Path = ROOT) -> tuple[str, str]:
    """Verify the staged image and TUI directories and copy their release assets, the TUI archives and every source
    offer, into `assets`; return the image archive's SHA-256 and manifest digest. A prerelease has only the image."""
    if (tui is None) != prerelease(version):
        fail("a Rust prerelease ships only the image; a stable release also the TUI archives")
    exact_files(image, image_files(version))
    digest, manifest = verify_image(image, version, revision, server_profile, root)
    if tui is None:
        return digest, manifest
    exact_files(tui, tui_files(version))
    for platform, target in load().tui.items():
        verify_tui(tui, version, platform, target, root)
    assets.mkdir(parents=True, exist_ok=True)
    for directory, names in ((image, set(server_offers(version))), (tui, tui_files(version))):
        for name in sorted(names):
            target = confined_path(assets / name, assets)
            if target.exists():
                fail(f"{name} arrives twice")
            shutil.copyfile(confined_path(directory / name, directory), target)
    return digest, manifest


def command_stage_image() -> None:
    version, out = release_version(env("VERSION")), runner_path("OUT_DIR")
    out.mkdir(parents=True, exist_ok=True)
    if offers := set(server_offers(version)):
        stage(runner_path("SERVER_EXPORT"), out, offers)
    shutil.copyfile(runner_path("OCI_ARCHIVE"), confined_path(out / OCI, out))
    (out / f"{OCI}.sha256").write_text(f"{file_sha256(out / OCI)}  {OCI}\n", encoding="utf-8")
    print(f"staged {sorted(image_files(version))}")


def command_stage_tui() -> None:
    version = release_version(env("VERSION"))
    stage(runner_path("TUI_EXPORT"), runner_path("OUT_DIR"), tui_files(version))
    print(f"staged {sorted(tui_files(version))}")


def command_check() -> None:
    version = release_version(env("VERSION"))
    if (profile := env("SERVER_PROFILE")) not in PROFILES:
        fail(f"SERVER_PROFILE must be one of {', '.join(PROFILES)}")
    with tempfile.TemporaryDirectory() as assets:
        verify(version, env_sha("REVISION"), runner_path("IMAGE_DIR"), runner_path("TUI_DIR"), Path(assets),
               profile)
    print(f"Rust release artifacts verified: {version}")


COMMANDS = {"stage-image": command_stage_image, "stage-tui": command_stage_tui, "check": command_check}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=COMMANDS)
    try:
        COMMANDS[parser.parse_args().command]()
    except (ControlPlaneError, OSError, ValueError) as exc:
        raise SystemExit(f"Rust release refused: {exc}") from exc


if __name__ == "__main__":
    main()
