"""Rust release artifacts remain untrusted data throughout shared verification."""
from __future__ import annotations

import io
import os
import json
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

from .fixtures import write_checksums
from .github_api import ControlPlaneError as VerificationError, JsonObject, file_sha256 as sha256_file
from .verify_release_assets import (
    TARGETS, expected_rust_artifacts, read_archive, require_same, tui_archive, tui_targets, verify_rust_artifacts,
    verify_rust_client_archive, verify_rust_source,
)

# Each shipped platform's Rust target, as the builders read it.
RUST_TARGETS = tui_targets(TARGETS)


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


def write_archive(path: Path, base: str, files: dict[str, bytes]) -> None:
    if path.suffix == ".zip":
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr(f"{base}/", b"")
            for name, payload in files.items():
                archive.writestr(name, payload)
        return
    with tarfile.open(path, "w:gz") as archive:
        directory = tarfile.TarInfo(base)
        directory.type = tarfile.DIRTYPE
        archive.addfile(directory)
        for name, payload in files.items():
            member = tarfile.TarInfo(name)
            member.size, member.mode = len(payload), 0o755
            archive.addfile(member, io.BytesIO(payload))


def write_source(path: Path, inventory: JsonObject, extra: dict[str, bytes] | None = None) -> None:
    members = {
        "inventory.json": json.dumps(inventory).encode(), "LEGAL.txt": b"fixture notices\n",
        "legal/rust-forks.json": Path("legal/rust-forks.json").read_bytes(),
        "third_party/cargo/example-1.0/source.rs": b"fixture",
    } | (extra or {})
    with tarfile.open(path, "w:gz") as archive:
        for name, payload in members.items():
            member = tarfile.TarInfo(name)
            member.size = len(payload)
            archive.addfile(member, io.BytesIO(payload))


def inventory(package: str, target: str) -> JsonObject:
    return {"schemaVersion": 1, "package": package, "profile": "release", "target": target,
            "cargoLockSha256": sha256_file(Path("rust/Cargo.lock")),
            "components": [{"component": {"name": "example", "version": "1.0"}}]}


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
                write_archive(dist / archive, base, files)
                write_source(dist / f"{base}_third-party-source.tar.gz", inventory("graphite-meter-client", target))
                arch, rest = target.split("-", 1)
                other = {"x86_64": "aarch64", "aarch64": "x86_64"}[arch] + "-" + rest
                with patch("subprocess.Popen", side_effect=AssertionError("artifact execution")):
                    verify_rust_client_archive(dist, "1.2.3", platform, target)
                    write_archive(dist / archive, base, files | {f"{base}/{binary}": executable(other)})
                    with self.assertRaisesRegex(VerificationError, f"does not hold a {target} executable"):
                        verify_rust_client_archive(dist, "1.2.3", platform, target)
                self.assertFalse(marker.exists())

    def test_member_read_rejects_oversize_member(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "archive.tar.gz"
            write_archive(path, "base", {"base/SOURCE.txt": b" " * 32})
            with self.assertRaisesRegex(VerificationError, "exceeds limit"):
                read_archive(path, "base/SOURCE.txt", limit=16)

    def test_artifacts_arrive_once_in_exactly_the_selection(self) -> None:
        from .release import merge

        names = expected_rust_artifacts("1.2.3", "both")
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
            verify_rust_artifacts(dist, "1.2.3", "server")
            # The image ships the static musl server; a glibc build for the same machine is another binary.
            source("arm64", RUST_TARGETS["linux/arm64"].replace("-musl", "-gnu"))
            with self.assertRaisesRegex(VerificationError, "invalid Rust build identity"):
                verify_rust_artifacts(dist, "1.2.3", "server")

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
                extra = {
                    "cargo_fixture": {"third_party/cargo/example-1.0/tests/test_vector.pem": b"public upstream fixture"},
                    "first_party_key": {"rust/.dev-certs/private.key": b"must not ship"},
                    "undeclared_tree": {"third_party/cargo/other-2.0/tests/key.pem": b"not in inventory"},
                }.get(mutation)
                path = Path(temporary) / "source.tar.gz"
                write_source(path, metadata, extra)
                with patch("subprocess.Popen", side_effect=AssertionError("artifact execution")):
                    if mutation in {"valid", "cargo_fixture"}:
                        verify_rust_source(path, "graphite-meter-server", target)
                    else:
                        with self.assertRaises(VerificationError):
                            verify_rust_source(path, "graphite-meter-server", target)


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
