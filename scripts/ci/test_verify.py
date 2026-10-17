from __future__ import annotations

import json
import os
import stat
import tarfile
import tempfile
import unittest
import zipfile
from collections.abc import Callable
from pathlib import Path
from unittest.mock import patch

from assets import archive_names
from fixtures import AMD, ARM, ATTESTED, RUNNABLE, SLSA, descriptor, index, outcome, write_oci, write_tar
from github_api import ControlPlaneError
from verify_oci import BLOB_LIMIT, validate_index_descriptors, verify as verify_oci


def rewrite(path: Path, change: Callable[[dict[str, bytes]], object]) -> None:
    """Rewrite the members of an uncompressed tar archive."""
    with tarfile.open(path) as archive:
        members = {entry.name: handle.read() for entry in archive if (handle := archive.extractfile(entry))}
    change(members)
    write_tar(path, members, compressed=False)


class ArchiveTests(unittest.TestCase):
    def test_archives_reject_traversal_links_and_special_files(self) -> None:
        link = tarfile.TarInfo("bundle/link")
        link.type, link.linkname = tarfile.SYMTYPE, "../../outside"
        device = zipfile.ZipInfo("bundle/device")
        device.create_system, device.external_attr = 3, (stat.S_IFCHR | 0o600) << 16
        with tempfile.TemporaryDirectory() as directory:
            for name, error in (("escape.tar.gz", "unsafe archive path"), ("link.tar.gz", "link or special"),
                                ("escape.zip", "unsafe archive path"), ("device.zip", "link or special")):
                path = Path(directory) / name
                if name == "escape.tar.gz":
                    write_tar(path, {"../escape": b"x"})
                elif name == "link.tar.gz":
                    with tarfile.open(path, "w:gz") as tar:
                        tar.addfile(link)
                else:
                    with zipfile.ZipFile(path, "w") as archive:
                        archive.writestr("..\\escape" if name == "escape.zip" else device, b"")
                with self.subTest(name=name), self.assertRaisesRegex(ControlPlaneError, error):
                    archive_names(path)


class OCITests(unittest.TestCase):
    def setUp(self) -> None:
        self.archive = Path(self.enterContext(tempfile.TemporaryDirectory())) / "image.oci.tar"
        self.enterContext(patch.dict(os.environ, {"REPOSITORY": "example/repo"}))
        self.enterContext(patch("sys.stdout"))

    def verify(self, error: str | None, version: str = "1.2.3") -> str | None:
        return outcome(self, error, lambda: verify_oci(version, "f" * 40, self.archive))

    def test_index_requires_linked_provenance_for_each_platform(self) -> None:
        attestations, images = validate_index_descriptors(index(*RUNNABLE, *ATTESTED))
        self.assertEqual([item["digest"] for item in attestations], [item["digest"] for item in ATTESTED])
        self.assertEqual({arch: item["digest"] for arch, item in images.items()}, {"amd64": AMD, "arm64": ARM})
        stray = descriptor("unknown", "unknown", "sha256:" + "e" * 64, "sha256:" + "f" * 64)
        mistyped = descriptor("unknown", "unknown", "sha256:" + "d" * 64, ARM)
        mistyped["annotations"] = {"vnd.docker.reference.type": "other",
                                   "vnd.docker.reference.digest": ARM}
        docker = descriptor("linux", "amd64", AMD)
        docker["mediaType"] = "application/vnd.docker.distribution.manifest.v2+json"
        for manifests, error in (
            ((*RUNNABLE, ATTESTED[0]), "one provenance attestation"),
            ((*RUNNABLE, ATTESTED[0], stray), "one provenance attestation"),
            ((*RUNNABLE, *ATTESTED, stray), "one provenance attestation"),
            ((*RUNNABLE, *ATTESTED, ATTESTED[0]), "one provenance attestation"),
            ((*RUNNABLE, ATTESTED[0], mistyped), "not a provenance attestation"),
            ((descriptor("linux", "amd64", "sha256:abc"), RUNNABLE[1], *ATTESTED), "sha256 digest"),
            ((docker, RUNNABLE[1], *ATTESTED), "must be an OCI manifest"),
            ((*RUNNABLE, *ATTESTED, descriptor("linux", "s390x", AMD)), "unexpected"),
            ((*RUNNABLE, *ATTESTED, RUNNABLE[0]), "duplicate"),
            ((RUNNABLE[0], ATTESTED[0]), "linux/amd64 and linux/arm64"),
        ):
            with self.subTest(error=error), self.assertRaisesRegex(ControlPlaneError, error):
                validate_index_descriptors(index(*manifests))
        for key, value in (("schemaVersion", 1), ("mediaType", "application/json")):
            valid = index(*RUNNABLE, *ATTESTED)
            valid[key] = value
            with self.subTest(key=key), self.assertRaisesRegex(ControlPlaneError, "schemaVersion 2"):
                validate_index_descriptors(valid)

    def test_every_blob_the_index_reaches_is_present_and_matches_its_descriptor(self) -> None:
        digest = write_oci(self.archive, "example/repo", "f" * 40, remote=False)
        self.assertEqual(self.verify(None), digest)
        original = self.archive.read_bytes()
        with tarfile.open(self.archive) as archive:
            blobs = [name for name in archive.getnames() if name.startswith("blobs/")]

        def flip(data: bytes) -> bytes:
            return data[:-1] + (b"x" if data[-1:] != b"x" else b"y")

        changes: list[tuple[Callable[[dict[str, bytes]], object], str]] = [
            (lambda members: members.pop("index.json"), "index.json"),
            (lambda members: members.update({"index.json": json.dumps(index(*RUNNABLE)).encode()}),
             "exactly one image index"),
        ]
        for name in blobs:
            changes += [(lambda members, name=name: members.pop(name), "lacks blob"),
                        (lambda members, name=name: members.update({name: flip(members[name])}), "match its digest"),
                        (lambda members, name=name: members.update({name: members[name][:-1]}), "descriptor's size")]
        for change, error in changes:
            with self.subTest(error=error):
                self.archive.write_bytes(original)
                rewrite(self.archive, change)
                self.verify(error)

    def test_each_image_carries_the_release_labels(self) -> None:
        for labels, version, error in (
            ({}, "1.2.4", "image.version"),
            ({"org.opencontainers.image.revision": "e" * 40}, "1.2.3", "image.revision"),
            ({"org.opencontainers.image.source": "https://github.com/example/fork"}, "1.2.3", "image.source"),
            ({"org.opencontainers.image.licenses": "MIT"}, "1.2.3", "image.licenses"),
        ):
            with self.subTest(error=error):
                write_oci(self.archive, "example/repo", "f" * 40, remote=False, labels=labels)
                self.verify(error, version)

    def test_provenance_records_the_release_commit_of_this_repository(self) -> None:
        spdx = "https://spdx.dev/Document"
        labels = {"org.opencontainers.image.source": "https://github.com/example/repo",
                  "org.opencontainers.image.revision": "f" * 40}
        for remote, repository, commit, predicate, limit, error in (
            (True, "example/repo", "f" * 40, SLSA, BLOB_LIMIT, None),
            (False, "example/repo", "f" * 40, SLSA, BLOB_LIMIT, None),
            (True, "example/repo", "e" * 40, SLSA, BLOB_LIMIT, "records sources \\['e"),
            (False, "example/repo", "e" * 40, SLSA, BLOB_LIMIT, "records sources \\['e"),
            (True, "example/fork", "f" * 40, SLSA, BLOB_LIMIT, "example/fork.git"),
            (False, "example/fork", "f" * 40, SLSA, BLOB_LIMIT, "example/fork"),
            (False, "example/repo", "f" * 40, spdx, BLOB_LIMIT, "no SLSA provenance"),
            (False, "example/repo", "f" * 40, SLSA, 16, "not a bounded regular file"),
        ):
            with self.subTest(remote=remote, repository=repository, error=error):
                write_oci(self.archive, repository, commit, remote=remote, predicate=predicate, labels=labels)
                with patch("verify_oci.BLOB_LIMIT", limit):
                    self.verify(error)

    def test_each_image_ships_the_server_and_notices_of_a_reviewed_build(self) -> None:
        server, notices = "graphite-meter", "usr/share/licenses/graphite-meter/THIRD_PARTY_NOTICES.txt"
        marked = b"\0UNREVIEWED DEVELOPMENT BUILD\0"
        for files, error in (
            ({server: b"server", notices: b"notices"}, None),
            ({server: b"server"}, r"lacks \['usr/share"),
            ({notices: b"notices", "bin/graphite-meter": b"server"}, r"lacks \['graphite-meter'\]"),
            ({server: marked, notices: b"notices"}, "development build's graphite-meter"),
            ({server: b"server", notices: b"UNREVIEWED DEVELOPMENT BUILD\n\nnotices"}, "development build's usr"),
        ):
            with self.subTest(files=list(files), error=error):
                write_oci(self.archive, "example/repo", "f" * 40, remote=False, files=files)
                self.verify(error)

    def test_symlinked_or_empty_archive_is_refused(self) -> None:
        for make in (lambda: self.archive.symlink_to(self.archive.parent), self.archive.touch):
            make()
            self.verify("not a regular file")
            self.archive.unlink()


if __name__ == "__main__":
    unittest.main()
