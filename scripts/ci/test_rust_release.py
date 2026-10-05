from __future__ import annotations

import json
import os
import tempfile
import unittest
from collections.abc import Callable
from pathlib import Path
from unittest.mock import patch

from fixtures import (
    RUST_OCI, RUST_STAGE, engine, executable, outcome, reviewed, rust_server, rust_source, write_oci, write_rust_offer,
    write_rust_release, write_rust_tui, write_tar,
)
from github_api import ControlPlaneError, JsonObject, file_sha256
from rust_release import (
    build_source, check_source_txt, image_files, main, native_executable, server_offers, stage, tui_files, verify,
    verify_offer, verify_tui,
)
from rust_workspace import ROOT, load, offer_name, tui_archive
from scripts.legal.model import Project
from scripts.legal.rust import report, source_notice
from verify_release_assets import TARGETS, tui_archives

REPO, SHA = "zR-JB/graphite-meter", "f" * 40
AMD64, ARM64, WINDOWS = "x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl", "x86_64-pc-windows-gnu"


class Scratch(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()


class StagingTests(Scratch):
    def test_the_release_ships_linux_and_windows_rust_builds_and_no_macos_one(self) -> None:
        self.assertEqual(image_files("1.2.3"), {
            RUST_OCI, f"{RUST_OCI}.sha256", "graphite-meter-server_1.2.3_linux_amd64_rust_third-party-source.tar.gz",
            "graphite-meter-server_1.2.3_linux_arm64_rust_third-party-source.tar.gz"})
        self.assertEqual(tui_files("1.2.3"), {
            f"graphite-meter-client_1.2.3_{platform}_rust{suffix}" for platform, archive in (
                ("linux_amd64", ".tar.gz"), ("linux_arm64", ".tar.gz"), ("windows_amd64", ".zip"))
            for suffix in (archive, "_third-party-source.tar.gz")})
        # Go keeps its macOS archives.
        self.assertIn("graphite-meter-client_1.2.3_darwin_arm64.tar.gz", tui_archives("1.2.3", TARGETS))

    def test_staging_copies_exactly_the_expected_files_out_of_platform_directories(self) -> None:
        export, out = self.root / "export", self.root / "out"
        for name, content in (("linux_amd64/a.tar.gz", "a"), ("linux_arm64/b.tar.gz", "b"),
                              ("linux_amd64/provenance.json", "{}"), ("other.txt", "x")):
            (export / name).parent.mkdir(parents=True, exist_ok=True)
            (export / name).write_text(content)
        stage(export, out, {"a.tar.gz", "b.tar.gz"})
        self.assertEqual(sorted(path.name for path in out.iterdir()), ["a.tar.gz", "b.tar.gz"])
        (export / "linux_arm64/a.tar.gz").write_text("again")
        for names, error in (({"a.tar.gz"}, "holds a.tar.gz twice"), ({"c.tar.gz"}, r"lacks \['c.tar.gz'\]")):
            with self.subTest(error=error), self.assertRaisesRegex(ControlPlaneError, error):
                stage(export, self.root / "other", names)
        (export / "linux_arm64/a.tar.gz").unlink()
        (export / "linux_arm64/a.tar.gz").symlink_to(export / "other.txt")
        stage(export, self.root / "links", {"a.tar.gz"})
        self.assertEqual((self.root / "links/a.tar.gz").read_text(), "a")

    def stage_and_check(self, version: str) -> dict[str, str]:
        """Stage a release of `version` and check it as the workflow does; return the commands' environment."""
        root = self.root / version
        image, tui = root / "built/image", root / "built/tui"
        oci = write_rust_release(image, tui, version, SHA, REPO)
        export = root / "export"
        for platform in load().server:
            name = offer_name("graphite-meter-server", version, platform)
            (export / platform.replace("/", "_")).mkdir(parents=True)
            (image / name).rename(export / platform.replace("/", "_") / name)
        env = {"RUNNER_TEMP": str(self.root), "VERSION": version, "REVISION": SHA, "SERVER_PROFILE": "release",
               "OCI_ARCHIVE": str(image / RUST_OCI), "SERVER_EXPORT": str(export), "TUI_EXPORT": str(tui),
               "IMAGE_DIR": str(root / "staged/image"), "TUI_DIR": str(root / "staged/tui")}
        env |= engine(root, REPO, f"{version}-rust", SHA, oci)
        for command, out in (("stage-image", "IMAGE_DIR"), ("stage-tui", "TUI_DIR"), ("check", "IMAGE_DIR")):
            with patch.dict(os.environ, env | {"OUT_DIR": env[out]}), patch("sys.argv", ["rust_release.py", command]):
                main()
        self.assertEqual({path.name for path in (root / "staged/image").iterdir()}, image_files(version))
        return env

    def test_the_commands_stage_into_and_check_from_runner_temp(self) -> None:
        # CI builds its image and archives as 0.0.0-ci, which has no release tag.
        self.stage_and_check("0.0.0-ci")
        env = self.stage_and_check("1.2.3")
        for change, error in (({"VERSION": "1.2.3/x"}, "not a release version"),
                              ({"SERVER_PROFILE": "dev"}, "SERVER_PROFILE"),
                              ({"IMAGE_DIR": "/etc"}, "outside")):
            with (self.subTest(error=error), patch.dict(os.environ, env | change),
                  patch("sys.argv", ["rust_release.py", "check"]), self.assertRaisesRegex(SystemExit, error)):
                main()


class OfferTests(Scratch):
    def offer(self, extra: dict[str, bytes] | None = None, package: str = "graphite-meter-server",
              profile: str = "release") -> Path:
        path = self.root / "graphite-meter-server_1.2.3_linux_amd64_rust_third-party-source.tar.gz"
        write_rust_offer(path, package, AMD64, profile, extra)
        return path

    def test_an_offer_holds_its_inventoried_sources_under_its_own_directory(self) -> None:
        top = "graphite-meter-server_1.2.3_linux_amd64_rust_third-party-source"
        forks = Path(__file__).resolve().parents[2] / "legal/rust-forks.json"
        for extra, package, profile, error in (
            ({}, "graphite-meter-server", "release", None),
            ({f"{top}/LICENSE": (forks.parents[1] / "LICENSE").read_bytes()}, "graphite-meter-server", "release",
             None),
            ({}, "graphite-meter-server", "ci", "does not inventory a release build"),
            ({}, "graphite-meter-client", "release", "does not inventory a release build"),
            ({"elsewhere/file": b"x"}, "graphite-meter-server", "release", "outside"),
            ({f"{top}/legal/rust-forks.json": b"[]"}, "graphite-meter-server", "release", "neither dependency"),
            ({f"{top}/third_party/cargo/other-1.0.0/lib.rs": b"x"}, "graphite-meter-server", "release",
             "neither dependency"),
            ({f"{top}/legal/manual/key.pem": b"x"}, "graphite-meter-server", "release", "certificate or key"),
            ({f"{top}/third_party/cargo/dependency-1.0.0/tests/key.pem": b"x"}, "graphite-meter-server", "release",
             None),
            ({f"{top}/LEGAL.txt": b"UNREVIEWED DEVELOPMENT BUILD\n"}, "graphite-meter-server", "release",
             "reviewed notices"),
            ({f"{top}/inventory.json": b"{}"}, "graphite-meter-server", "release", "schemaVersion"),
        ):
            with self.subTest(extra=sorted(extra), package=package, profile=profile):
                path = self.offer(extra, package, profile)
                notices = outcome(self, error, lambda: verify_offer(path, "graphite-meter-server", AMD64, "release"))
                if error is None:
                    self.assertEqual(notices, f"notices of {package} for {AMD64}\n")

    def test_an_offer_holds_the_source_of_every_inventoried_crate_built_from_this_lock(self) -> None:
        path = self.root / "offer_third-party-source.tar.gz"
        top = "offer_third-party-source"
        inventory = (b'{"schemaVersion": 1, "package": "graphite-meter-client", "target": "%s", "profile": "release",'
                     b' "cargoLockSha256": "%s", "components": [{"component": {"name": "absent", "version": "1"}}],'
                     b' "browser": []}')
        lock = file_sha256(Path(__file__).resolve().parents[2] / "rust/Cargo.lock")
        for digest, error in ((lock, "lacks the source of"), ("0" * 64, "from this Cargo.lock")):
            write_tar(path, {f"{top}/inventory.json": inventory % (AMD64.encode(), digest.encode()),
                             f"{top}/LEGAL.txt": b"notices\n"})
            with self.subTest(error=error), self.assertRaisesRegex(ControlPlaneError, error):
                verify_offer(path, "graphite-meter-client", AMD64, "release")

    def test_a_server_offer_holds_the_source_of_every_inventoried_browser_package_and_only_it_does(self) -> None:
        top = "graphite-meter-server_1.2.3_linux_amd64_rust_third-party-source"
        package = {"name": "package", "version": "1.0.0"}
        for name, browser, extra, error in (
            ("graphite-meter-server", [package], {}, None),
            ("graphite-meter-server", [package, {"name": "@scope/absent", "version": "2.0.0"}], {},
             r"lacks the source of \[.*npm/@scope/absent-2.0.0/"),
            ("graphite-meter-server", [], {}, "browser packages exactly when"),
            ("graphite-meter-server", [package], {f"{top}/third_party/npm/other-1.0.0/index.js": b"x"},
             "neither dependency"),
            ("graphite-meter-client", [package], {}, "browser packages exactly when"),
        ):
            with self.subTest(package=name, browser=browser, extra=sorted(extra)):
                inventory = {"schemaVersion": 1, "package": name, "target": AMD64, "profile": "release",
                             "cargoLockSha256": file_sha256(ROOT / "rust/Cargo.lock"),
                             "components": [{"component": {"name": "dependency", "version": "1.0.0"}}],
                             "browser": browser}
                path = self.offer(extra | {f"{top}/inventory.json": json.dumps(inventory).encode()}, name)
                outcome(self, error, lambda: verify_offer(path, name, AMD64, "release"))


class TuiTests(Scratch):
    def test_executables_match_their_target(self) -> None:
        for data, target, expected in (
            (executable(AMD64), AMD64, True), (executable("aarch64-unknown-linux-musl"), AMD64, False),
            (executable(WINDOWS), WINDOWS, True), (executable(AMD64), WINDOWS, False),
            (b"#!/bin/sh\n", AMD64, False), (b"MZ", WINDOWS, False),
        ):
            with self.subTest(target=target, data=data[:20]):
                self.assertEqual(native_executable(data, target), expected)

    def test_a_tui_archive_holds_go_s_layout_with_a_reviewed_executable_and_its_offer_s_notices(self) -> None:
        offer = offer_name("graphite-meter-client", "1.2.3", "windows/amd64")
        for changes, error in (
            ({}, None),
            ({"extra.txt": b"x"}, "unexpected"),
            ({"graphite-meter-client.exe": executable(AMD64)}, f"does not hold a {WINDOWS} executable"),
            ({"graphite-meter-client.exe": executable(WINDOWS, b"UNREVIEWED DEVELOPMENT BUILD")}, "unreviewed"),
            # A reviewed build carries the SHA-256 of exactly the notices beside it.
            ({"graphite-meter-client.exe": executable(WINDOWS)}, "does not hold the reviewed build of its notices"),
            ({"graphite-meter-client.exe": executable(WINDOWS, reviewed(b"other\n"))},
             "does not hold the reviewed build of its notices"),
            ({"LICENSE": b"other"}, "LICENSE differs"),
            ({"THIRD_PARTY_NOTICES.txt": b"other\n"}, "notices differ"),
            ({"SOURCE.txt": rust_source("1.2.4", offer, WINDOWS)}, "SOURCE.txt does not name"),
            ({"SOURCE.txt": rust_source("1.2.3", "other.tar.gz", WINDOWS)}, "SOURCE.txt does not name"),
            ({"SOURCE.txt": rust_source("1.2.3", offer, AMD64)}, "SOURCE.txt does not name"),
            ({"SOURCE.txt": rust_source("1.2.3", offer, WINDOWS) + b"more\n"}, "SOURCE.txt does not name"),
            # The source is this release's tag of legal/project.json's repository, exactly.
            ({"SOURCE.txt": rust_source("1.2.3", offer, WINDOWS).replace(b"/tree/v1.2.3", b"/tree/main")},
             "SOURCE.txt does not name"),
            ({"SOURCE.txt": rust_source("1.2.3", offer, WINDOWS).replace(b"zR-JB/", b"other/")},
             "SOURCE.txt does not name"),
        ):
            directory = self.root / str(len(list(self.root.iterdir())))
            directory.mkdir()
            write_rust_tui(directory, "1.2.3", "windows/amd64", WINDOWS, changes)
            with self.subTest(changes=sorted(changes)):
                outcome(self, error, lambda: verify_tui(directory, "1.2.3", "windows/amd64", WINDOWS))
        self.assertEqual(tui_archive("1.2.3", "windows/amd64")[2], "graphite-meter-client.exe")

    def test_the_checked_source_txt_is_the_one_the_collector_writes_for_each_version_form(self) -> None:
        project, repository = Project.read(ROOT), "https://github.com/zR-JB/graphite-meter"
        for version, source in (("1.2.3", f"{repository}/tree/v1.2.3"), ("1.2.3-rc.1", repository),
                                ("0.0.0-ci", repository)):
            with self.subTest(version=version):
                self.assertEqual(build_source(version), source)
                self.assertIn(f"Source code: {source}\n".encode(), report(project, version, "", False))
                offer = offer_name("graphite-meter-server", version, "linux/amd64")
                written = source_notice(project, version, offer, AMD64)
                if version == "1.2.3-rc.1":
                    self.assertEqual(written, f"{source}\n")
                    continue
                check_source_txt(written, "the image's linux/amd64", version, offer, AMD64)
                with self.assertRaisesRegex(ControlPlaneError, f"does not name {offer} for {AMD64} at {source}$"):
                    check_source_txt(written.replace(source, f"{repository}/tree/v9.9.9"), "the image's", version,
                                     offer, AMD64)


Edit = Callable[[Path, Path], JsonObject | None]


class VerifyTests(Scratch):
    def run_verify(self, edit: Edit | None = None, profile: str = "release") -> tuple[str, str] | None:
        """Verify a staged release after `edit` changed it, and the image index it rewrote."""
        image, tui, assets = self.root / "image", self.root / "tui", self.root / "assets"
        oci = write_rust_release(image, tui, "1.2.3", SHA, REPO)
        if edit is not None:
            oci = edit(image, tui) or oci
        with patch.dict(os.environ, engine(self.root, REPO, "1.2.3-rust", SHA, oci)):
            return verify("1.2.3", SHA, image, tui, assets, profile)

    def test_the_release_verifies_and_collects_every_rust_asset(self) -> None:
        image, tui, assets = self.root / "image", self.root / "tui", self.root / "assets"
        digest, manifest = self.run_verify() or ("", "")
        self.assertEqual((digest, manifest[:7]), (file_sha256(image / RUST_OCI), "sha256:"))
        self.assertEqual({path.name for path in assets.iterdir()}, set(server_offers("1.2.3")) | tui_files("1.2.3"))
        self.assertEqual({path.name for path in tui.iterdir()}, tui_files("1.2.3"))

    def test_the_image_and_its_offers_bind_to_each_other(self) -> None:
        amd64 = offer_name("graphite-meter-server", "1.2.3", "linux/amd64")

        def checksum(image: Path, _: Path) -> None:
            (image / f"{RUST_OCI}.sha256").write_text(f"{'0' * 64}  {RUST_OCI}\n")

        def rebuilt(image: Path, sources: dict[str, dict[str, bytes]],
                    stage: tuple[str, str | None] = RUST_STAGE) -> JsonObject:
            oci = write_oci(image / RUST_OCI, REPO, SHA, remote=False, arch_files=sources, stage=stage)
            (image / f"{RUST_OCI}.sha256").write_text(f"{file_sha256(image / RUST_OCI)}  {RUST_OCI}\n")
            return oci

        def swapped(image: Path, _: Path) -> JsonObject:
            named = {"usr/share/licenses/graphite-meter/SOURCE.txt": rust_source("1.2.3", amd64, AMD64)}
            return rebuilt(image, {"amd64": named | {"graphite-meter": rust_server(AMD64)},
                                   "arm64": named | {"graphite-meter": rust_server(ARM64)}})

        def servers(amd64_server: bytes, arm64_server: bytes, stage: tuple[str, str | None] = RUST_STAGE) -> Edit:
            def edit(image: Path, _: Path) -> JsonObject:
                files = {arch: {"usr/share/licenses/graphite-meter/SOURCE.txt": rust_source(
                    "1.2.3", offer_name("graphite-meter-server", "1.2.3", f"linux/{arch}"), target),
                    "graphite-meter": server} for arch, target, server in (
                    ("amd64", AMD64, amd64_server), ("arm64", ARM64, arm64_server))}
                return rebuilt(image, files, stage)
            return edit

        def unsourced(image: Path, _: Path) -> JsonObject:
            return rebuilt(image, {})

        def missing_offer(image: Path, _: Path) -> None:
            (image / amd64).unlink()

        def extra_tui(_: Path, tui: Path) -> None:
            (tui / "graphite-meter-client_1.2.3_darwin_arm64_rust.tar.gz").write_bytes(b"x")

        def ci_offer(image: Path, _: Path) -> None:
            write_rust_offer(image / amd64, "graphite-meter-server", AMD64, "ci")

        rows: tuple[tuple[Edit | None, str, str | None], ...] = (
            (checksum, "release", "does not match its checksum"),
            (swapped, "release", "linux/arm64 SOURCE.txt does not name"),
            (unsourced, "release", "linux/amd64 SOURCE.txt"),
            # Each server is the reviewed build of its own offer's notices.
            (servers(rust_server(AMD64), rust_server(ARM64)), "release", None),
            (servers(b"server", rust_server(ARM64)), "release", "linux/amd64 server is not the reviewed build"),
            (servers(rust_server(AMD64), rust_server(AMD64)), "release", "linux/arm64 server is not the reviewed"),
            # Provenance names the Rust Dockerfile and its image target: not Go's image, nor another stage.
            (servers(rust_server(AMD64), rust_server(ARM64), ("container/Dockerfile", None)), "release",
             "built target None of 'Dockerfile', not 'server' of 'container/Dockerfile.rust'"),
            (servers(rust_server(AMD64), rust_server(ARM64), ("container/Dockerfile.rust", "server-build")),
             "release", "built target 'server-build'"),
            (missing_offer, "release", "files are"),
            (extra_tui, "release", "files are"),
            (ci_offer, "release", "does not inventory a release build"),
            (None, "ci", "does not inventory a ci build"),
        )
        for edit, profile, error in rows:
            with self.subTest(error=error):
                self.root = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
                outcome(self, error, lambda: self.run_verify(edit, profile))
                self.assertEqual((self.root / "assets").exists(), error is None)


class PrereleaseTests(Scratch):
    """A prerelease, as Go's, ships only the image, which BuildKit fetched from the PR head and whose SOURCE.txt names
    the repository."""

    def test_a_prerelease_stages_and_verifies_only_its_image(self) -> None:
        version = "1.2.3-rc.1"
        self.assertEqual((server_offers(version), image_files(version)), ({}, {RUST_OCI, f"{RUST_OCI}.sha256"}))
        built, staged, assets = self.root / "built", self.root / "staged", self.root / "assets"
        oci = write_rust_release(built, None, version, SHA, REPO)
        env = {"RUNNER_TEMP": str(self.root), "VERSION": version, "OCI_ARCHIVE": str(built / RUST_OCI),
               "SERVER_EXPORT": str(self.root / "absent"), "OUT_DIR": str(staged)}
        with patch.dict(os.environ, env), patch("sys.argv", ["rust_release.py", "stage-image"]):
            main()
        with patch.dict(os.environ, engine(self.root, REPO, f"{version}-rust", SHA, oci)):
            self.assertEqual(verify(version, SHA, staged, None, assets)[0], file_sha256(staged / RUST_OCI))
            for tui_dir, release in ((self.root / "tui", version), (None, "1.2.3")):
                with self.subTest(release=release), self.assertRaisesRegex(ControlPlaneError, "ships only the image"):
                    verify(release, SHA, staged, tui_dir, assets)
        self.assertFalse(assets.exists())

    def test_a_prerelease_image_names_the_repository_as_go_s_prerelease_image_does(self) -> None:
        version, offer = "1.2.3-rc.1", offer_name("graphite-meter-server", "1.2.3-rc.1", "linux/amd64")
        repository, server = b"https://github.com/zR-JB/graphite-meter\n", rust_server(AMD64)
        for source, binary, error in ((repository, server, None),
                                      (rust_source(version, offer, AMD64), server, "does not name the repository"),
                                      (b"https://github.com/zR-JB/graphite-meter/tree/v1.2.3-rc.1\n", server,
                                       "does not name the repository"),
                                      (repository, b"server", "server is not a reviewed build")):
            with self.subTest(source=source, binary=binary):
                image = self.root / str(len(list(self.root.iterdir())))
                image.mkdir()
                files = {"usr/share/licenses/graphite-meter/SOURCE.txt": source, "graphite-meter": binary}
                oci = write_oci(image / RUST_OCI, REPO, SHA, remote=True, arch_files={"amd64": files, "arm64": files},
                                stage=RUST_STAGE)
                (image / f"{RUST_OCI}.sha256").write_text(f"{file_sha256(image / RUST_OCI)}  {RUST_OCI}\n")
                with patch.dict(os.environ, engine(self.root, REPO, f"{version}-rust", SHA, oci)):
                    outcome(self, error, lambda: verify(version, SHA, image, None, self.root / "assets"))


if __name__ == "__main__":
    unittest.main()
