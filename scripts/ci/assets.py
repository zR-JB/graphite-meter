#!/usr/bin/env python3
"""Stage the image, the server and client archives and the one source offer of a release, and verify them as the
release does.

    python3 scripts/ci/assets.py stage   # VERSION, OUT_DIR, SERVER_EXPORT, TUI_EXPORT
    python3 scripts/ci/assets.py check   # VERSION, REVISION, REPOSITORY, STAGED_DIR, PROFILE
    python3 scripts/ci/assets.py upload {request,image,client,bundle}
        # SOURCE, OUT_DIR, VERSION, and an image's SERVER_PLATFORMS or a client's TUI_PLATFORM

Verification reads the staged files as data and never runs them. Every path lies inside RUNNER_TEMP.
"""

import argparse
import gzip
import hashlib
import io
import json
import re
import shutil
import stat
import sys
import tarfile
import zipfile
from contextlib import ExitStack
from datetime import datetime
from pathlib import Path, PurePosixPath

import verify_oci
from github_api import (
    TLS_NAME, ControlPlaneError, JsonObject, JsonValue, append_summary, confined_path, decode_json, expect_array,
    expect_object, fail, file_sha256, int_field, object_field, runner_path, str_field, table,
)
from trust import env, env_sha, exact_files
from workspace import (PRERELEASE, ROOT, VERSION, build_name, build_offer, build_profile, load, offer_name,
                       pgo_architecture, release_archive, source_txt)

# The source offer's members are written as the collector writes its parts.
sys.path.append(str(ROOT))
from scripts.legal.artifacts import add_bytes, write_archive

OCI = "graphite-meter.oci.tar"
CREATED, REFERENCE = "org.opencontainers.image.created", "vnd.docker.reference.digest"
LAYOUT = re.compile(r"oci-layout|blobs(?:/sha256(?:/[0-9a-f]{64})?)?")
OCI_LIMIT = 1024 * 1024 * 1024
FILE_LIMIT = 16 * 1024 * 1024
EXECUTABLE_LIMIT = 128 * 1024 * 1024
# Bounds on what merging the source offer holds in memory.
MEMBER_LIMIT = 256 * 1024 * 1024
OFFER_LIMIT = 2 * 1024 * 1024 * 1024
SERVER, CLIENT = "graphite-meter-server", "graphite-meter-client"
PROFILES = ("ci", "release")
ARCHIVE_FILES = ("LICENSE", "COPYRIGHT", "THIRD_PARTY_NOTICES.txt", "SOURCE.txt")
# A reviewed build's executable carries its notices' SHA-256 after this (rust/legal/src/build.rs).
REVIEWED = "graphite-meter reviewed notices sha256:"
PROFILED = "graphite-meter pgo profile sha256:"
# ELF e_machine and PE machine of each target's architecture.
MACHINES = {"x86_64": (62, 0x8664), "aarch64": (183, 0xAA64)}


def release_version(value: str) -> str:
    if VERSION.fullmatch(value) is None:
        fail(f"{value!r} is not a release version")
    return value


def prerelease(version: str) -> bool:
    return PRERELEASE.fullmatch(version) is not None


def builds(version: str) -> dict[str, tuple[str, str, str]]:
    """Each build the source offer covers, with its package, platform and Rust target; a prerelease has none."""
    if prerelease(version):
        return {}
    workspace = load()
    return {build_name(package, platform): (package, platform, target)
            for package, shipped in ((SERVER, workspace.server), (CLIENT, workspace.tui))
            for platform, target in shipped.items()}


def release_assets(version: str) -> set[str]:
    """What a GitHub Release carries besides checksums.txt; a prerelease has no Release."""
    if prerelease(version):
        return set()
    return {release_archive(package, version, platform)[0] for package, platform, _ in builds(version).values()} \
        | {offer_name(version)}


def image_part(platform: str) -> str:
    """The OCI archive of one platform's image, which staging joins into OCI."""
    return f"graphite-meter_{platform.replace('/', '_')}.oci.tar"


def staged_files(version: str) -> set[str]:
    return {OCI, f"{OCI}.sha256", *release_assets(version)}


def require_same(label: str, expected: set[str], actual: set[str]) -> None:
    if actual != expected:
        fail(f"{label}: missing={sorted(expected - actual)} unexpected={sorted(actual - expected)}")


def archive_names(path: Path) -> set[str]:
    """Return member names, refusing traversal, links, special files and duplicates."""
    try:
        if path.name.endswith(".tar.gz"):
            with tarfile.open(path, mode="r:gz") as tar:
                members = [(item.name, item.isfile() or item.isdir()) for item in tar.getmembers()]
        elif path.suffix == ".zip":
            regular = {0, stat.S_IFREG, stat.S_IFDIR}
            with zipfile.ZipFile(path) as archive:
                members = [(item.filename, stat.S_IFMT(item.external_attr >> 16) in regular)
                           for item in archive.infolist()]
        else:
            fail(f"unsupported release archive type: {path}")
    except (OSError, tarfile.TarError, zipfile.BadZipFile) as exc:
        raise ControlPlaneError(f"cannot inspect {path}: {exc}") from exc
    names: set[str] = set()
    for name, regular in members:
        if not name or "\\" in name or name.startswith("/") or ".." in PurePosixPath(name).parts:
            fail(f"{path.name} contains unsafe archive path: {name!r}")
        if not regular:
            fail(f"{path.name} contains a link or special entry: {name!r}")
        if (normalized := name.rstrip("/")) in names:
            fail(f"{path.name} contains duplicate archive entry: {name!r}")
        names.add(normalized)
    return names


def regular_files(root: Path) -> list[Path]:
    """Every file under `root`, refusing links and special files, which an upload would follow or misread."""
    files = []
    for path in sorted(root.rglob("*")):
        mode = path.lstat()
        if stat.S_ISREG(mode.st_mode) and mode.st_nlink == 1:
            files.append(confined_path(path, root))
        elif not stat.S_ISDIR(mode.st_mode):
            fail(f"{path.name} is a link or special file")
    return files


def find(export: Path, names: set[str]) -> dict[str, Path]:
    """Exactly `names` in a BuildKit export, wherever its platform directories hold them."""
    found: dict[str, Path] = {}
    for path in regular_files(export):
        if path.name in names:
            if path.name in found:
                fail(f"the export holds {path.name} twice")
            found[path.name] = confined_path(path, export)
    if missing := sorted(names - found.keys()):
        fail(f"the export lacks {missing}")
    return found


def merge(parts: list[Path], out: Path) -> None:
    """Write the union of the parts' files as `out`, refusing a name two parts hold with different bytes."""
    top = out.name.removesuffix(".tar.gz") + "/"
    members: dict[str, bytes] = {}
    total = 0
    for part in parts:
        try:
            with tarfile.open(part, "r|gz") as archive:
                for entry in archive:
                    name = entry.name
                    if not entry.isfile() or not name.startswith(top) or "\\" in name \
                            or ".." in PurePosixPath(name).parts:
                        fail(f"{part.name}: {name!r} is not a regular file under {top}")
                    total += entry.size
                    handle = archive.extractfile(entry)
                    if entry.size > MEMBER_LIMIT or total > OFFER_LIMIT or handle is None:
                        fail(f"{part.name}: {name} exceeds the source offer's bounds")
                    if members.setdefault(name, data := handle.read()) != data:
                        fail(f"{part.name}: {name} differs from another build's")
        except (OSError, tarfile.TarError) as exc:
            raise ControlPlaneError(f"cannot read {part.name}: {exc}") from exc
    with out.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed, \
            tarfile.open(fileobj=compressed, mode="w") as archive:
        for name in sorted(members):
            add_bytes(archive, name, members[name])


def join(parts: list[Path], out: Path) -> None:
    """Write single-platform OCI archives as the one archive a build of all their platforms exports: their blobs and
    an index of their images followed by their provenance manifests."""
    images: list[JsonValue] = []
    attestations: list[JsonValue] = []
    created: list[str] = []
    entries: dict[str, tuple[tarfile.TarInfo, tarfile.TarFile | bytes]] = {}
    with ExitStack() as stack:
        for path in parts:
            try:
                tar = stack.enter_context(tarfile.open(path, "r:"))
                top = verify_oci.image_index(tar)
                index = verify_oci.blob(tar, top, verify_oci.INDEX_TYPE)
                own = "blobs/sha256/" + str_field(top, "digest", path.name).removeprefix("sha256:")
                templates = tar.getmember(own), tar.getmember("index.json")
            except (KeyError, tarfile.TarError) as exc:
                raise ControlPlaneError(f"cannot read {path.name}: {exc}") from exc
            # BuildKit lists a single-platform image, then its provenance, and nothing else.
            manifests = expect_array(index.get("manifests"), path.name)
            if index.keys() != {"schemaVersion", "mediaType", "manifests"} or len(manifests) != 2:
                fail(f"{path.name} does not hold one image and its provenance")
            image, attestation = (expect_object(item, path.name) for item in manifests)
            if object_field(attestation, "annotations", path.name).get(REFERENCE) != image.get("digest"):
                fail(f"{path.name} does not hold one image and its provenance")
            images.append(image)
            attestations.append(attestation)
            created.append(str_field(object_field(top, "annotations", path.name), CREATED, path.name))
            for info in tar.getmembers():
                if info.name in ("index.json", own):
                    continue
                if LAYOUT.fullmatch(info.name) is None:
                    fail(f"{path.name} holds {info.name!r} outside the OCI layout")
                entries.setdefault(info.name, (info, tar))
        index = json.dumps({"schemaVersion": 2, "mediaType": verify_oci.INDEX_TYPE,
                            "manifests": [*images, *attestations]}, indent=2).encode()
        digest = hashlib.sha256(index).hexdigest()
        layout = json.dumps({"schemaVersion": 2, "mediaType": verify_oci.INDEX_TYPE, "manifests": [
            {"mediaType": verify_oci.INDEX_TYPE, "digest": f"sha256:{digest}", "size": len(index),
             "annotations": {CREATED: max(created, key=datetime.fromisoformat)}}]}, separators=(",", ":")).encode()
        for template, name, data in ((templates[0], f"blobs/sha256/{digest}", index),
                                     (templates[1], "index.json", layout)):
            info = template.replace(name=name)
            info.size = len(data)
            entries[name] = info, data
        # In name order and USTAR format, as BuildKit writes it.
        with tarfile.open(out, "w", format=tarfile.USTAR_FORMAT) as joined:
            for name in sorted(entries):
                info, source = entries[name]
                joined.addfile(info, io.BytesIO(source) if isinstance(source, bytes) else source.extractfile(info))


def stage_assets(version: str, server_export: Path, tui_export: Path, out: Path) -> None:
    """Package each server of the staged image, copy the client archives out of the exports and merge every build's
    source offer part into one offer."""
    workspace = load()
    parts = find(server_export, {build_offer(SERVER, platform) for platform in workspace.server})
    archives = {release_archive(CLIENT, version, platform)[0] for platform in workspace.tui}
    found = find(tui_export, archives | {build_offer(CLIENT, platform) for platform in workspace.tui})
    for name in sorted(archives):
        shutil.copyfile(found.pop(name), confined_path(out / name, out))
    merge([path for _, path in sorted((parts | found).items())], confined_path(out / offer_name(version), out))
    images = verify_oci.servers(out / OCI)
    for platform in workspace.server:
        archive, base, binary = release_archive(SERVER, version, platform)
        source, server = images[platform.removeprefix("linux/")]
        notices = read(parts[build_offer(SERVER, platform)],
                       f"{offer_name(version).removesuffix('.tar.gz')}/builds/{build_name(SERVER, platform)}/LEGAL.txt")
        write_archive(confined_path(out / archive, out), base, binary, server, notices, source.encode(), ROOT)


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


def repository(root: Path = ROOT) -> str:
    """legal/project.json's repository, which every SOURCE.txt names."""
    project = expect_object(decode_json((root / "legal/project.json").read_text(), "project.json"),
                            "project.json")
    return str_field(project, "repository", "project.json")


def check_source_txt(content: str, where: str, version: str, build: str, target: str, root: Path = ROOT) -> None:
    if content != source_txt(repository(root), version, build, target):
        fail(f"{where} SOURCE.txt does not name {offer_name(version)} and build {build} at {repository(root)}")


def offer_files(path: Path, top: str) -> tuple[list[str], dict[str, bytes]]:
    """A source offer's files and, read in the same pass, the bytes of each outside its third_party/ sources."""
    files: list[str] = []
    kept: dict[str, bytes] = {}
    try:
        with tarfile.open(path, "r|gz") as archive:
            for entry in archive:
                if entry.isdir():
                    continue
                files.append(entry.name)
                if not entry.name.startswith(f"{top}/third_party/"):
                    handle = archive.extractfile(entry) if entry.isfile() and entry.size <= FILE_LIMIT else None
                    if handle is None:
                        fail(f"{path.name}: {entry.name} is not a bounded regular file")
                    kept[entry.name] = handle.read()
    except (OSError, tarfile.TarError) as exc:
        raise ControlPlaneError(f"cannot read {path.name}: {exc}") from exc
    return files, kept


def verify_offer(path: Path, version: str, profile: str,
                 root: Path = ROOT) -> tuple[dict[str, str], dict[str, str | None]]:
    """Check the source offer covers exactly the release's builds; return each build's notices and profile SHA-256."""
    top = path.name.removesuffix(".tar.gz")
    names = archive_names(path)
    if outside := sorted(name for name in names if name != top and not name.startswith(top + "/")):
        fail(f"{path.name} holds files outside {top}/: {outside[:5]}")
    # One pass reads what is checked against the checkout; a member read apiece rescans the whole archive.
    files, kept = offer_files(path, top)

    def kept_text(name: str) -> str:
        try:
            return kept[name].decode()
        except KeyError:
            raise ControlPlaneError(f"cannot read {name} from {path.name}") from None
        except UnicodeDecodeError as exc:
            raise ControlPlaneError(f"{path.name}: {name} is not UTF-8") from exc

    def tree(ecosystem: str, component: JsonObject) -> str:
        return (f"{top}/third_party/{ecosystem}/{str_field(component, 'name', path.name)}-"
                f"{str_field(component, 'version', path.name)}/")

    expected = builds(version)
    require_same(f"{path.name} builds", set(expected),
                 {name.split("/")[2] for name in files if name.startswith(f"{top}/builds/")})
    lock = file_sha256(root / "rust/Cargo.lock")
    trees: set[str] = set()
    notices: dict[str, str] = {}
    profiles: dict[str, str | None] = {}
    for build, (package, _, target) in expected.items():
        where = f"{path.name} builds/{build}"
        inventory = expect_object(decode_json(kept_text(f"{top}/builds/{build}/inventory.json"), where), where)
        identity = {"package": package, "target": target, "profile": profile, "cargoLockSha256": lock}
        if int_field(inventory, "schemaVersion", where) != 1 or any(
                inventory.get(key) != value for key, value in identity.items()):
            fail(f"{where} does not inventory a {profile} build of {package} for {target} from this Cargo.lock")
        trees |= {tree("cargo", object_field(expect_object(item, where), "component", where))
                  for item in expect_array(inventory.get("components"), f"{where} components")}
        browser = {tree("npm", expect_object(item, where))
                   for item in expect_array(inventory.get("browser"), f"{where} browser")}
        if package != SERVER and browser:
            fail(f"{where}: only the server inventories browser packages")
        trees |= browser
        notices[build] = kept_text(f"{top}/builds/{build}/LEGAL.txt")
        if not notices[build].strip() or verify_oci.DEVELOPMENT in notices[build]:
            fail(f"{where} lacks reviewed notices")
        trained = kept.get(f"{top}/{build_profile(build)}")
        if (trained is not None) != (profile == "release" and pgo_architecture(target) is not None):
            fail(f"{where} {'holds' if trained is not None else 'lacks'} the PGO profile of a {profile} build")
        profiles[build] = None if trained is None else hashlib.sha256(trained).hexdigest()
    own = {f"{top}/builds/{build}/{name}" for build in expected for name in ("inventory.json", "LEGAL.txt")}
    own |= {f"{top}/{build_profile(build)}" for build, digest in profiles.items() if digest is not None}
    for name in files:
        if name.startswith(tuple(trees)) or name in own:
            continue
        relative = name.removeprefix(top + "/")
        if TLS_NAME.search(relative):
            fail(f"{path.name} holds certificate or key material outside dependency source: {relative}")
        local = confined_path(root / relative, root)
        if not local.is_file() or kept[name] != local.read_bytes():
            fail(f"{path.name} holds {relative}, which is neither dependency source nor this checkout's file")
    if missing := sorted(tree for tree in trees if not any(name.startswith(tree) for name in files)):
        fail(f"{path.name} lacks the source of {missing[:5]}")
    return notices, profiles


def marked(executable: bytes, marker: str) -> set[str]:
    """The SHA-256 digests that `executable` names after `marker`."""
    return {digest.decode() for digest in re.findall(re.escape(marker.encode()) + rb"([0-9a-f]{64})", executable)}


def native_executable(data: bytes, target: str) -> bool:
    """Whether `data` is a 64-bit little-endian executable for `target`: PE for Windows, otherwise ELF."""
    elf, pe = MACHINES.get(target.split("-", 1)[0], (0, 0))

    def field(offset: int, size: int) -> int:
        return int.from_bytes(data[offset:offset + size], "little")

    if "-windows-" in target:
        start = field(0x3C, 4)
        return data[:2] == b"MZ" and data[start:start + 4] == b"PE\0\0" and field(start + 4, 2) == pe
    return data[:6] == b"\x7fELF\x02\x01" and field(16, 2) in (2, 3) and field(18, 2) == elf


def verify_archive(directory: Path, package: str, version: str, platform: str, target: str, notices: str,
                   profile: str | None, root: Path = ROOT) -> str:
    """A release archive holds a reviewed executable for `target` of its build's `notices`, this checkout's license
    files, those notices and the SOURCE.txt that names the offer and the build; return the executable's SHA-256."""
    archive, base, binary = release_archive(package, version, platform)
    path = directory / archive
    require_same(archive, {f"{base}/{name}" for name in (binary, *ARCHIVE_FILES)}, archive_names(path) - {base})
    executable = read(path, f"{base}/{binary}", EXECUTABLE_LIMIT)
    if not native_executable(executable, target):
        fail(f"{archive} does not hold a {target} executable")
    if verify_oci.DEVELOPMENT.encode() in executable:
        fail(f"{archive} holds an unreviewed development build")
    for name in ("LICENSE", "COPYRIGHT"):
        if read(path, f"{base}/{name}") != (root / name).read_bytes():
            fail(f"{archive}: {name} differs from this checkout's")
    if text(path, f"{base}/THIRD_PARTY_NOTICES.txt") != notices:
        fail(f"{archive}'s notices differ from its build's in the source offer")
    if marked(executable, REVIEWED) != {hashlib.sha256(notices.encode()).hexdigest()}:
        fail(f"{archive} does not hold the reviewed build of its notices")
    if marked(executable, PROFILED) != ({profile} if profile else set()):
        fail(f"{archive} was not optimized with the PGO profile its build's part of the source offer holds")
    check_source_txt(text(path, f"{base}/SOURCE.txt"), archive, version, build_name(package, platform), target, root)
    return hashlib.sha256(executable).hexdigest()


def verify_image(directory: Path, version: str, revision: str, notices: dict[str, str], executables: dict[str, str],
                 root: Path = ROOT) -> tuple[str, str]:
    """Verify the staged image against its builds' `notices` and archived `executables`' SHA-256; return the
    archive's SHA-256 and manifest digest."""
    path = directory / OCI
    if path.stat().st_size > OCI_LIMIT:
        fail(f"{OCI} exceeds {OCI_LIMIT} bytes")
    digest = file_sha256(path)
    if (directory / f"{OCI}.sha256").read_text() != f"{digest}  {OCI}\n":
        fail(f"{OCI} does not match its checksum")
    server = load().server

    def check_files(arch: str, content: str, binary: bytes) -> None:
        platform = f"linux/{arch}"
        if platform not in server:
            fail(f"the image ships {platform}, which the workspace does not")
        build = build_name(SERVER, platform)
        check_source_txt(content, f"the image's {platform}", version, build, server[platform], root)
        digests = marked(binary, REVIEWED)
        if prerelease(version) and len(digests) != 1:
            fail(f"the image's {platform} server is not a reviewed build")
        if not prerelease(version) and digests != {hashlib.sha256(notices[build].encode()).hexdigest()}:
            fail(f"the image's {platform} server is not the reviewed build of its notices in the source offer")
        if not native_executable(binary, server[platform]):
            fail(f"the image's {platform} server is not a {server[platform]} executable")
        if not prerelease(version) and hashlib.sha256(binary).hexdigest() != executables[build]:
            fail(f"the image's {platform} server is not the one {release_archive(SERVER, version, platform)[0]} holds")

    return digest, verify_oci.verify(version, revision, path, check_files)


def verify(version: str, revision: str, staged: Path, profile: str = "release",
           root: Path = ROOT) -> tuple[str, str]:
    """Verify the staged release files; return the image archive's SHA-256 and manifest digest."""
    notices: dict[str, str] = {}
    executables: dict[str, str] = {}
    if not prerelease(version):
        notices, profiles = verify_offer(staged / offer_name(version), version, profile, root)
        executables = {build: verify_archive(staged, package, version, platform, target, notices[build],
                                             profiles[build], root)
                       for build, (package, platform, target) in builds(version).items()}
    return verify_image(staged, version, revision, notices, executables, root)


def size(path: Path) -> str:
    value = path.stat().st_size
    return f"{value / 2**20:.1f} MiB" if value >= 2**20 else f"{value} B"


def summarize(directory: Path, version: str) -> None:
    append_summary(table([("File", "Size"), *((f"`{name}`", size(directory / name))
                                              for name in sorted(staged_files(version)))]))


def upload_names(kind: str) -> set[str]:
    """What each release request job uploads."""
    if kind == "request":
        return {"request.json"}
    version = release_version(env("VERSION"))
    if kind == "bundle":
        return {"request.json", *staged_files(version)}
    if kind == "client":
        platform = env("TUI_PLATFORM")
        if platform not in load().tui:
            fail(f"{platform} is not a TUI platform")
        return {release_archive(CLIENT, version, platform)[0], build_offer(CLIENT, platform)}
    platforms = env("SERVER_PLATFORMS").split()
    if not set(platforms) <= load().server.keys():
        fail(f"{platforms} are not server platforms")
    offers = {build_offer(SERVER, platform) for platform in platforms}
    return {*map(image_part, platforms), *(set() if prerelease(version) else offers)}


def command_upload(kind: str) -> None:
    """Move exactly the named regular files of this job's output into OUT_DIR, the one directory it uploads."""
    names, out = upload_names(kind), runner_path("OUT_DIR")
    out.mkdir(parents=True)
    for name, path in find(runner_path("SOURCE"), names).items():
        path.rename(confined_path(out / name, out))
    exact_files(out, names)


def stage(version: str, server_export: Path, tui_export: Path, out: Path) -> None:
    """Join the image's platform archives and, for a stable release, stage its assets."""
    parts = find(server_export, {image_part(platform) for platform in load().server})
    join([path for _, path in sorted(parts.items())], confined_path(out / OCI, out))
    (out / f"{OCI}.sha256").write_text(f"{file_sha256(out / OCI)}  {OCI}\n")
    if not prerelease(version):
        stage_assets(version, server_export, tui_export, out)


def command_stage() -> None:
    version, out = release_version(env("VERSION")), runner_path("OUT_DIR")
    out.mkdir(parents=True, exist_ok=True)
    stage(version, runner_path("SERVER_EXPORT"), runner_path("TUI_EXPORT"), out)
    summarize(out, version)


def command_check() -> None:
    version = release_version(env("VERSION"))
    if (profile := env("PROFILE")) not in PROFILES:
        fail(f"PROFILE must be one of {', '.join(PROFILES)}")
    staged = runner_path("STAGED_DIR")
    exact_files(staged, staged_files(version))
    verify(version, env_sha("REVISION"), staged, profile)
    print(f"release files verified: {version}")


COMMANDS = {"stage": command_stage, "check": command_check}
UPLOADS = ("request", "image", "client", "bundle")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=[*COMMANDS, "upload"])
    parser.add_argument("kind", nargs="?", choices=UPLOADS)
    args = parser.parse_args()
    try:
        if args.command == "upload":
            command_upload(args.kind or fail("upload needs a kind"))
        else:
            COMMANDS[args.command]()
    except (ControlPlaneError, OSError, ValueError) as exc:
        raise SystemExit(f"release files refused: {exc}") from exc


if __name__ == "__main__":
    main()
