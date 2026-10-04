"""Rust release artifacts remain untrusted data throughout shared verification."""
from __future__ import annotations

import base64
import contextlib
import io
import json
import os
import shutil
import tempfile
import unittest
from itertools import product
from pathlib import Path
from unittest.mock import patch

from ..legal.model import manual_files, manual_sources
from ..legal.rust import DEVELOPMENT, DEVELOPMENT_NOTICE
from .fixtures import outcome, statement, write_archive
from .github_api import ControlPlaneError as VerificationError, JsonObject, file_sha256 as sha256_file, write_checksums
from .toolchains import rust_tui_targets
from .verify_release_assets import (
    TARGETS, expected_rust_artifacts, merge, read_archive, require_same, rust_builds, rust_files, stage_rust,
    tui_archive, verify_rust, verify_rust_client_archive, verify_rust_source,
)

# Each shipped platform's Rust target, as the builders read it.
RUST_TARGETS = rust_tui_targets(TARGETS)
# What the server image adds to its binary, which its source offer covers.
CA, = (entry for entry in manual_sources(Path("."), "graphite-meter-server") if entry.name == "ca-certificates")
REPOSITORY, COMMIT = "example/repo", "f" * 40


def executable(target: str) -> bytes:
    data = bytearray(256)
    arch = target.split("-")[0]
    if "-windows-" in target:
        data[:2], data[0x3C:0x40], data[0x80:0x84] = b"MZ", (0x80).to_bytes(4, "little"), b"PE\0\0"
        data[0x84:0x86] = {"x86_64": 0x8664, "aarch64": 0xAA64}[arch].to_bytes(2, "little")
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


def write_build(dist: Path, package: str, platform: str) -> dict[str, bytes]:
    """Write one Rust build's archive and source offer as the release verifier accepts them."""
    target = RUST_TARGETS[platform]
    files = {}
    if package == "graphite-meter-client":
        archive, base, binary = tui_archive("1.2.3", platform, "_rust")
        files = {
            f"{base}/{binary}": executable(target), f"{base}/THIRD_PARTY_NOTICES.txt": b"fixture notices\n",
            f"{base}/LICENSE": Path("LICENSE").read_bytes(), f"{base}/COPYRIGHT": Path("COPYRIGHT").read_bytes(),
            f"{base}/SOURCE.txt": f"{base}_third-party-source.tar.gz".encode()}
        write_archive(dist / archive, files, base)
    source, = (name for name in rust_files("1.2.3", package, platform) if name.endswith("_third-party-source.tar.gz"))
    write_source(dist / source, inventory(package, target))
    return files


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
            member = f"{base}/{binary}"
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as temporary:
                dist = Path(temporary)
                marker = dist / "executed"
                files = write_build(dist, "graphite-meter-client", platform)
                files[member] += f"touch '{marker}'\n".encode()
                arch, rest = target.split("-", 1)
                other = {"x86_64": "aarch64", "aarch64": "x86_64"}[arch] + "-" + rest
                with patch("subprocess.Popen", side_effect=AssertionError("artifact execution")):
                    for binary_data, error in ((files[member], None),
                                               (executable(other), f"does not hold a {target} executable"),
                                               (files[member] + DEVELOPMENT.encode(), "unreviewed development build")):
                        write_archive(dist / archive, files | {member: binary_data}, base)
                        outcome(self, error, lambda: verify_rust_client_archive(dist, "1.2.3", platform, target))
                self.assertFalse(marker.exists())

    def test_member_read_rejects_oversize_member(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "archive.tar.gz"
            write_archive(path, {"base/SOURCE.txt": b" " * 32}, "base")
            with self.assertRaisesRegex(VerificationError, "exceeds limit"):
                read_archive(path, "base/SOURCE.txt", limit=16)

    def test_artifacts_arrive_once_in_exactly_the_selection(self) -> None:
        names = expected_rust_artifacts("1.2.3", *rust_builds("both"))
        windows = {name for name in names if "_windows_" in name}
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for part, files in (("linux", names - windows), ("windows", windows)):
                (root / part).mkdir()
                for name in files:
                    (root / part / name).write_text(name)
                write_checksums(root / part)
            merged = merge(root / "linux", root / "handoff")
            with self.assertRaisesRegex(VerificationError, "missing="):
                require_same("Rust artifacts", names, merged)
            require_same("Rust artifacts", names, merged | merge(root / "windows", root / "handoff"))
            with self.assertRaisesRegex(VerificationError, "more than one artifact"):
                merge(root / "windows", root / "handoff")


class RustServerReleaseTests(unittest.TestCase):
    def test_server_source_identity_and_component_presence(self) -> None:
        target = RUST_TARGETS["linux/arm64"]
        image: JsonObject = {"imageComponents": [{"ecosystem": CA.ecosystem, "name": CA.name, "version": CA.version}]}
        manual = {path: b"reviewed" for path in manual_files(CA)}
        mutations: dict[str, tuple[JsonObject, dict[str, bytes]]] = {
            "valid": ({}, {}),
            "cargo_fixture": ({}, {"third_party/cargo/example-1.0/tests/test_vector.pem": b"public upstream fixture"}),
            "package": ({"package": "graphite-meter-client"}, {}),
            "target": ({"target": RUST_TARGETS["linux/amd64"]}, {}),
            "target_libc": ({"target": target.replace("-musl", "-gnu")}, {}),
            "lock": ({"cargoLockSha256": "0" * 64}, {}),
            "missing": ({"components": [{"component": {"name": "absent", "version": "1.0"}}]}, {}),
            "first_party_key": ({}, {"rust/.dev-certs/private.key": b"must not ship"}),
            "undeclared_tree": ({}, {"third_party/cargo/other-2.0/tests/key.pem": b"not in inventory"}),
            "schema_boolean": ({"schemaVersion": True}, {}),
            "component_array": ({"components": {"component": "not an array"}}, {}),
            "component_name": ({"components": [{"component": {"name": [], "version": "1.0"}}]}, {}),
            "browser_component": ({"browserComponents": ["not an object"]}, {}),
            "image_component": (image, manual),
            "image_notice_missing": (image, {}),
            "image_unreviewed": (
                {"imageComponents": [{"ecosystem": CA.ecosystem, "name": "unreviewed", "version": CA.version}]}, manual),
            "development_notices": ({}, {"LEGAL.txt": DEVELOPMENT_NOTICE.encode() + b"fixture notices\n"}),
        }
        for mutation, (change, extra) in mutations.items():
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                metadata = inventory("graphite-meter-server", target) | change
                path = Path(temporary) / "source.tar.gz"
                write_source(path, metadata, extra)
                with patch("subprocess.Popen", side_effect=AssertionError("artifact execution")):
                    outcome(self, None if mutation in {"valid", "cargo_fixture", "image_component"} else "",
                            lambda: verify_rust_source(path, "graphite-meter-server", target))


class RustStagingTests(unittest.TestCase):
    """A request's exports are staged as release-request.yml does and verified as release.py does."""

    SERVER, TUI = rust_builds("both")

    def test_ci_checks_its_exports_with_the_release_commands(self) -> None:
        from .release import COMMANDS

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            write_exports(root / "export", self.SERVER, self.TUI)
            environment = {
                "RUNNER_TEMP": str(root), "RUST": "both", "VERSION": "1.2.3",
                "RUST_EXPORT": str(root / "export"), "RUST_ASSETS": str(root / "staged"),
                "GITHUB_SHA": COMMIT, "GITHUB_REPOSITORY": REPOSITORY,
            }
            with patch.dict(os.environ, environment), contextlib.redirect_stdout(io.StringIO()):
                COMMANDS["stage-rust"]()
                self.assertEqual({path.name for path in (root / "staged").iterdir()},
                                 expected_rust_artifacts("1.2.3", self.SERVER, self.TUI) | {"checksums.txt"})
                COMMANDS["check-rust"]()

    def test_staging_refuses_an_export_it_cannot_account_for(self) -> None:
        arm64 = Path("server") / self.SERVER[1].replace("/", "_")
        source, = rust_files("1.2.3", "graphite-meter-server", self.SERVER[1])
        for remove, error in ((arm64, "lacks"), (arm64 / source, "attests no single expected Rust export")):
            with self.subTest(error=error), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                write_exports(root / "export", self.SERVER, self.TUI)
                path = root / "export" / remove
                shutil.rmtree(path) if path.is_dir() else path.unlink()
                with self.assertRaisesRegex(VerificationError, error):
                    stage_rust(root / "export", root / "staged", "1.2.3", self.SERVER, self.TUI)

    def test_each_statement_binds_exactly_its_files_to_the_release_commit(self) -> None:
        server, source = self.SERVER[:1], rust_files("1.2.3", "graphite-meter-server", self.SERVER[0])
        name = f"graphite-meter-server_1.2.3_{server[0].replace('/', '_')}_rust.provenance.json"
        archive = tui_archive("1.2.3", self.TUI[0], "_rust")[0]

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
                write_exports(root / "export", server, self.TUI)
                stage_rust(root / "export", root / "staged", "1.2.3", server, self.TUI)
                if edit is not None:
                    edit(root / "staged")
                    write_checksums(root / "staged")
                outcome(self, error, lambda: verify_rust([root / "staged"], root / "assets", "1.2.3", server,
                                                         self.TUI, COMMIT, REPOSITORY))


class RustPrereleaseSourceTests(unittest.TestCase):
    def test_a_prerelease_is_checked_against_the_files_of_its_pr_head(self) -> None:
        from .fixtures import github
        from .release import RUST_SOURCE_FILES, fetch_files

        forks = json.loads(Path("legal/rust-forks.json").read_text())
        bumped = json.dumps([dict(forks[0], rev="0" * 40), *forks[1:]]).encode()
        head = {name: Path(name).read_bytes() for name in RUST_SOURCE_FILES} | {"legal/rust-forks.json": bumped}

        def contents(encoding: str = "base64") -> dict[str, object]:
            return {f"repos/{REPOSITORY}/contents/{name}?ref={COMMIT}":
                    {"encoding": encoding, "content": base64.encodebytes(data).decode()} for name, data in head.items()}

        target = RUST_TARGETS["linux/amd64"]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_source(root / "source.tar.gz", inventory("graphite-meter-client", target),
                         {"legal/rust-forks.json": bumped})
            with github(contents()):
                source = fetch_files(REPOSITORY, COMMIT, root / "head")
            verify_rust_source(root / "source.tar.gz", "graphite-meter-client", target, source)
            # Main's files would refuse every PR that bumps a fork.
            with self.assertRaisesRegex(VerificationError, "fork identities differ"):
                verify_rust_source(root / "source.tar.gz", "graphite-meter-client", target)
            with github(contents("none")), self.assertRaisesRegex(VerificationError, "not bounded base64"):
                fetch_files(REPOSITORY, COMMIT, root / "other")


class RustRequestBoundaryTests(unittest.TestCase):
    def test_dispatch_selection_cannot_be_forged_in_the_artifact(self) -> None:
        from .fixtures import git_head, github
        from .release import OCI, RUST_OCI, Release, request_title, verify_request
        from .test_trust import MAIN, HEAD, REPO, REQUEST_RUN, ARTIFACTS, artifacts, dispatch_run, trusted

        for stable, selection, forged in product((True, False), ("none", "server", "tui", "both"), (False, True)):
            with self.subTest(stable=stable, selection=selection, forged=forged), \
                    tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                request = root / "request"
                candidate = request / "release-request-4242"
                candidate.mkdir(parents=True)
                release = Release("v1.2.3" if stable else "v1.2.3-rc.1",
                                  MAIN if stable else HEAD, 0 if stable else 101, selection)
                files = {OCI, f"{OCI}.sha256"} | ({RUST_OCI, f"{RUST_OCI}.sha256"} if release.rust_server else set())
                for name in files:
                    (candidate / name).write_text("untrusted data")
                (candidate / "request.json").write_text(json.dumps({
                    "schemaVersion": 3, "repository": REPO, "tag": release.tag, "sourceSha": release.sha,
                    "pr": release.pr, "mode": "validate", "requestRunId": 4242, "requestRunAttempt": 1,
                    "rust": selection,
                }))
                names = [candidate.name, "release-assets-4242"]
                names += ["release-rust-assets-4242"] * (selection != "none")
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
                    "WORKFLOW_REF": f"{REPO}/.github/workflows/release.yml@refs/heads/main", "REQUEST_RUN_ID": "4242",
                } | git_head(root, MAIN)
                with patch.dict(os.environ, environment), github(responses):
                    result = outcome(self, "dispatch inputs" if forged else None, lambda: verify_request(request))
                    self.assertEqual(result, None if forged else (release, False))
