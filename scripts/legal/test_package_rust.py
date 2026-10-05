from __future__ import annotations

import io
import tarfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

from scripts import package_rust
from scripts.legal.model import LegalError
from scripts.legal.rust import ROOT, Build
from scripts.legal.test_rust import Scratch

FILES = ("COPYRIGHT", "LICENSE", "SOURCE.txt", "THIRD_PARTY_NOTICES.txt")


class PackageTests(Scratch):
    def setUp(self) -> None:
        super().setUp()
        for name in ("LICENSE", "COPYRIGHT", "legal/project.json"):
            self.write(name, (ROOT / name).read_bytes())
        self.enterContext(patch.object(package_rust, "ROOT", self.root))
        self.enterContext(patch("sys.stdout", io.StringIO()))
        self.built: list[Build] = []

    def collect(self, build: Build) -> Path:
        self.built.append(build)
        self.write(f"{build.out.relative_to(self.root)}/LEGAL.txt", "notices\n")
        self.write(f"{build.out.relative_to(self.root)}/THIRD_PARTY_SOURCE.tar.gz", "sources")
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


if __name__ == "__main__":
    unittest.main()
