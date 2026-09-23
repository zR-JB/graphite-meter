"""Rust release artifacts remain untrusted data throughout shared verification."""
from __future__ import annotations

import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from verify_release_assets import VerificationError, read_tar_text, sha256_file, verify_rust_client_archive


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
