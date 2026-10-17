from __future__ import annotations

import json
import os
import stat
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

from assets import archive_names
from fixtures import AMD, ARM, ATTESTED, RUNNABLE, SLSA, descriptor, engine, index, outcome, write_oci, write_tar
from github_api import ControlPlaneError
from verify_oci import BLOB_LIMIT, select_engine, validate_index_descriptors, verify as verify_oci


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
    def test_index_requires_linked_provenance_for_each_platform(self) -> None:
        self.assertEqual(validate_index_descriptors(index(*RUNNABLE, *ATTESTED)),
                         ([item["digest"] for item in ATTESTED], {"amd64": AMD, "arm64": ARM}))
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

    def test_verification_runs_offline_with_only_the_archive_mounted_read_only(self) -> None:
        wrong_revision = {"org.opencontainers.image.revision": "e" * 40}
        for change, version, error in (
            ({}, "1.2.3", None),
            ({}, "1.2.4", "image.version"),
            ({"FAKE_LABELS": wrong_revision}, "1.2.3", "image.revision"),
            ({"FAKE_DIGEST": "sha256:abc"}, "1.2.3", "digest is 'sha256:abc'"),
            ({"FAKE_INDEX": index(*RUNNABLE)}, "1.2.3", "one provenance attestation"),
        ):
            with (tempfile.TemporaryDirectory() as directory,
                  self.subTest(change=change, version=version)):
                root = Path(directory)
                archive = root / "image.oci.tar"
                oci = write_oci(archive, "example/repo", "f" * 40, remote=False)
                env = engine(root, "example/repo", "1.2.3", "f" * 40, oci)
                labels = json.loads(env["FAKE_LABELS"]) | change.pop("FAKE_LABELS", {})
                env |= {"FAKE_LABELS": json.dumps(labels)} | {
                    key: value if isinstance(value, str) else json.dumps(value)
                    for key, value in change.items()}
                with patch.dict(os.environ, env):
                    self.assertEqual(outcome(self, error, lambda: verify_oci(version, "f" * 40, archive)),
                                     None if error else AMD)
                log = (root / "engine.log").read_text()
                for call in (line.split() for line in log.splitlines()):
                    self.assertEqual(call[:5], ["run", "--rm", "--network", "none",
                                                "--entrypoint"])
                    mounts = [call[i + 1] for i, value in enumerate(call) if value == "-v"]
                    self.assertIn(mounts, ([], [f"{archive}:/work/image.oci.tar:ro"]))
                if error is None:
                    self.assertIn(" copy --all oci-archive:/work/image.oci.tar ", log)

    def test_provenance_records_the_release_commit_of_this_repository(self) -> None:
        spdx = "https://spdx.dev/Document"
        for remote, repository, commit, tamper, predicate, limit, error in (
            (True, "example/repo", "f" * 40, False, SLSA, BLOB_LIMIT, None),
            (False, "example/repo", "f" * 40, False, SLSA, BLOB_LIMIT, None),
            (True, "example/repo", "e" * 40, False, SLSA, BLOB_LIMIT, "records sources \\['e"),
            (False, "example/repo", "e" * 40, False, SLSA, BLOB_LIMIT, "records sources \\['e"),
            (True, "example/fork", "f" * 40, False, SLSA, BLOB_LIMIT, "example/fork.git"),
            (False, "example/fork", "f" * 40, False, SLSA, BLOB_LIMIT, "example/fork"),
            (False, "example/repo", "f" * 40, True, SLSA, BLOB_LIMIT, "does not match its digest"),
            (False, "example/repo", "f" * 40, False, spdx, BLOB_LIMIT, "no SLSA provenance"),
            (False, "example/repo", "f" * 40, False, SLSA, 16, "not a bounded regular file"),
        ):
            with (tempfile.TemporaryDirectory() as directory,
                  self.subTest(remote=remote, repository=repository, error=error)):
                archive = Path(directory) / "image.oci.tar"
                oci = write_oci(archive, repository, commit, remote=remote, tamper=tamper,
                                predicate=predicate)
                env = engine(Path(directory), "example/repo", "1.2.3", "f" * 40, oci)
                with patch.dict(os.environ, env), patch("verify_oci.BLOB_LIMIT", limit):
                    outcome(self, error, lambda: verify_oci("1.2.3", "f" * 40, archive))

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
            with tempfile.TemporaryDirectory() as directory, self.subTest(files=list(files), error=error):
                archive = Path(directory) / "image.oci.tar"
                oci = write_oci(archive, "example/repo", "f" * 40, remote=False, files=files)
                with patch.dict(os.environ, engine(Path(directory), "example/repo", "1.2.3", "f" * 40, oci)):
                    outcome(self, error, lambda: verify_oci("1.2.3", "f" * 40, archive))

    def test_engine_is_a_known_name_resolved_on_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            env = engine(Path(directory), "example/repo", "1.2.3", "f" * 40)
            env["PATH"] = str(Path(directory) / "bin")
            for configured, error in (("docker", None), ("", None), ("podman", "requires Docker"),
                                      (str(Path(directory) / "bin" / "docker"), "must be one of"),
                                      ("sh", "must be one of")):
                with (self.subTest(configured=configured),
                      patch.dict(os.environ, env | {"CONTAINER_ENGINE": configured})):
                    self.assertEqual(outcome(self, error, select_engine), None if error else "docker")

    def test_symlinked_or_empty_archive_is_refused_before_the_engine_runs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "image.oci.tar"
            for make in (lambda: archive.symlink_to(directory), archive.touch):
                make()
                with self.assertRaisesRegex(ControlPlaneError, "not a regular file"):
                    verify_oci("1.2.3", "f" * 40, archive)
                archive.unlink()


if __name__ == "__main__":
    unittest.main()
