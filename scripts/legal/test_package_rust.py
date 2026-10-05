from __future__ import annotations

import io
import tarfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

from scripts import package_rust
from scripts.ci.rust_workspace import load, offer_name
from scripts.legal.model import LegalError, Project
from scripts.legal.rust import ROOT, Build, source_notice
from scripts.legal.test_rust import Scratch

FILES = ("COPYRIGHT", "LICENSE", "SOURCE.txt", "THIRD_PARTY_NOTICES.txt")
PLATFORMS = (("linux/arm64", "aarch64-unknown-linux-musl"), ("windows/amd64", "x86_64-pc-windows-gnu"))


def tar_gz(files: dict[str, bytes]) -> bytes:
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:gz") as archive:
        for name, content in files.items():
            entry = tarfile.TarInfo(name)
            entry.size = len(content)
            archive.addfile(entry, io.BytesIO(content))
    return data.getvalue()


class PackageTests(Scratch):
    def setUp(self) -> None:
        super().setUp()
        for name in ("LICENSE", "COPYRIGHT", "legal/project.json"):
            self.write(name, (ROOT / name).read_bytes())
        self.enterContext(patch.object(package_rust, "ROOT", self.root))
        self.enterContext(patch("sys.stdout", io.StringIO()))
        self.built: list[Build] = []

    def collect(self, build: Build) -> Path:
        """Write what the collector writes for a reviewed build."""
        assert build.target is not None
        self.built.append(build)
        out = build.out.relative_to(self.root)
        platform_name = next(name for name, target in load().tui.items() if target == build.target)
        offer = offer_name(build.package, build.version, platform_name)
        self.write(f"{out}/LEGAL.txt", "notices\n")
        self.write(f"{out}/{offer}", tar_gz({"source/README.txt": b"sources"}))
        self.write(f"{out}/SOURCE.txt", source_notice(Project.read(self.root), build.version, offer, build.target))
        return self.write("rust/target/built/graphite-meter-client", "binary")

    def test_each_platform_gets_go_s_archive_layout_with_the_rust_marker_and_its_source_offer(self) -> None:
        output = self.root / "dist"
        output.mkdir()
        with patch.object(package_rust, "collect", self.collect), \
                patch.object(package_rust, "host_platform", return_value="plan9/amd64"):
            package_rust.package("1.2.3", "linux/arm64", "aarch64-unknown-linux-musl", output)
            package_rust.package("1.2.3", "windows/amd64", "x86_64-pc-windows-gnu", output)
        self.assertEqual([(build.target, build.profile, build.version, build.development) for build in self.built],
                         [("aarch64-unknown-linux-musl", "release", "1.2.3", False),
                          ("x86_64-pc-windows-gnu", "release", "1.2.3", False)])
        linux = "graphite-meter-client_1.2.3_linux_arm64_rust"
        windows = "graphite-meter-client_1.2.3_windows_amd64_rust"
        self.assertEqual(sorted(path.name for path in output.iterdir()), [
            f"{linux}.tar.gz", f"{linux}_third-party-source.tar.gz",
            f"{windows}.zip", f"{windows}_third-party-source.tar.gz"])
        with tarfile.open(output / f"{linux}.tar.gz") as archive:
            members = {member.name: member for member in archive.getmembers() if member.isfile()}
            self.assertEqual(sorted(members), sorted(f"{linux}/{name}" for name in (*FILES, "graphite-meter-client")))
            notices = archive.extractfile(members[f"{linux}/THIRD_PARTY_NOTICES.txt"])
            source = archive.extractfile(members[f"{linux}/SOURCE.txt"])
            assert notices is not None and source is not None
            self.assertEqual(notices.read(), b"notices\n")
            self.assertEqual(source.read().decode().splitlines(), [
                "Graphite Meter source: https://github.com/zR-JB/graphite-meter/tree/v1.2.3",
                "Matching release: v1.2.3", f"Dependency source archive: {linux}_third-party-source.tar.gz",
                "Rust target: aarch64-unknown-linux-musl"])
        with zipfile.ZipFile(output / f"{windows}.zip") as archive:
            self.assertEqual(sorted(name for name in archive.namelist() if not name.endswith("/")),
                             sorted(f"{windows}/{name}" for name in (*FILES, "graphite-meter-client.exe")))
        self.assertEqual(list((self.root / "rust/target").glob(".rust-notices-*")), [])

    def test_a_build_for_this_machine_reports_its_version_and_keeps_huge_pages_off(self) -> None:
        executable = self.write("graphite-meter-client", '#!/bin/sh\necho "graphite-meter-client ${VERSION_SHOWN}"\n'
                                '[ -n "$MIMALLOC_VERBOSE" ] && echo "mimalloc: option \'allow_thp\': '
                                '${MIMALLOC_ALLOW_THP:-${THP_DEFAULT}}" >&2\nexit 0\n')
        executable.chmod(0o755)
        for shown, default, error in (("1.2.3-rust", "0", None), ("1.2.2-rust", "0", "reports"),
                                      ("1.2.3-rust", "1", "THP setting")):
            with self.subTest(shown=shown, default=default), \
                    patch.dict("os.environ", {"VERSION_SHOWN": shown, "THP_DEFAULT": default}):
                if error is None:
                    package_rust.probe(executable, "1.2.3", "linux/amd64")
                    continue
                with self.assertRaisesRegex(LegalError, error):
                    package_rust.probe(executable, "1.2.3", "linux/amd64")

    def test_the_check_runs_the_archived_build_for_this_machine_only(self) -> None:
        output = self.root / "dist"
        output.mkdir()
        probed: list[tuple[bytes, str]] = []
        with patch.object(package_rust, "collect", self.collect), \
                patch.object(package_rust, "probe", lambda path, _, name: probed.append((path.read_bytes(), name))):
            with patch.object(package_rust, "host_platform", return_value="plan9/amd64"):
                for platform_name, target in PLATFORMS:
                    package_rust.package("1.2.3", platform_name, target, output)
                with self.assertRaisesRegex(LegalError, "no Rust TUI is built for plan9/amd64"):
                    package_rust.check("1.2.3", output)
            for host, _ in PLATFORMS:
                with patch.object(package_rust, "host_platform", return_value=host):
                    package_rust.check("1.2.3", output)
        self.assertEqual(probed, [(b"binary", "linux/arm64"), (b"binary", "windows/amd64")])

    def test_archives_are_reproducible_with_fixed_times_owners_and_modes(self) -> None:
        archives: list[dict[str, bytes]] = []
        for number in range(2):
            output = self.root / f"dist-{number}"
            output.mkdir()
            with patch.object(package_rust, "collect", self.collect), \
                    patch.object(package_rust, "host_platform", return_value="plan9/amd64"):
                for platform_name, target in PLATFORMS:
                    (self.root / "LICENSE").touch()
                    package_rust.package("1.2.3", platform_name, target, output)
            archives.append({path.name: path.read_bytes() for path in output.iterdir()})
        self.assertEqual(archives[0], archives[1])
        linux = "graphite-meter-client_1.2.3_linux_arm64_rust"
        with tarfile.open(self.root / f"dist-0/{linux}.tar.gz") as archive:
            self.assertEqual({(entry.name, entry.mode, entry.mtime, entry.uid, entry.gid, entry.uname)
                              for entry in archive}, {(linux, 0o755, 0, 0, 0, "")} | {
                (f"{linux}/{name}", 0o755 if name == "graphite-meter-client" else 0o644, 0, 0, 0, "")
                for name in (*FILES, "graphite-meter-client")})
        windows = "graphite-meter-client_1.2.3_windows_amd64_rust"
        with zipfile.ZipFile(self.root / f"dist-0/{windows}.zip") as archive:
            self.assertEqual({(entry.filename, entry.date_time, entry.external_attr >> 16)
                              for entry in archive.infolist()}, {(f"{windows}/", (1980, 1, 1, 0, 0, 0), 0o40755)} | {
                (f"{windows}/{name}", (1980, 1, 1, 0, 0, 0), 0o100755 if name.endswith(".exe") else 0o100644)
                for name in (*FILES, "graphite-meter-client.exe")})

if __name__ == "__main__":
    unittest.main()
