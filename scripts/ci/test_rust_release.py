"""Rust release artifacts remain untrusted data throughout shared verification."""
from __future__ import annotations

import io
import os
import json
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from github_api import ControlPlaneError as VerificationError, JsonObject, file_sha256 as sha256_file
from verify_release_assets import read_tar_text, verify_rust_client_archive, verify_rust_server_source, expected_rust_artifacts


class RustArchiveBoundaryTests(unittest.TestCase):
    def test_executable_member_is_never_executed(self) -> None:
        version = "1.2.3"
        base = f"graphite-meter-client_{version}_linux_amd64_rust"
        metadata = {
            "schemaVersion": 1, "implementation": "rust", "version": "1.2.3-rust",
            "target": "x86_64-unknown-linux-gnu", "minimumGlibc": "2.36",
            "neededLibraries": ["libc.so.6"], "rustc": "rustc test-fixture\n",
        }
        inventory: JsonObject = {
            "schemaVersion": 1, "package": "graphite-meter-client", "profile": "release",
            "target": metadata["target"], "rustc": metadata["rustc"],
            "cargoLockSha256": sha256_file(Path("rust/Cargo.lock")),
            "components": [{"component": {"name": "example", "version": "1.0"}}],
        }
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            marker = dist / "executed"
            header = bytearray(64)
            header[:7] = b"\x7fELF\x02\x01\x01"
            header[16:18] = (3).to_bytes(2, "little")
            header[18:20] = (62).to_bytes(2, "little")
            header[20:24] = (1).to_bytes(4, "little")
            header[52:54] = (64).to_bytes(2, "little")
            files = {
                f"{base}/graphite-meter-client": bytes(header) + f"touch '{marker}'\n".encode(),
                f"{base}/BUILD.json": json.dumps(metadata).encode(),
                f"{base}/LEGAL.txt": b"fixture notices\n",
                f"{base}/LICENSE": Path("LICENSE").read_bytes(),
                f"{base}/COPYRIGHT": Path("COPYRIGHT").read_bytes(),
                f"{base}/SOURCE.txt": f"{base}_third-party-source.tar.gz glibc 2.36".encode(),
            }
            with tarfile.open(dist / f"{base}.tar.gz", "w:gz") as archive:
                directory = tarfile.TarInfo(base)
                directory.type = tarfile.DIRTYPE
                archive.addfile(directory)
                for name, payload in files.items():
                    member = tarfile.TarInfo(name)
                    member.size = len(payload)
                    member.mode = 0o755
                    archive.addfile(member, io.BytesIO(payload))
            with tarfile.open(dist / f"{base}_third-party-source.tar.gz", "w:gz") as archive:
                for name, payload in {
                    "inventory.json": json.dumps(inventory).encode(),
                    "LEGAL.txt": b"fixture notices\n", "legal/rust-forks.json": Path("legal/rust-forks.json").read_bytes(),
                    "third_party/cargo/example-1.0/source.rs": b"fixture",
                }.items():
                    member = tarfile.TarInfo(name)
                    member.size = len(payload)
                    archive.addfile(member, io.BytesIO(payload))
            with patch("subprocess.Popen", side_effect=AssertionError("artifact execution")):
                verify_rust_client_archive(dist, version)
                header[18:20] = (183).to_bytes(2, "little")
                files[f"{base}/graphite-meter-client"] = bytes(header)
                with tarfile.open(dist / f"{base}.tar.gz", "w:gz") as archive:
                    directory = tarfile.TarInfo(base)
                    directory.type = tarfile.DIRTYPE
                    archive.addfile(directory)
                    for name, payload in files.items():
                        member = tarfile.TarInfo(name)
                        member.size = len(payload)
                        archive.addfile(member, io.BytesIO(payload))
                with self.assertRaisesRegex(VerificationError, "AMD64 ELF"):
                    verify_rust_client_archive(dist, version)
            self.assertFalse(marker.exists())

    def test_metadata_read_rejects_oversize_member(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "metadata.tar.gz"
            with tarfile.open(path, "w:gz") as archive:
                member = tarfile.TarInfo("BUILD.json")
                member.size = 32
                archive.addfile(member, io.BytesIO(b" " * 32))
            with self.assertRaisesRegex(VerificationError, "exceeds limit"):
                read_tar_text(path, "BUILD.json", limit=16)


class RustServerReleaseTests(unittest.TestCase):
    def test_assets_require_exactly_the_dispatched_rust_selection(self) -> None:
        from fixtures import write_release_assets
        from verify_release_assets import verify_artifacts

        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            write_release_assets(dist, "1.2.3")
            name = next(iter(expected_rust_artifacts("1.2.3", "server")))
            inventory: JsonObject = {
                "schemaVersion": 1, "package": "graphite-meter-server", "profile": "release",
                "target": "x86_64-unknown-linux-gnu",
                "cargoLockSha256": sha256_file(Path("rust/Cargo.lock")),
                "components": [{"component": {"name": "example", "version": "1.0"}}],
            }
            with tarfile.open(dist / name, "w:gz") as archive:
                for filename, payload in {
                    "inventory.json": json.dumps(inventory).encode(), "LEGAL.txt": b"notices",
                    "legal/rust-forks.json": Path("legal/rust-forks.json").read_bytes(),
                    "third_party/cargo/example-1.0/source.rs": b"source",
                }.items():
                    member = tarfile.TarInfo(filename)
                    member.size = len(payload)
                    archive.addfile(member, io.BytesIO(payload))
            with (dist / "checksums.txt").open("a") as checksums:
                checksums.write(f"{sha256_file(dist / name)}  {name}\n")
            with patch("subprocess.Popen", side_effect=AssertionError("artifact execution")):
                verify_artifacts("1.2.3", dist, "server")
                for selection in ("none", "tui", "both"):
                    with self.subTest(selection=selection), self.assertRaises(VerificationError):
                        verify_artifacts("1.2.3", dist, selection)

    def test_server_source_identity_and_component_presence(self) -> None:
        inventory: JsonObject = {
            "schemaVersion": 1,
            "package": "graphite-meter-server",
            "profile": "release",
            "target": "x86_64-unknown-linux-gnu",
            "cargoLockSha256": sha256_file(Path("rust/Cargo.lock")),
            "components": [{"component": {"name": "example", "version": "1.0"}}],
        }
        for mutation in (
            "valid",
            "cargo_fixture",
            "package",
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
                dist = Path(temporary)
                metadata = dict(inventory)
                if mutation == "package":
                    metadata["package"] = "graphite-meter-client"
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
                files = {
                    "inventory.json": json.dumps(metadata).encode(),
                    "LEGAL.txt": b"notices",
                    "legal/rust-forks.json": Path("legal/rust-forks.json").read_bytes(),
                }
                if mutation != "missing":
                    files["third_party/cargo/example-1.0/source.rs"] = b"source"
                if mutation == "cargo_fixture":
                    files["third_party/cargo/example-1.0/tests/test_vector.pem"] = b"public upstream fixture"
                if mutation == "first_party_key":
                    files["rust/.dev-certs/private.key"] = b"must not ship"
                if mutation == "undeclared_tree":
                    files["third_party/cargo/other-2.0/tests/key.pem"] = b"not in inventory"
                archive_path = dist / next(iter(expected_rust_artifacts("1.2.3", "server")))
                with tarfile.open(archive_path, "w:gz") as archive:
                    for name, payload in files.items():
                        member = tarfile.TarInfo(name)
                        member.size = len(payload)
                        archive.addfile(member, io.BytesIO(payload))
                with patch("subprocess.Popen", side_effect=AssertionError("artifact execution")):
                    if mutation in {"valid", "cargo_fixture"}:
                        verify_rust_server_source(dist, "1.2.3")
                    else:
                        with self.assertRaises(VerificationError):
                            verify_rust_server_source(dist, "1.2.3")


class RustRequestBoundaryTests(unittest.TestCase):
    def test_dispatch_selection_cannot_be_forged_in_the_artifact(self) -> None:
        from fixtures import fake, git_head
        from release import OCI, Release, request_title, verify_request
        from test_trust import (MAIN, HEAD, REPO, REQUEST_RUN, ARTIFACTS,
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
                        names = [candidate.name]
                        if stable:
                            names.append("release-assets-4242")
                        elif selection != "none":
                            names.append("release-rust-assets-4242")
                        for name in names[1:]:
                            (request / name).mkdir()
                        dispatched = Release(release.tag, release.sha, release.pr,
                                             "tui" if selection == "none" else "none") if forged else release
                        api = fake(trusted(stable, "validate") | {
                            REQUEST_RUN: dispatch_run(31337, 4242, request_title("validate", dispatched, MAIN)),
                            ARTIFACTS: artifacts(*names),
                        })
                        environment = {
                            "REPOSITORY": REPO, "REPOSITORY_OWNER": "zR-JB", "PUBLISHER_SHA": MAIN,
                            "WORKFLOW_REF": f"{REPO}/.github/workflows/release.yml@refs/heads/main",
                            "REQUEST_RUN_ID": "4242",
                        } | git_head(root, MAIN)
                        with patch.dict(os.environ, environment):
                            if forged:
                                with self.assertRaisesRegex(VerificationError, "dispatch inputs"):
                                    verify_request(request, api=api)
                            else:
                                self.assertEqual(verify_request(request, api=api), (release, False))

    def test_rust_oci_requires_one_amd64_image_with_matching_provenance(self) -> None:
        from fixtures import ATTESTED, RUNNABLE, index
        from verify_oci import validate_index_descriptors
        self.assertEqual(validate_index_descriptors(index(RUNNABLE[0], ATTESTED[0]), {"amd64"}),
                         [ATTESTED[0]["digest"]])
        for manifests in ((RUNNABLE[0],), (RUNNABLE[0], ATTESTED[1]),
                          (*RUNNABLE, *ATTESTED), (RUNNABLE[1], ATTESTED[1])):
            with self.subTest(manifests=manifests), self.assertRaises(VerificationError):
                validate_index_descriptors(index(*manifests), {"amd64"})
