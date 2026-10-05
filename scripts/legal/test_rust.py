from __future__ import annotations

import io
import json
import os
import subprocess
import tarfile
import unittest
import zlib
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Any
from unittest.mock import patch

from scripts.ci.github_api import ControlPlaneError
from scripts.legal import rust_platform as platform
from scripts.legal.check_rust_reviews import manual_problems
from scripts.legal.model import LegalError, Review, marshal, sha256
from scripts.legal.model import Project
from scripts.legal.rust import (DEVELOPMENT, ROOT, Build, Prepared, build_verify, parse, report, source_notice,
                                source_offer, stage_browser, write_changed)
from scripts.legal.rust_inventory import compiled, components, selected
from scripts.legal.rust_platform import SYSROOT

REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
NOQ = "git+https://github.com/zR-JB/noq?rev=94e4e10bf84f5ffcad04f8c471dc8e98b980298f#94e4e10bf84f5ffcad04f8c471dc8e98b980298f"


class Scratch(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(TemporaryDirectory())).resolve()

    def write(self, relative: str, content: str | bytes) -> Path:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content.encode() if isinstance(content, str) else content)
        return path


class InventoryTests(Scratch):
    def package(self, name: str, version: str, source: str | None, license: str = "MIT") -> dict[str, Any]:
        manifest = self.write(f"{name}-{version}/Cargo.toml", "[package]\n")
        self.write(f"{name}-{version}/LICENSE-MIT", f"{name} license\n")
        return {"id": f"{name}@{version}", "name": name, "version": version, "source": source,
                "license": license, "manifest_path": str(manifest)}

    def review(self, name: str, version: str, source: str = REGISTRY, modified: bool = False) -> Review:
        text = f"{name} license\n".encode()
        return Review.parse({"ecosystem": "cargo", "name": name, "reviewedVersion": version, "upstream": source,
                             "declaredLicenseExpression": "MIT", "selectedLicenseExpression": "MIT",
                             "legalFiles": [{"name": "LICENSE-MIT", "sha256": sha256(text), "kind": "license"}],
                             "modified": modified, "reviewDecision": "approved", "reviewNotes": "reviewed"})

    def test_a_name_and_version_must_name_exactly_one_package(self) -> None:
        metadata = {"packages": [{"id": "a", "name": "ring", "version": "1.0"},
                                 {"id": "b", "name": "ring", "version": "1.0"},
                                 {"id": "c", "name": "h2", "version": "0.4"}]}
        self.assertEqual(selected(metadata, {("h2", "0.4")}), {"c"})
        for crates in ({("ring", "1.0")}, {("h2", "0.5")}):
            with self.subTest(crates=crates), self.assertRaisesRegex(LegalError, "missing or ambiguous"):
                selected(metadata, crates)

    def test_registry_reviews_span_versions_git_reviews_bind_the_fork_and_workspace_crates_need_none(self) -> None:
        packages = [self.package("ring", "0.17.15", REGISTRY), self.package("noq", "1.3.0", NOQ),
                    self.package("graphite-meter-net", "0.0.0", None)]
        metadata = {"packages": packages, "workspace_members": ["graphite-meter-net@0.0.0"]}
        builds: dict[str, Any] = {item["id"]: {} for item in packages}
        found, inventory, failures = components(metadata, builds, [self.review("ring", "0.17.14"),
                                                                   self.review("noq", "1.3.0", NOQ, True)], [])
        self.assertEqual(failures, {})
        self.assertEqual([(item.name, item.modified) for item in found], [("noq", True), ("ring", False)])
        self.assertEqual(inventory[0]["upstreamRevision"], "94e4e10bf84f5ffcad04f8c471dc8e98b980298f")
        _, _, failures = components(metadata, builds, [self.review("ring", "0.17.14"),
                                                       self.review("noq", "1.2.0", NOQ, True)], [])
        self.assertEqual(list(failures), [("noq", "1.3.0", NOQ)])
        other = NOQ.replace("94e4e10", "0000000")
        with self.assertRaisesRegex(LegalError, "unreviewed Cargo git source"):
            components({"packages": [self.package("noq", "1.3.0", other)], "workspace_members": []},
                       {"noq@1.3.0": {}}, [], [])

    def test_changed_license_bytes_need_a_new_review(self) -> None:
        packages = [self.package("ring", "0.17.15", REGISTRY)]
        self.write("ring-0.17.15/LICENSE-MIT", "changed\n")
        _, _, failures = components({"packages": packages, "workspace_members": []}, {"ring@0.17.15": {}},
                                    [self.review("ring", "0.17.14")], [])
        self.assertIn("legal fingerprint changed", failures["ring", "0.17.15", REGISTRY])

    def messages(self) -> list[dict[str, Any]]:
        return [
            {"reason": "compiler-artifact", "package_id": "dependency", "target": {"name": "dependency", "kind": ["lib"]},
             "profile": {"test": False}, "features": ["b", "a"], "executable": None},
            {"reason": "build-script-executed", "package_id": "dependency", "linked_libs": ["static=crypto"]},
            {"reason": "compiler-artifact", "package_id": "app", "target": {"name": "app", "kind": ["bin"]},
             "profile": {"test": False}, "features": [], "executable": "/target/app"},
            {"reason": "build-finished", "success": True},
        ]

    def test_a_build_reports_its_units_and_native_libraries_and_only_a_complete_binary_build_counts(self) -> None:
        result = compiled(self.messages(), "app", "app")
        self.assertEqual(result["dependency"], {"units": [{"name": "dependency", "kinds": ["lib"],
                                                           "features": ["a", "b"]}],
                                                "nativeLibraries": ["static=crypto"]})
        messages = self.messages()
        tests = json.loads(json.dumps(messages))
        tests[0]["target"]["kind"] = ["test"]
        for bad in (messages[:-1], messages[:-1] + [{"reason": "build-finished", "success": False}],
                    messages[:2] + messages[-1:], tests):
            with self.subTest(messages=bad), self.assertRaises(LegalError):
                compiled(bad, "app", "app")


class CollectorTests(Scratch):
    def test_development_reports_open_with_the_marker_rust_legal_keeps(self) -> None:
        source = (ROOT / "rust/legal/src/lib.rs").read_text()
        self.assertIn(f'const DEVELOPMENT: &str = "{DEVELOPMENT}";', source)
        from scripts.legal.model import Project
        project = Project.read(ROOT)
        development = report(project, "development", "notices\n", True)
        self.assertTrue(development.startswith(DEVELOPMENT.encode()))
        reviewed = report(project, "1.2.3", "notices\n", False)
        self.assertTrue(reviewed.startswith(b"Graphite Meter\n"))
        self.assertIn(b"Source code: https://github.com/zR-JB/graphite-meter/tree/v1.2.3\n", reviewed)
        self.assertTrue(reviewed.endswith(b"\nnotices\n"))

    def test_staging_mirrors_the_browser_build_but_keeps_the_server_legal_files(self) -> None:
        source, staged = self.root / "dist", self.root / "staged"
        self.write("dist/index.html", "<head></head>")
        self.write("dist/assets/app.js", "app")
        self.write("dist/legal/about.json", "Go's")
        self.write("staged/legal/about.json", "Rust's")
        self.write("staged/assets/stale.js", "stale")
        stage_browser(source, staged)
        listed = sorted(path.relative_to(staged).as_posix() for path in staged.rglob("*") if path.is_file())
        self.assertEqual(listed, ["assets/app.js", "index.html", "legal/about.json"])
        self.assertEqual((staged / "legal/about.json").read_text(), "Rust's")
        os.utime(staged / "assets/app.js", (1, 1))
        stage_browser(source, staged)
        self.assertEqual((staged / "assets/app.js").stat().st_mtime, 1)
        (source / "assets/link.js").symlink_to(source / "index.html")
        with self.assertRaisesRegex(LegalError, "symbolic link"):
            stage_browser(source, staged)
        (source / "index.html").unlink()
        with self.assertRaisesRegex(LegalError, "no index.html"):
            stage_browser(source, staged)

    def test_write_changed_keeps_unchanged_files_untouched(self) -> None:
        path = self.write("inputs/rust/Cargo.lock", "lock")
        os.utime(path, (1, 1))
        write_changed(path, b"lock")
        self.assertEqual(path.stat().st_mtime, 1)
        write_changed(path, b"changed")
        self.assertEqual(path.read_bytes(), b"changed")

    def test_arguments_select_a_shipped_build_inside_the_checkout(self) -> None:
        out = str(ROOT / "rust/target/notices")
        build, template = parse(["--package", "graphite-meter-client", "--target", "x86_64-pc-windows-gnu",
                                 "--out", out, "--version", "1.2.3"])
        self.assertEqual((build.target, build.profile, build.development, template),
                         ("x86_64-pc-windows-gnu", "release", False, False))
        build, _ = parse(["--package", "graphite-meter-server", "--development", "--profile", "ci", "--out", out,
                          "--browser-assets", str(ROOT / "client/dist"), "--browser-scan", "/tmp/modules.json"])
        self.assertTrue(build.development)
        for arguments in (
            ["--package", "graphite-meter-server", "--target", "x86_64-pc-windows-gnu", "--out", out,
             "--browser-assets", "client/dist", "--browser-scan", "/tmp/scan.json"],
            ["--package", "graphite-meter-client", "--target", "x86_64-unknown-linux-musl", "--development",
             "--out", out],
            ["--package", "graphite-meter-client", "--development", "--out", out, "--version", "1.0 beta"],
            ["--package", "graphite-meter-server", "--development", "--out", out],
            ["--package", "graphite-meter-client", "--development", "--out", out, "--browser-assets", "client/dist",
             "--browser-scan", "/tmp/scan.json"],
            ["--package", "graphite-meter-client", "--development", "--out", "/tmp/notices"],
            ["--package", "graphite-meter-client", "--development", "--devel", "--out", out],
        ):
            with self.subTest(arguments=arguments), self.assertRaises(SystemExit), \
                    patch("sys.stderr", io.StringIO()):
                parse(arguments)
        with self.assertRaises(ControlPlaneError):
            parse(["--package", "graphite-meter-client", "--development", "--out", "/etc/notices"])


class BuildVerifyTests(Scratch):
    """build_verify around a fake Cargo build that compiled `dependency` into `app`."""

    def setUp(self) -> None:
        super().setUp()
        self.out = self.root / "notices"
        self.report = self.write("notices/LEGAL.txt", "UNREVIEWED DEVELOPMENT BUILD\nnotices\n").read_bytes()
        self.payload = zlib.compress(self.report)
        self.write("build/legal.zlib", self.payload)
        self.binary = self.write("bin/app", b"code" + self.payload + b"UNREVIEWED DEVELOPMENT BUILD")
        manifest = self.write("dependency/Cargo.toml", "")
        self.write("dependency/LICENSE", "license\n")
        self.metadata = {"packages": [
            {"id": "app", "name": "graphite-meter-client", "version": "0.0.0", "source": None,
             "manifest_path": str(self.write("app/Cargo.toml", ""))},
            {"id": "dependency", "name": "dependency", "version": "1.0.0", "source": REGISTRY, "license": "MIT",
             "manifest_path": str(manifest)}], "workspace_members": ["app"], "target_directory": str(self.root)}
        review = Review.parse({"ecosystem": "cargo", "name": "dependency", "upstream": REGISTRY,
                               "declaredLicenseExpression": "MIT", "selectedLicenseExpression": "MIT",
                               "legalFiles": [{"name": "LICENSE", "sha256": sha256(b"license\n"), "kind": "license"}],
                               "reviewDecision": "approved", "reviewNotes": "reviewed"})
        self.reviews = [review]
        prepared, _, _ = components(self.metadata, {"dependency": {}}, self.reviews, [])
        self.state = Prepared("host", "host", self.root, self.metadata, {"app", "dependency"}, prepared,
                              "platform", None, self.reviews, [])

    def run_build(self, compiled_ids: tuple[str, ...] = ("dependency", "app")) -> Path:
        artifacts = [{"reason": "compiler-artifact", "package_id": identity,
                      "target": {"name": "graphite-meter-client" if identity == "app" else identity,
                                 "kind": ["bin"] if identity == "app" else ["lib"]},
                      "profile": {"test": False}, "features": [],
                      "executable": str(self.binary) if identity == "app" else None} for identity in compiled_ids]
        messages = [*artifacts, {"reason": "build-script-executed", "package_id": "app", "linked_libs": [],
                                 "out_dir": str(self.root / "build")}, {"reason": "build-finished", "success": True}]
        result = subprocess.CompletedProcess([], 0, "".join(json.dumps(item) + "\n" for item in messages))
        build = Build("graphite-meter-client", None, "ci", "development", self.out)
        with patch("scripts.legal.rust.subprocess.run", return_value=result):
            return build_verify(build, self.state)[0]

    def test_a_build_of_the_prepared_crates_with_embedded_notices_passes(self) -> None:
        self.assertEqual(self.run_build(), self.binary)
        inventory = json.loads((self.out / "inventory.json").read_bytes())
        self.assertEqual([item["component"]["name"] for item in inventory["components"]], ["dependency"])

    def test_unprepared_crates_changed_notices_or_other_embedded_notices_are_refused(self) -> None:
        self.state.selected = {"app"}
        with self.assertRaisesRegex(LegalError, "absent from the prepared notices"):
            self.run_build()
        self.state.selected = {"app", "dependency"}
        self.write("dependency/LICENSE", "changed\n")
        with self.assertRaisesRegex(LegalError, "missing or changed notices"):
            self.run_build()
        self.write("dependency/LICENSE", "license\n")
        self.write("notices/LEGAL.txt", "other notices\n")
        with self.assertRaisesRegex(LegalError, "does not embed the prepared notices"):
            self.run_build()
        self.write("notices/LEGAL.txt", self.report)
        self.binary.write_bytes(b"code" + self.payload)
        with self.assertRaisesRegex(LegalError, "only a development build's executable"):
            self.run_build()

    def test_the_source_offer_lies_under_its_own_name_and_source_txt_names_it(self) -> None:
        def vendor(command: list[str], **_: object) -> subprocess.CompletedProcess[str]:
            (Path(command[-1]) / "dependency-1.0.0").mkdir(parents=True)
            (Path(command[-1]) / "dependency-1.0.0/lib.rs").write_text("code\n")
            return subprocess.CompletedProcess(command, 0)

        self.write("notices/inventory.json", "{}\n")
        self.write("notices/old_third-party-source.tar.gz", "stale")
        build = Build("graphite-meter-client", "x86_64-pc-windows-gnu", "release", "1.2.3", self.out)
        with patch("scripts.legal.rust.subprocess.run", vendor):
            source_offer(build, self.state, self.state.components)
        base = "graphite-meter-client_1.2.3_windows_amd64_rust"
        self.assertEqual(sorted(path.name for path in self.out.glob("*_third-party-source.tar.gz")),
                         [f"{base}_third-party-source.tar.gz"])
        with tarfile.open(self.out / f"{base}_third-party-source.tar.gz") as archive:
            self.assertEqual(sorted(archive.getnames()), [f"{base}_third-party-source/{name}" for name in (
                "LEGAL.txt", "inventory.json", "legal/rust-forks.json", "third_party/cargo/dependency-1.0.0/lib.rs")])
        self.assertEqual((self.out / "SOURCE.txt").read_text().splitlines(), [
            "Graphite Meter source: https://github.com/zR-JB/graphite-meter/tree/v1.2.3", "Matching release: v1.2.3",
            f"Dependency source archive: {base}_third-party-source.tar.gz", "Rust target: x86_64-pc-windows-gnu"])
        self.assertEqual((self.out / "SOURCE.txt").read_text(), source_notice(
            Project.read(ROOT), "1.2.3", f"{base}_third-party-source.tar.gz", "x86_64-pc-windows-gnu"))


class PlatformTests(Scratch):
    NATIVE = SYSROOT + "lib/rustlib/t/lib/self-contained/libc.a"
    CRT1 = SYSROOT + "lib/rustlib/t/lib/self-contained/crt1.o"
    STD = SYSROOT + "lib/rustlib/t/lib/libstd-1.rlib"

    def setUp(self) -> None:
        super().setUp()
        self.sysroot = self.root / "sysroot"
        texts = {SYSROOT + "share/doc/rust/COPYRIGHT-library.html": "<p>library</p>\n",
                 SYSROOT + "share/doc/rust/licenses/MIT.txt": "MIT\n", str(self.root / "musl/COPYRIGHT"): "musl\n"}
        for path, content in {**texts, self.STD: "std", self.NATIVE: "libc"}.items():
            self.write(platform.source(path, self.sysroot).relative_to(self.root).as_posix(), content)
        self.listing = "".join(f"{path}\t{sha256(texts[path].encode())}\n" for path in sorted(texts))
        self.entry: dict[str, Any] = {
            "target": "t", "systemLibraries": ["kernel32.dll"], "reviewDecision": "approved",
            "reviewNotes": "reviewed", "description": "Platform notices.", "nativeInputs": [self.NATIVE],
            "notices": {str(self.root / "musl/COPYRIGHT"): "musl/COPYRIGHT"},
            "noticesSha256": sha256(self.listing.encode())}

    def notice(self, entry: dict[str, Any] | None = None, inputs: set[str] | None = None,
               libraries: set[str] | None = None) -> str:
        return platform.notice(self.entry if entry is None else entry, target="t", sysroot=self.sysroot,
                               inputs={self.NATIVE, self.STD} if inputs is None else inputs,
                               libraries={"kernel32.dll"} if libraries is None else libraries)

    def test_the_notice_lists_the_standard_library_then_the_record_texts(self) -> None:
        notice = self.notice()
        self.assertTrue(notice.startswith("Platform notices.\n\n--- rust-standard-library/COPYRIGHT-library.html"))
        self.assertTrue(notice.endswith("--- musl/COPYRIGHT ---\n\nmusl\n"))

    def test_changed_notice_texts_unreviewed_inputs_and_imports_and_missing_approval_are_refused(self) -> None:
        self.write("sysroot/lib/rustlib/t/lib/libstd-1.rlib", "new compiler output")
        self.notice()
        self.write("musl/COPYRIGHT", "changed\n")
        with self.assertRaisesRegex(LegalError, "platform notices changed"):
            self.notice()
        self.write("musl/COPYRIGHT", "musl\n")
        for change, inputs, libraries, message in (
            ({}, {self.STD, self.CRT1}, None, "native inputs lack review"),
            ({}, None, {"kernel32.dll", "evil.dll"}, "system libraries lack review"),
            ({"reviewDecision": "pending"}, None, None, "absent or unresolved"),
            ({"reviewNotes": ""}, None, None, "absent or unresolved"),
            ({"notices": {SYSROOT + "share/doc/rust/licenses/MIT.txt": "MIT"}}, None, None,
             "beyond the Rust standard"),
        ):
            with self.subTest(message=message), self.assertRaisesRegex(LegalError, message):
                self.notice(self.entry | change, inputs, libraries)

    def test_the_candidate_of_a_new_input_passes_once_approved(self) -> None:
        self.write("sysroot/lib/rustlib/t/lib/self-contained/crt1.o", "crt1")
        record, listing = platform.candidate(self.entry, target="t", sysroot=self.sysroot,
                                             inputs={self.STD, self.NATIVE, self.CRT1}, libraries={"kernel32.dll"})
        self.assertEqual((record["nativeInputs"], record["reviewDecision"], listing),
                         ([self.CRT1, self.NATIVE], "pending", self.listing))
        approved = record | {"reviewDecision": "approved", "reviewNotes": "reviewed"}
        self.notice(approved, inputs={self.STD, self.CRT1})

    def test_gnu_ld_and_lld_maps_name_the_native_inputs_outside_cargo(self) -> None:
        maps = {
            "GNU ld": "Archive member included to satisfy reference by file (symbol)\n\n"
                      "/usr/lib/libgcc.a(_ctors.o)\n                              /build/app.o (__CTOR_LIST__)\n"
                      "/rust/lib/rustlib/t/lib/libstd.rlib(std.o)\n                              /build/app.o (main)\n"
                      "/registry/crate/lib/libimport.a(stub.o)\n                              /build/app.o (Stub)\n\n"
                      "Linker script and memory map\n\nLOAD /usr/lib/gcc/../crt1.o\nLOAD /usr/lib/libunused.a\n"
                      "LOAD /build/app.o\n",
            "LLD": "             VMA              LMA     Size Align Out     In      Symbol\n"
                   "             2fc              2fc       20     4         /usr/lib/gcc/../crt1.o:(.note.ABI-tag)\n"
                   "            1000             1000       10     1         /usr/lib/libgcc.a(_ctors.o):(.text)\n"
                   "            1010             1010       10     1         /rust/lib/rustlib/t/lib/libstd.rlib(std.o):(.text)\n"
                   "            1020             1020       10     1         /build/app.o:(.text)\n",
        }
        for linker, listing in maps.items():
            with self.subTest(linker=linker):
                path = self.write("link.map", listing)
                self.assertEqual(platform.linked(path, Path("/rust"), {Path("/build"), Path("/registry/crate")}),
                                 {"/usr/lib/crt1.o", "/usr/lib/libgcc.a", SYSROOT + "lib/rustlib/t/lib/libstd.rlib"})
        with self.assertRaises(ControlPlaneError):
            platform.link_map(self.root, "../escape", "ci")

    def test_fetched_and_cached_notices_must_match_their_reviewed_bytes(self) -> None:
        relative = "legal/manual/musl/COPYRIGHT"
        sources = {"rustVersion": "pinned", relative: {"url": "https://example.invalid/COPYRIGHT",
                                                       "sha256": sha256(b"reviewed")}}
        self.enterContext(patch("scripts.legal.rust_platform.rust_channel", return_value="pinned"))
        entry: dict[str, Any] = {"notices": {relative: "musl/COPYRIGHT"}}
        for changed, message in (({"rustVersion": "old"}, "needs review for Rust pinned"),
                                 ({relative: {"url": "http://example.invalid/COPYRIGHT", "sha256": "0" * 64}},
                                  "HTTPS URL")):
            self.write("legal/rust-notice-sources.json", json.dumps(sources | changed))
            with self.subTest(changed=changed), self.assertRaisesRegex(LegalError, message):
                platform.fetch_notices(self.root, entry)
        self.write("legal/rust-notice-sources.json", json.dumps(sources))
        with patch("urllib.request.urlopen", return_value=io.BytesIO(b"changed")), \
                self.assertRaisesRegex(LegalError, "bytes differ"):
            platform.fetch_notices(self.root, entry)
        self.assertFalse((self.root / relative).exists())
        with patch("urllib.request.urlopen", return_value=io.BytesIO(b"reviewed")):
            platform.fetch_notices(self.root, entry)
        with patch("urllib.request.urlopen", side_effect=AssertionError("the cache works offline")):
            platform.fetch_notices(self.root, entry)
        self.write(relative, "tampered")
        with self.assertRaisesRegex(LegalError, "bytes differ"):
            platform.fetch_notices(self.root, entry)


class ManualProvenanceTests(Scratch):
    def setUp(self) -> None:
        super().setUp()
        self.enterContext(patch("scripts.legal.rust_platform.rust_channel", return_value="1.99.0"))
        license = self.write("legal/manual/rust/crate/LICENSE", "license\n")
        self.entry = {"ecosystem": "cargo", "name": "crate", "version": "1.0.0", "upstream": REGISTRY,
                      "licenseExpression": "MIT", "artifactScopes": ["rust"], "reviewNotes": "reviewed",
                      "localLegalFiles": [{"name": "legal/manual/rust/crate/LICENSE",
                                           "sha256": sha256(license.read_bytes()), "kind": "license"}]}
        self.write("legal/rust-notice-sources.json", json.dumps({"rustVersion": "1.99.0"}))

    def test_manual_texts_keep_their_hash_and_notice_sources_their_rust_release(self) -> None:
        self.write("legal/rust-provenance.json", marshal([self.entry]))
        self.assertEqual(manual_problems(self.root), [])
        for name, content, message in (
            ("legal/manual/rust/crate/LICENSE", "changed\n", "hash changed"),
            ("legal/rust-provenance.json", marshal([self.entry | {"artifactScopes": ["tui"]}]), "rust scope"),
            ("legal/rust-provenance.json", marshal([self.entry | {"reviewNotes": ""}]), "incomplete"),
            ("legal/rust-notice-sources.json", json.dumps({"rustVersion": "1.98.0"}), "review for Rust 1.99.0"),
        ):
            with self.subTest(name=name, message=message):
                original = (self.root / name).read_bytes()
                self.write(name, content)
                self.assertRegex(" ".join(manual_problems(self.root)), message)
                self.write(name, original)


if __name__ == "__main__":
    unittest.main()
