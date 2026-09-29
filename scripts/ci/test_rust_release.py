"""Rust release artifacts remain untrusted data throughout shared verification."""
from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from ..legal.model import manual_files, manual_sources
from .fixtures import statement, write_archive
from .github_api import ControlPlaneError as VerificationError, JsonObject, file_sha256 as sha256_file, write_checksums
from .toolchains import tui_targets
from .verify_release_assets import (
    TARGETS, expected_rust_artifacts, merge, read_archive, require_same, rust_builds, rust_files, stage_rust,
    tui_archive, verify_rust, verify_rust_artifacts, verify_rust_client_archive, verify_rust_source,
)

# Each shipped platform's Rust target, as the builders read it.
RUST_TARGETS = tui_targets(TARGETS)
# What the server image adds to its binary, which its source offer covers.
CA, = (entry for entry in manual_sources(Path("."), "graphite-meter-server") if entry.name == "ca-certificates")
REPOSITORY, COMMIT = "example/repo", "f" * 40


def executable(target: str) -> bytes:
    data = bytearray(256)
    arch = target.split("-")[0]
    if "-windows-" in target:
        data[:2], data[0x3C:0x40], data[0x80:0x84] = b"MZ", (0x80).to_bytes(4, "little"), b"PE\0\0"
        data[0x84:0x86] = {"x86_64": 0x8664, "aarch64": 0xAA64}[arch].to_bytes(2, "little")
    elif "-apple-" in target:
        data[:4] = (0xFEEDFACF).to_bytes(4, "little")
        data[4:8] = {"x86_64": 0x01000007, "aarch64": 0x0100000C}[arch].to_bytes(4, "little")
    else:
        data[:7], data[16:18], data[20:24], data[52:54] = (
            b"\x7fELF\x02\x01\x01", (3).to_bytes(2, "little"), (1).to_bytes(4, "little"), (64).to_bytes(2, "little"))
        data[18:20] = {"x86_64": 62, "aarch64": 183}[arch].to_bytes(2, "little")
    return bytes(data)


def write_source(path: Path, inventory: JsonObject, extra: dict[str, bytes] | None = None) -> None:
    write_archive(path, {
        "inventory.json": json.dumps(inventory).encode(), "LEGAL.txt": b"fixture notices\n",
        "legal/rust-forks.json": Path("legal/rust-forks.json").read_bytes(),
        "third_party/cargo/example-1.0/source.rs": b"fixture",
    } | (extra or {}))


def inventory(package: str, target: str) -> JsonObject:
    return {"schemaVersion": 1, "package": package, "profile": "release", "target": target,
            "cargoLockSha256": sha256_file(Path("rust/Cargo.lock")),
            "components": [{"component": {"name": "example", "version": "1.0"}}]}


def write_build(dist: Path, package: str, platform: str) -> None:
    """Write one Rust build's archive and source offer as the release verifier accepts them."""
    target = RUST_TARGETS[platform]
    if package == "graphite-meter-client":
        archive, base, binary = tui_archive("1.2.3", platform, "_rust")
        write_archive(dist / archive, {
            f"{base}/{binary}": executable(target), f"{base}/THIRD_PARTY_NOTICES.txt": b"fixture notices\n",
            f"{base}/LICENSE": Path("LICENSE").read_bytes(), f"{base}/COPYRIGHT": Path("COPYRIGHT").read_bytes(),
            f"{base}/SOURCE.txt": f"{base}_third-party-source.tar.gz".encode()}, base)
    source, = (name for name in rust_files("1.2.3", package, platform) if name.endswith("_third-party-source.tar.gz"))
    write_source(dist / source, inventory(package, target))


def write_exports(root: Path, server: list[str], tui: list[str], revision: str = COMMIT) -> None:
    """Write BuildKit's local exports of a request, each platform's statement beside the files it attests.

    Like BuildKit, an export of several platforms gets a directory per platform, and the TUI export one
    stray attestation that no release expects.
    """
    exports = {root / "tui": [("graphite-meter-client", platform) for platform in tui]}
    for platform in server:
        exports[root / "server" / (platform.replace("/", "_") if len(server) > 1 else "")] = [
            ("graphite-meter-server", platform)]
    for directory, builds in exports.items():
        if not builds:
            continue
        directory.mkdir(parents=True)
        for package, platform in builds:
            write_build(directory, package, platform)
        subjects = {path.name: sha256_file(path) for path in sorted(directory.iterdir())}
        (directory / "provenance.json").write_text(json.dumps(
            statement(REPOSITORY, revision, remote=False, subjects=subjects)))
        (directory / "sbom.spdx.json").write_text("{}")


class RustArchiveBoundaryTests(unittest.TestCase):
    def test_executable_member_is_never_executed(self) -> None:
        for platform, target in RUST_TARGETS.items():
            archive, base, binary = tui_archive("1.2.3", platform, "_rust")
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as temporary:
                dist = Path(temporary)
                marker = dist / "executed"
                files = {
                    f"{base}/{binary}": executable(target) + f"touch '{marker}'\n".encode(),
                    f"{base}/THIRD_PARTY_NOTICES.txt": b"fixture notices\n",
                    f"{base}/LICENSE": Path("LICENSE").read_bytes(),
                    f"{base}/COPYRIGHT": Path("COPYRIGHT").read_bytes(),
                    f"{base}/SOURCE.txt": f"{base}_third-party-source.tar.gz".encode(),
                }
                write_archive(dist / archive, files, base)
                write_source(dist / f"{base}_third-party-source.tar.gz", inventory("graphite-meter-client", target))
                arch, rest = target.split("-", 1)
                other = {"x86_64": "aarch64", "aarch64": "x86_64"}[arch] + "-" + rest
                with patch("subprocess.Popen", side_effect=AssertionError("artifact execution")):
                    verify_rust_client_archive(dist, "1.2.3", platform, target)
                    write_archive(dist / archive, files | {f"{base}/{binary}": executable(other)}, base)
                    with self.assertRaisesRegex(VerificationError, f"does not hold a {target} executable"):
                        verify_rust_client_archive(dist, "1.2.3", platform, target)
                self.assertFalse(marker.exists())

    def test_member_read_rejects_oversize_member(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "archive.tar.gz"
            write_archive(path, {"base/SOURCE.txt": b" " * 32}, "base")
            with self.assertRaisesRegex(VerificationError, "exceeds limit"):
                read_archive(path, "base/SOURCE.txt", limit=16)

    def test_artifacts_arrive_once_in_exactly_the_selection(self) -> None:
        names = expected_rust_artifacts("1.2.3", *rust_builds("both"))
        darwin = {name for name in names if "_darwin_" in name}
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for part, files in (("linux", names - darwin), ("darwin", darwin)):
                (root / part).mkdir()
                for name in files:
                    (root / part / name).write_text(name)
                write_checksums(root / part)
            merged = merge(root / "linux", root / "handoff")
            with self.assertRaisesRegex(VerificationError, "missing="):
                require_same("Rust artifacts", names, merged)
            require_same("Rust artifacts", names, merged | merge(root / "darwin", root / "handoff"))
            with self.assertRaisesRegex(VerificationError, "more than one artifact"):
                merge(root / "darwin", root / "handoff")


class RustServerReleaseTests(unittest.TestCase):
    def test_each_server_source_names_the_listed_linux_target(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)

            def source(arch: str, target: str) -> None:
                write_source(dist / f"graphite-meter-server_1.2.3_linux_{arch}_rust_third-party-source.tar.gz",
                             inventory("graphite-meter-server", target))

            for arch in ("amd64", "arm64"):
                source(arch, RUST_TARGETS[f"linux/{arch}"])
            verify_rust_artifacts(dist, "1.2.3", *rust_builds("server"))
            # The image ships the static musl server; a glibc build for the same machine is another binary.
            source("arm64", RUST_TARGETS["linux/arm64"].replace("-musl", "-gnu"))
            with self.assertRaisesRegex(VerificationError, "invalid Rust build identity"):
                verify_rust_artifacts(dist, "1.2.3", *rust_builds("server"))

    def test_server_source_identity_and_component_presence(self) -> None:
        target = RUST_TARGETS["linux/arm64"]
        for mutation in (
            "valid",
            "cargo_fixture",
            "package",
            "target",
            "lock",
            "missing",
            "first_party_key",
            "undeclared_tree",
            "schema_boolean",
            "component_array",
            "component_name",
            "browser_component",
            "image_component",
            "image_notice_missing",
            "image_unreviewed",
        ):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                metadata = inventory("graphite-meter-server", target)
                if mutation == "package":
                    metadata["package"] = "graphite-meter-client"
                if mutation == "target":
                    metadata["target"] = RUST_TARGETS["linux/amd64"]
                if mutation == "lock":
                    metadata["cargoLockSha256"] = "0" * 64
                if mutation == "schema_boolean":
                    metadata["schemaVersion"] = True
                if mutation == "component_array":
                    metadata["components"] = {"component": "not an array"}
                if mutation == "component_name":
                    metadata["components"] = [{"component": {"name": [], "version": "1.0"}}]
                if mutation == "browser_component":
                    metadata["browserComponents"] = ["not an object"]
                if mutation == "missing":
                    metadata["components"] = [{"component": {"name": "absent", "version": "1.0"}}]
                if mutation.startswith("image"):
                    name = "unreviewed" if mutation == "image_unreviewed" else CA.name
                    metadata["imageComponents"] = [{"ecosystem": CA.ecosystem, "name": name, "version": CA.version}]
                extra = {
                    "cargo_fixture": {"third_party/cargo/example-1.0/tests/test_vector.pem": b"public upstream fixture"},
                    "first_party_key": {"rust/.dev-certs/private.key": b"must not ship"},
                    "undeclared_tree": {"third_party/cargo/other-2.0/tests/key.pem": b"not in inventory"},
                    "image_component": {path: b"reviewed" for path in manual_files(CA)},
                    "image_unreviewed": {path: b"reviewed" for path in manual_files(CA)},
                }.get(mutation)
                path = Path(temporary) / "source.tar.gz"
                write_source(path, metadata, extra)
                with patch("subprocess.Popen", side_effect=AssertionError("artifact execution")):
                    if mutation in {"valid", "cargo_fixture", "image_component"}:
                        verify_rust_source(path, "graphite-meter-server", target)
                    else:
                        with self.assertRaises(VerificationError):
                            verify_rust_source(path, "graphite-meter-server", target)


class RustStagingTests(unittest.TestCase):
    """A request's exports are staged as release-request.yml does and verified as release.py does."""

    SERVER, TUI = rust_builds("both")
    DOCKER = [platform for platform in TUI if not platform.startswith("darwin/")]

    def test_a_staged_request_passes_the_release_verification(self) -> None:
        # A release request exports every server platform; CI exports only the first.
        for server, macos in ((self.SERVER, True), (self.SERVER[:1], False)):
            with self.subTest(server=server), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                write_exports(root / "export", server, self.DOCKER)
                stage_rust(root / "export", root / "staged", "1.2.3", server, self.TUI)
                self.assertEqual({path.name for path in (root / "staged").iterdir()},
                                 expected_rust_artifacts("1.2.3", server, self.DOCKER) | {"checksums.txt"})
                parts, tui = [root / "staged"], self.DOCKER
                if macos:
                    (root / "darwin").mkdir()
                    for platform in set(self.TUI) - set(self.DOCKER):
                        write_build(root / "darwin", "graphite-meter-client", platform)
                    write_checksums(root / "darwin")
                    parts, tui = parts + [root / "darwin"], self.TUI
                verify_rust(parts, root / "assets", "1.2.3", server, tui, COMMIT, REPOSITORY)

    def test_ci_stages_and_checks_its_exports_with_the_release_commands(self) -> None:
        from .release import command_check_rust, command_stage_rust

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            write_exports(root / "export", self.SERVER[:1], self.DOCKER)
            environment = {
                "RUNNER_TEMP": str(root), "RUST": "both", "RUST_SERVER": self.SERVER[0], "VERSION": "1.2.3",
                "RUST_EXPORT": str(root / "export"), "RUST_ASSETS": str(root / "staged"),
                "GITHUB_SHA": COMMIT, "GITHUB_REPOSITORY": REPOSITORY,
            }
            with patch.dict(os.environ, environment), contextlib.redirect_stdout(io.StringIO()):
                command_stage_rust()
                command_check_rust()
                with patch.dict(os.environ, {"RUST_SERVER": "linux/s390x"}), \
                        self.assertRaisesRegex(VerificationError, "RUST_SERVER"):
                    command_stage_rust()

    def test_staging_refuses_an_export_it_cannot_account_for(self) -> None:
        arm64 = Path("server") / self.SERVER[1].replace("/", "_")
        source, = rust_files("1.2.3", "graphite-meter-server", self.SERVER[1])
        for remove, error in ((arm64, "lacks"), (arm64 / source, "attests no single expected Rust export")):
            with self.subTest(error=error), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                write_exports(root / "export", self.SERVER, self.DOCKER)
                path = root / "export" / remove
                shutil.rmtree(path) if path.is_dir() else path.unlink()
                with self.assertRaisesRegex(VerificationError, error):
                    stage_rust(root / "export", root / "staged", "1.2.3", self.SERVER, self.TUI)

    def test_each_statement_binds_exactly_its_files_to_the_release_commit(self) -> None:
        server, source = self.SERVER[:1], rust_files("1.2.3", "graphite-meter-server", self.SERVER[0])
        name = f"graphite-meter-server_1.2.3_{server[0].replace('/', '_')}_rust.provenance.json"
        archive = tui_archive("1.2.3", self.DOCKER[0], "_rust")[0]

        def restate(staged: Path, **change: object) -> None:
            subjects = {file: sha256_file(staged / file) for file in source}
            arguments: dict = {"repository": REPOSITORY, "revision": COMMIT, "remote": False} | change
            (staged / name).write_text(json.dumps(statement(subjects=subjects, **arguments)))

        def swap(staged: Path) -> None:
            (staged / archive).write_bytes((staged / archive).read_bytes() + b"swapped")

        for edit, error in (
            (None, None),
            (lambda staged: restate(staged, remote=True), None),
            (lambda staged: restate(staged, revision="e" * 40), "records source e"),
            (lambda staged: restate(staged, repository="example/fork", remote=True), "provenance built"),
            (lambda staged: (staged / name).write_text(json.dumps({"_type": "other"})), "not an in-toto"),
            (lambda staged: (staged / name).write_text(json.dumps(
                statement(REPOSITORY, COMMIT, remote=False, subjects={}))), "does not attest exactly"),
            (swap, "does not attest exactly"),
        ):
            with self.subTest(error=error), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                write_exports(root / "export", server, self.DOCKER)
                stage_rust(root / "export", root / "staged", "1.2.3", server, self.DOCKER)
                if edit is not None:
                    edit(root / "staged")
                    write_checksums(root / "staged")

                def verify() -> None:
                    verify_rust([root / "staged"], root / "assets", "1.2.3", server, self.DOCKER, COMMIT, REPOSITORY)

                if error is None:
                    verify()
                else:
                    with self.assertRaisesRegex(VerificationError, error):
                        verify()


class RustRequestBoundaryTests(unittest.TestCase):
    def test_dispatch_selection_cannot_be_forged_in_the_artifact(self) -> None:
        from .fixtures import git_head, github
        from .release import OCI, Release, request_title, verify_request
        from .test_trust import (MAIN, HEAD, REPO, REQUEST_RUN, ARTIFACTS,
                                artifacts, dispatch_run, trusted)

        for stable in (True, False):
            for selection in ("none", "server", "tui", "both"):
                for forged in (False, True):
                    with self.subTest(stable=stable, selection=selection, forged=forged), \
                            tempfile.TemporaryDirectory() as temporary:
                        root = Path(temporary)
                        request = root / "request"
                        candidate = request / "release-request-4242"
                        candidate.mkdir(parents=True)
                        release = Release("v1.2.3" if stable else "v1.2.3-rc.1",
                                          MAIN if stable else HEAD, 0 if stable else 101, selection)
                        files = {OCI, f"{OCI}.sha256"}
                        if release.rust_server:
                            files |= {"graphite-meter-rust.oci.tar", "graphite-meter-rust.oci.tar.sha256"}
                        for name in files:
                            (candidate / name).write_text("untrusted data")
                        (candidate / "request.json").write_text(json.dumps({
                            "schemaVersion": 3, "repository": REPO, "tag": release.tag,
                            "sourceSha": release.sha, "pr": release.pr, "mode": "validate",
                            "requestRunId": 4242, "requestRunAttempt": 1, "rust": selection,
                        }))
                        names = [candidate.name] + ["release-assets-4242"] * stable
                        names += ["release-rust-assets-4242"] * (selection != "none")
                        names += ["release-rust-darwin-4242"] * release.rust_tui
                        for name in names[1:]:
                            (request / name).mkdir()
                        dispatched = Release(release.tag, release.sha, release.pr,
                                             "tui" if selection == "none" else "none") if forged else release
                        responses = trusted(stable, "validate") | {
                            REQUEST_RUN: dispatch_run(31337, 4242, request_title("validate", dispatched, MAIN)),
                            ARTIFACTS: artifacts(*names),
                        }
                        environment = {
                            "REPOSITORY": REPO, "REPOSITORY_OWNER": "zR-JB", "PUBLISHER_SHA": MAIN,
                            "WORKFLOW_REF": f"{REPO}/.github/workflows/release.yml@refs/heads/main",
                            "REQUEST_RUN_ID": "4242",
                        } | git_head(root, MAIN)
                        with patch.dict(os.environ, environment), github(responses):
                            if forged:
                                with self.assertRaisesRegex(VerificationError, "dispatch inputs"):
                                    verify_request(request)
                            else:
                                self.assertEqual(verify_request(request), (release, False))
