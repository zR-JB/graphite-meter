"""Rust release artifacts remain untrusted data throughout shared verification."""
from __future__ import annotations

import io
import json
import tarfile
import tempfile
import subprocess
import textwrap
import unittest
from pathlib import Path
from unittest.mock import patch

from verify_release_assets import VerificationError, read_tar_text, sha256_file, verify_rust_client_archive, verify_rust_server_source, expected_rust_artifacts


class RustArchiveBoundaryTests(unittest.TestCase):
    def test_executable_member_is_never_executed(self) -> None:
        version = "1.2.3"
        base = f"graphite-meter-client_{version}_linux_amd64_rust"
        metadata = {
            "schemaVersion": 1, "implementation": "rust", "version": "1.2.3-rust",
            "target": "x86_64-unknown-linux-gnu", "minimumGlibc": "2.36",
            "neededLibraries": ["libc.so.6"], "rustc": "rustc test-fixture\n",
        }
        inventory = {
            "schemaVersion": 1, "package": "graphite-meter-client", "profile": "release",
            "target": metadata["target"], "rustc": metadata["rustc"],
            "cargoLockSha256": sha256_file(Path("rust/Cargo.lock")),
            "components": [{"component": {"name": "example", "version": "1.0"}}],
        }
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            marker = dist / "executed"
            files = {
                f"{base}/graphite-meter-client": f"#!/bin/sh\ntouch '{marker}'\n".encode(),
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
                    "LEGAL.txt": b"fixture notices\n", "rust/vendor/PATCHES.md": b"fixture",
                    "third_party/cargo/example-1.0/source.rs": b"fixture",
                }.items():
                    member = tarfile.TarInfo(name)
                    member.size = len(payload)
                    archive.addfile(member, io.BytesIO(payload))
            with patch("subprocess.Popen", side_effect=AssertionError("artifact execution")):
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
    def test_artifact_selection_is_additive(self) -> None:
        self.assertEqual(expected_rust_artifacts("1.2.3", "none"), set())
        server = expected_rust_artifacts("1.2.3", "server")
        self.assertEqual(server, {"graphite-meter-server_1.2.3_linux_amd64_rust_third-party-source.tar.gz"})
        self.assertEqual(expected_rust_artifacts("1.2.3", "both"), server | expected_rust_artifacts("1.2.3", "tui"))

    def test_server_source_identity_and_component_presence(self) -> None:
        inventory = {
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
        ):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                dist = Path(temporary)
                metadata = dict(inventory)
                if mutation == "package":
                    metadata["package"] = "graphite-meter-client"
                if mutation == "lock":
                    metadata["cargoLockSha256"] = "0" * 64
                files = {
                    "inventory.json": json.dumps(metadata).encode(),
                    "LEGAL.txt": b"notices",
                    "rust/vendor/PATCHES.md": b"patch provenance",
                }
                if mutation != "missing":
                    files["third_party/cargo/example-1.0/source.rs"] = b"source"
                if mutation == "cargo_fixture":
                    files["third_party/cargo/example-1.0/tests/test_vector.pem"] = b"public upstream fixture"
                if mutation == "first_party_key":
                    files["rust/vendor/.dev-certs/private.key"] = b"must not ship"
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

    def test_publisher_tags_cannot_cross_implementation_boundary(self) -> None:
        workflow = Path(".github/workflows/_publish-oci.yml").read_text()
        start = workflow.index('          case "$IMPLEMENTATION" in')
        end = workflow.index("          # Minimize")
        script = textwrap.dedent(workflow[start:end])
        for implementation, tag, pr, accepted in (
            ("go", "1.2.3", "", True),
            ("go", "1.2.3-rc.1", "42", True),
            ("go", "1.2.3-rust", "", False),
            ("rust", "1.2.3-rust", "", True),
            ("rust", "1.2.3-rc.1-rust", "", False),
            ("rust", "1.2.3", "", False),
            ("rust", "latest-rust", "", False),
            ("rust", "1.2.3-rust", "42", False),
            ("unknown", "1.2.3", "", False),
        ):
            with self.subTest(implementation=implementation, tag=tag, pr=pr):
                result = subprocess.run(
                    ["bash", "-c", script],
                    env={
                        "IMPLEMENTATION": implementation,
                        "IMAGE_TAG": tag,
                        "PR_NUMBER": pr,
                    },
                    capture_output=True,
                )
                self.assertEqual(result.returncode == 0, accepted, result.stderr)
