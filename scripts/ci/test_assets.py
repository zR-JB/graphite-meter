import gzip
import io
import json
import os
import shutil
import tarfile
import tempfile
import unittest
from collections.abc import Callable, Mapping
from pathlib import Path
from unittest.mock import patch

from assets import (
    CLIENT, OCI, SERVER, command_upload, find, main, merge, native_executable, staged_files, verify, verify_archive,
    verify_offer,
)
from fixtures import (
    REPOSITORY_URL, executable, notices, outcome, reviewed, server_binary, write_client, write_exports, write_oci,
    write_staged, write_tar,
)
from github_api import ControlPlaneError, file_sha256
from workspace import ROOT, build_name, load, offer_name, source_txt

REPO, SHA = "zR-JB/graphite-meter", "f" * 40
TOP = "graphite-meter_1.2.3_third-party-source"
AMD64, WINDOWS = "x86_64-unknown-linux-musl", "x86_64-pc-windows-gnu"
SERVER_AMD64, CLIENT_LINUX = build_name(SERVER, "linux/amd64"), build_name(CLIENT, "linux/amd64")


def members(path: Path) -> dict[str, bytes]:
    with tarfile.open(path, "r:gz") as archive:
        return {entry.name: handle.read() for entry in archive
                if entry.isfile() and (handle := archive.extractfile(entry)) is not None}


def rewrite(path: Path, changes: Mapping[str, bytes | None]) -> None:
    """Replace, add or, for None, delete members of a .tar.gz."""
    files: dict[str, bytes | None] = {**members(path), **changes}
    write_tar(path, {name: data for name, data in files.items() if data is not None})


class Scratch(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
        self.enterContext(patch.dict(os.environ, {"REPOSITORY": REPO}))
        self.enterContext(patch("sys.stdout", io.StringIO()))

    def fresh(self) -> Path:
        directory = self.root / str(len(list(self.root.iterdir())))
        directory.mkdir()
        return directory

    def stage(self, version: str, out: Path, exports: Path) -> None:
        env = {"RUNNER_TEMP": str(self.root), "VERSION": version, "OUT_DIR": str(out),
               "GITHUB_STEP_SUMMARY": str(self.root / "summary"),
               "OCI_ARCHIVE": str(exports / OCI), "SERVER_EXPORT": str(exports / "server-export"),
               "TUI_EXPORT": str(exports / "tui-export")}
        with patch.dict(os.environ, env), patch("sys.argv", ["assets.py", "stage"]):
            main()


class StagingTests(Scratch):
    def test_staging_adds_the_image_archives_and_one_deterministic_offer_of_every_build(self) -> None:
        exports = self.fresh()
        write_exports(exports, "1.2.3", SHA, REPO)
        self.assertEqual(staged_files("1.2.3"), {OCI, f"{OCI}.sha256", offer_name("1.2.3"), *(
            f"graphite-meter-{package}_1.2.3_{platform}.tar.gz"
            for package in ("server", "client") for platform in ("linux_amd64", "linux_arm64")),
            "graphite-meter-client_1.2.3_windows_amd64.zip"})
        offers, servers = [], []
        for out in (self.fresh(), self.fresh()):
            (out / "request.json").write_text("{}")
            self.stage("1.2.3", out, exports)
            self.assertEqual({path.name for path in out.iterdir()}, staged_files("1.2.3") | {"request.json"})
            offers.append((out / offer_name("1.2.3")).read_bytes())
            servers.append((out / "graphite-meter-server_1.2.3_linux_arm64.tar.gz").read_bytes())
        self.assertEqual(offers[0], offers[1])
        self.assertEqual(servers[0], servers[1])
        base = "graphite-meter-server_1.2.3_linux_arm64"
        self.assertEqual(members(out / f"{base}.tar.gz")[f"{base}/graphite-meter-server"],
                         server_binary("linux/arm64"))
        # The gzip header's MTIME field.
        self.assertEqual(offers[0][4:8], bytes(4))
        with tarfile.open(fileobj=io.BytesIO(gzip.decompress(offers[0]))) as archive:
            entries = archive.getmembers()
        names = [entry.name for entry in entries]
        self.assertEqual(names, sorted(names))
        self.assertEqual({(entry.mode, entry.mtime, entry.uid, entry.gid, entry.uname) for entry in entries},
                         {(0o644, 0, 0, 0, "")})
        self.assertEqual(names.count(f"{TOP}/third_party/cargo/dependency-1.0.0/src/lib.rs"), 1)
        workspace = load()
        self.assertEqual({name.split("/")[2] for name in names if "/builds/" in name},
                         {build_name(SERVER, platform) for platform in workspace.server}
                         | {build_name(CLIENT, platform) for platform in workspace.tui})

    def test_merging_refuses_conflicting_unsafe_or_unbounded_members(self) -> None:
        link = tarfile.TarInfo(f"{TOP}/link")
        link.type, link.linkname = tarfile.SYMTYPE, "/etc/passwd"
        for second, limit, error in (
            ({f"{TOP}/legal/rust-forks.json": b"[]"}, 1 << 20, None),
            ({f"{TOP}/legal/rust-forks.json": b"other"}, 1 << 20, "differs from another build's"),
            ({"elsewhere/file": b"x"}, 1 << 20, "not a regular file under"),
            ({f"{TOP}/../escape": b"x"}, 1 << 20, "not a regular file under"),
            (link, 1 << 20, "not a regular file under"),
            ({f"{TOP}/big": b"xxx"}, 2, "exceeds the source offer's bounds"),
        ):
            with self.subTest(error=error):
                directory = self.fresh()
                write_tar(directory / "a.tar.gz", {f"{TOP}/legal/rust-forks.json": b"[]"})
                if isinstance(second, tarfile.TarInfo):
                    with tarfile.open(directory / "b.tar.gz", "w:gz") as archive:
                        archive.addfile(second)
                else:
                    write_tar(directory / "b.tar.gz", second)
                with patch("assets.MEMBER_LIMIT", limit):
                    outcome(self, error, lambda: merge([directory / "a.tar.gz", directory / "b.tar.gz"],
                                                       directory / f"{TOP}.tar.gz"))

    def test_staging_finds_exactly_the_expected_files_in_platform_directories(self) -> None:
        export = self.root / "export"
        for name, content in (("linux_amd64/a.tar.gz", "a"), ("linux_arm64/b.tar.gz", "b"),
                              ("linux_amd64/provenance.json", "{}"), ("other.txt", "x")):
            (export / name).parent.mkdir(parents=True, exist_ok=True)
            (export / name).write_text(content)
        self.assertEqual(sorted(find(export, {"a.tar.gz", "b.tar.gz"})), ["a.tar.gz", "b.tar.gz"])
        (export / "linux_arm64/a.tar.gz").write_text("again")
        for names, error in (({"a.tar.gz"}, "holds a.tar.gz twice"), ({"c.tar.gz"}, r"lacks \['c.tar.gz'\]")):
            with self.subTest(error=error), self.assertRaisesRegex(ControlPlaneError, error):
                find(export, names)

    def test_upload_moves_exactly_the_named_regular_files_and_refuses_links(self) -> None:
        archive, offer = "graphite-meter-client_1.2.3_linux_amd64.tar.gz", f"{CLIENT_LINUX}_third-party-source.tar.gz"
        links: dict[str, Callable[[Path], None]] = {
            "symlink": lambda source: (source / "x").symlink_to("/proc/self/environ"),
            "directory symlink": lambda source: (source / "x").symlink_to(self.root),
            "hardlink": lambda source: (source / "x").hardlink_to(source / archive),
            "fifo": lambda source: os.mkfifo(source / "x"),
        }
        for change, error in ((None, None), ("missing", "lacks"), *((name, "link or special file") for name in links)):
            with self.subTest(change=change):
                source, out = self.fresh(), self.root / f"upload-{len(list(self.root.iterdir()))}"
                (source / "linux_amd64").mkdir()
                (source / archive).write_text("archive")
                (source / "provenance.json").write_text("{}")
                if change != "missing":
                    (source / "linux_amd64" / offer).write_text("offer")
                if change in links:
                    links[change](source)
                env = {"RUNNER_TEMP": str(self.root), "VERSION": "1.2.3", "TUI_PLATFORM": "linux/amd64",
                       "SOURCE": str(source), "OUT_DIR": str(out)}
                with patch.dict(os.environ, env):
                    outcome(self, error, lambda: command_upload("client"))
                if error is None:
                    self.assertEqual(sorted(path.name for path in out.iterdir()), sorted([archive, offer]))

    def test_the_commands_stage_into_and_check_from_runner_temp(self) -> None:
        # CI builds the image and the client archives with the ci profile, as 0.0.0-ci, which has no release tag.
        env: dict[str, str] = {}
        out = self.root
        for version, profile in (("0.0.0-ci", "ci"), ("1.2.3", "release")):
            exports, out = self.fresh(), self.fresh()
            write_exports(exports, version, SHA, REPO, profile)
            self.stage(version, out, exports)
            env = {"RUNNER_TEMP": str(self.root), "VERSION": version, "REVISION": SHA, "PROFILE": profile,
                   "STAGED_DIR": str(out), "REPOSITORY": REPO}
            with patch.dict(os.environ, env), patch("sys.argv", ["assets.py", "check"]):
                main()
        (out / "extra").write_text("x")
        for change, error in (({"VERSION": "1.2.3/x"}, "not a release version"), ({"PROFILE": "dev"}, "PROFILE"),
                              ({"STAGED_DIR": "/etc"}, "outside"), ({}, "files are")):
            with (self.subTest(error=error), patch.dict(os.environ, env | change),
                  patch("sys.argv", ["assets.py", "check"]), self.assertRaisesRegex(SystemExit, error)):
                main()


class OfferTests(Scratch):
    def test_the_offer_holds_exactly_the_release_builds_and_their_inventoried_sources(self) -> None:
        staged = self.fresh()
        write_staged(staged, "1.2.3", SHA, REPO)
        original = members(staged / offer_name("1.2.3"))

        def inventory(build: str, **change: object) -> bytes:
            return json.dumps(json.loads(original[f"{TOP}/builds/{build}/inventory.json"]) | change).encode()

        rows: tuple[tuple[dict[str, bytes | None], str | None], ...] = (
            ({}, None),
            ({f"{TOP}/LICENSE": (ROOT / "LICENSE").read_bytes()}, None),
            ({f"{TOP}/third_party/cargo/dependency-1.0.0/tests/key.pem": b"x"}, None),
            ({"elsewhere/file": b"x"}, "outside"),
            ({f"{TOP}/legal/rust-forks.json": b"[]"}, "neither dependency"),
            ({f"{TOP}/third_party/cargo/other-1.0.0/lib.rs": b"x"}, "neither dependency"),
            ({f"{TOP}/third_party/npm/other-1.0.0/index.js": b"x"}, "neither dependency"),
            ({f"{TOP}/legal/manual/key.pem": b"x"}, "certificate or key"),
            ({f"{TOP}/builds/{CLIENT_LINUX}/extra.txt": b"x"}, "neither dependency"),
            ({f"{TOP}/builds/other/LEGAL.txt": b"x"}, r"builds: missing=\[\] unexpected=\['other'\]"),
            ({f"{TOP}/builds/{SERVER_AMD64}/{name}": None for name in ("LEGAL.txt", "inventory.json")},
             rf"builds: missing=\['{SERVER_AMD64}'\]"),
            ({f"{TOP}/builds/{CLIENT_LINUX}/LEGAL.txt": b"UNREVIEWED DEVELOPMENT BUILD\n"}, "reviewed notices"),
            ({f"{TOP}/builds/{CLIENT_LINUX}/inventory.json": b"{}"}, "schemaVersion"),
            ({f"{TOP}/builds/{SERVER_AMD64}/inventory.json": inventory(SERVER_AMD64, profile="ci")},
             f"does not inventory a release build of {SERVER}"),
            ({f"{TOP}/builds/{CLIENT_LINUX}/inventory.json": inventory(CLIENT_LINUX, cargoLockSha256="0" * 64)},
             "from this Cargo.lock"),
            ({f"{TOP}/builds/{CLIENT_LINUX}/inventory.json": inventory(
                CLIENT_LINUX, browser=[{"name": "package", "version": "1.0.0"}])}, "only the server"),
            ({f"{TOP}/builds/{SERVER_AMD64}/inventory.json": inventory(
                SERVER_AMD64, browser=[{"name": "@scope/absent", "version": "2.0.0"}])},
             r"lacks the source of \[.*npm/@scope/absent-2.0.0/"),
            ({f"{TOP}/third_party/cargo/dependency-1.0.0/src/lib.rs": None}, "lacks the source of"),
        )
        for changes, error in rows:
            with self.subTest(changes=sorted(changes), error=error):
                path = self.fresh() / offer_name("1.2.3")
                write_tar(path, original)
                rewrite(path, changes)
                found = outcome(self, error, lambda: verify_offer(path, "1.2.3", "release"))
                if found is not None:
                    self.assertEqual(found, {build: notices(build).decode() for build in found})
                    self.assertEqual(len(found), len(load().server) + len(load().tui))


class ClientTests(Scratch):
    def test_executables_match_their_target(self) -> None:
        for data, target, expected in (
            (executable(AMD64), AMD64, True), (executable("aarch64-unknown-linux-musl"), AMD64, False),
            (executable(WINDOWS), WINDOWS, True), (executable(AMD64), WINDOWS, False),
            (b"#!/bin/sh\n", AMD64, False), (b"MZ", WINDOWS, False),
        ):
            with self.subTest(target=target, data=data[:20]):
                self.assertEqual(native_executable(data, target), expected)

    def test_a_client_archive_holds_a_reviewed_executable_its_build_s_notices_and_source_txt(self) -> None:
        build = build_name(CLIENT, "windows/amd64")
        source = source_txt(REPOSITORY_URL, "1.2.3", build, WINDOWS).encode()
        for changes, error in (
            ({}, None),
            ({"extra.txt": b"x"}, "unexpected"),
            ({"graphite-meter-client.exe": executable(AMD64)}, f"does not hold a {WINDOWS} executable"),
            ({"graphite-meter-client.exe": executable(WINDOWS, b"UNREVIEWED DEVELOPMENT BUILD")}, "unreviewed"),
            # A reviewed build carries the SHA-256 of exactly its notices.
            ({"graphite-meter-client.exe": executable(WINDOWS)}, "does not hold the reviewed build of its notices"),
            ({"graphite-meter-client.exe": executable(WINDOWS, reviewed(b"other\n"))},
             "does not hold the reviewed build of its notices"),
            ({"LICENSE": b"other"}, "LICENSE differs"),
            ({"THIRD_PARTY_NOTICES.txt": b"other\n"}, "notices differ"),
            ({"SOURCE.txt": source_txt(REPOSITORY_URL, "1.2.4", build, WINDOWS).encode()}, "SOURCE.txt does not name"),
            ({"SOURCE.txt": source_txt(REPOSITORY_URL, "1.2.3", CLIENT_LINUX, WINDOWS).encode()},
             "SOURCE.txt does not name"),
            ({"SOURCE.txt": source + b"more\n"}, "SOURCE.txt does not name"),
            # The source is this release's tag of legal/project.json's repository, exactly.
            ({"SOURCE.txt": source.replace(b"/tree/v1.2.3", b"/tree/main")}, "SOURCE.txt does not name"),
            ({"SOURCE.txt": source.replace(b"zR-JB/", b"other/")}, "SOURCE.txt does not name"),
        ):
            directory = self.fresh()
            write_client(directory, "1.2.3", "windows/amd64", changes)
            with self.subTest(changes=sorted(changes)):
                outcome(self, error, lambda: verify_archive(directory, CLIENT, "1.2.3", "windows/amd64", WINDOWS,
                                                            notices(build).decode()))
        self.assertEqual(source.decode().splitlines(), [
            "Graphite Meter source: https://github.com/zR-JB/graphite-meter/tree/v1.2.3", "Matching release: v1.2.3",
            "Dependency source archive: graphite-meter_1.2.3_third-party-source.tar.gz",
            f"Build: graphite-meter-client_windows_amd64 ({WINDOWS})"])


Edit = Callable[[Path], None]


class VerifyTests(Scratch):
    def run_verify(self, version: str, edit: Edit | None = None, profile: str = "release") -> tuple[str, str] | None:
        """Verify a staged release after `edit` changed it, and the image index it rewrote."""
        staged = self.fresh()
        write_staged(staged, version, SHA, REPO)
        if edit is not None:
            edit(staged)
        return verify(version, SHA, staged, profile)

    def test_the_image_the_archives_and_the_offer_bind_to_each_other(self) -> None:
        def checksum(staged: Path) -> None:
            (staged / f"{OCI}.sha256").write_text(f"{'0' * 64}  {OCI}\n")

        def image(sources: dict[str, bytes], servers: dict[str, bytes],
                  stage: tuple[str, str | None] = ("container/Dockerfile", "server")) -> Edit:
            def edit(staged: Path) -> None:
                files = {arch: {"usr/share/licenses/graphite-meter/SOURCE.txt": sources[arch],
                                "graphite-meter": servers[arch]} for arch in ("amd64", "arm64")}
                write_oci(staged / OCI, REPO, SHA, remote=False, arch_files=files, stage=stage)
                (staged / f"{OCI}.sha256").write_text(f"{file_sha256(staged / OCI)}  {OCI}\n")
            return edit

        source = {arch: source_txt(REPOSITORY_URL, "1.2.3", build_name(SERVER, f"linux/{arch}"),
                                   load().server[f"linux/{arch}"]).encode() for arch in ("amd64", "arm64")}
        server = {arch: server_binary(f"linux/{arch}") for arch in ("amd64", "arm64")}

        def client_notices(staged: Path) -> None:
            write_client(staged, "1.2.3", "linux/arm64", {"THIRD_PARTY_NOTICES.txt": b"other\n"})

        rows: tuple[tuple[Edit | None, str, str | None], ...] = (
            (None, "release", None),
            (checksum, "release", "does not match its checksum"),
            (image(source | {"arm64": source["amd64"]}, server), "release", "linux/arm64 SOURCE.txt does not name"),
            (image(source, server | {"amd64": b"server"}), "release", "linux/amd64 server is not the reviewed build"),
            (image(source, server | {"arm64": server["amd64"]}), "release", "linux/arm64 server is not the reviewed"),
            (image(source, server | {"amd64": server["amd64"] + b"\0"}), "release",
             "linux/amd64 server is not the one graphite-meter-server_1.2.3_linux_amd64.tar.gz holds"),
            (image(source, server, ("container/Dockerfile.rust", "server")), "release",
             "built target 'server' of 'Dockerfile.rust', not 'server' of 'container/Dockerfile'"),
            (image(source, server, ("container/Dockerfile", "server-build")), "release", "built target 'server-build'"),
            (client_notices, "release", "notices differ from its build's"),
            (None, "ci", "does not inventory a ci build"),
        )
        for edit, profile, error in rows:
            with self.subTest(error=error):
                result = outcome(self, error, lambda: self.run_verify("1.2.3", edit, profile))
                if result is not None:
                    self.assertEqual(result[1][:7], "sha256:")

    def test_a_prerelease_stages_and_verifies_only_its_image_which_names_the_repository(self) -> None:
        version = "1.2.3-rc.1"
        self.assertEqual(staged_files(version), {OCI, f"{OCI}.sha256"})
        exports, out = self.fresh(), self.fresh()
        write_exports(exports, version, SHA, REPO)
        # A prerelease's build exports no client archives or source offer parts.
        for export in ("server-export", "tui-export"):
            shutil.rmtree(exports / export)
        self.stage(version, out, exports)
        self.assertEqual({path.name for path in out.iterdir()}, staged_files(version))
        self.assertEqual(verify(version, SHA, out)[0], file_sha256(out / OCI))
        repository = f"{REPOSITORY_URL}\n".encode()
        arm64 = server_binary("linux/arm64")
        for sources, binary, error in (
            (repository, arm64, None),
            (source_txt(REPOSITORY_URL, "1.2.3", SERVER_AMD64, AMD64).encode(), arm64, "SOURCE.txt does not name"),
            (f"{REPOSITORY_URL}/tree/v{version}\n".encode(), arm64, "SOURCE.txt does not name"),
            (repository, b"server", "linux/arm64 server is not a reviewed build"),
            (repository, server_binary("linux/amd64"), "linux/arm64 server is not a aarch64"),
        ):
            with self.subTest(sources=sources, binary=binary):
                image = self.fresh()
                files = {arch: {"usr/share/licenses/graphite-meter/SOURCE.txt": sources, "graphite-meter": server}
                         for arch, server in (("amd64", server_binary("linux/amd64")), ("arm64", binary))}
                write_oci(image / OCI, REPO, SHA, version, remote=True, arch_files=files)
                (image / f"{OCI}.sha256").write_text(f"{file_sha256(image / OCI)}  {OCI}\n")
                outcome(self, error, lambda: verify(version, SHA, image))


if __name__ == "__main__":
    unittest.main()
