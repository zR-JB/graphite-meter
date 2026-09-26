from __future__ import annotations

import hashlib
import json
import os
import stat
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

from fixtures import (
    AMD,
    ARM,
    ATTESTED,
    RUNNABLE,
    descriptor,
    engine,
    index,
    source_members,
    write_checksums,
    write_release_assets,
    write_oci,
    write_tar,
)
from verify_oci import (
    select_engine,
    validate_index_descriptors,
    verify as verify_oci,
)
from github_api import ControlPlaneError
from verify_release_assets import (
    archive_names,
    release_dist,
    tui_archives,
    verify_artifacts,
    verify_checksums,
    verify_client_archives,
    verify_release_file_set,
    verify_third_party_source_archive,
    verify as verify_release,
)

SOURCE = "graphite-meter_1.2.3_third-party-source"
LINUX = "graphite-meter-client_1.2.3_linux_amd64"


class ReleaseAssetTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.dist = Path(directory.name)

    def test_checksums_cover_exactly_safe_regular_artifacts(self) -> None:
        payload = self.dist / "artifact.bin"
        payload.write_bytes(b"graphite-meter")
        digest = hashlib.sha256(b"graphite-meter").hexdigest()
        (self.dist / "checksums.txt").write_text(f"{digest}  artifact.bin\n")
        verify_release_file_set(self.dist, verify_checksums(self.dist))
        (self.dist / "extra.bin").write_bytes(b"not checksummed")
        with self.assertRaisesRegex(ControlPlaneError, r"unexpected=\['extra.bin'\]"):
            verify_release_file_set(self.dist, {"artifact.bin"})
        (self.dist / "extra.bin").unlink()
        (self.dist / "link.bin").symlink_to(payload)
        (self.dist / "directory").mkdir()
        with self.assertRaisesRegex(ControlPlaneError, r"non-regular entries: \['directory', 'l"):
            verify_release_file_set(self.dist, {"artifact.bin", "link.bin", "directory"})
        for line, error in ((f"{digest}  ../artifact.bin", "unsafe"),
                            (f"{digest}  artifact\tname.bin", "unsafe"),
                            (f"{digest}  artifact.bin\n{digest}  artifact.bin", "duplicate"),
                            (f"{digest}  link.bin", "not a regular file: link.bin"),
                            (f"{digest}  directory", "not a regular file: directory"),
                            ("0" * 64 + "  artifact.bin", "checksum mismatch"),
                            ("", "empty")):
            (self.dist / "checksums.txt").write_text(line + "\n" if line else "")
            with self.subTest(line=line), self.assertRaisesRegex(ControlPlaneError, error):
                verify_checksums(self.dist)

    def test_archives_reject_traversal_links_and_special_files(self) -> None:
        link = tarfile.TarInfo("bundle/link")
        link.type, link.linkname = tarfile.SYMTYPE, "../../outside"
        device = zipfile.ZipInfo("bundle/device")
        device.create_system, device.external_attr = 3, (stat.S_IFCHR | 0o600) << 16
        for name, error in (("escape.tar.gz", "unsafe archive path"),
                            ("link.tar.gz", "link or special"),
                            ("escape.zip", "unsafe archive path"),
                            ("device.zip", "link or special")):
            path = self.dist / name
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

    def test_third_party_source_offer_excludes_project_source_and_manual_keys(self) -> None:
        root = SOURCE
        base = source_members("1.2.3") | {
            f"{root}/third_party/npm/example/cert.pem": b"upstream fixture"}
        for extra, error in (
            ({}, None),
            ({f"{root}/third_party/manual/sample/private.key": b"x"}, "outside upstream"),
            ({f"{root}/project/LICENSE": b"x"}, "unexpected non-third-party"),
            ({f"{root}/README.txt": b"source is elsewhere"}, "describe the source offer"),
        ):
            write_tar(self.dist / f"{root}.tar.gz", base | extra)
            with self.subTest(error=error):
                if error is None:
                    verify_third_party_source_archive(self.dist, "1.2.3")
                else:
                    with self.assertRaisesRegex(ControlPlaneError, error):
                        verify_third_party_source_archive(self.dist, "1.2.3")

    def test_tui_archives_follow_targets_and_require_the_binary(self) -> None:
        targets = self.dist / "targets.txt"
        targets.write_text("linux/amd64\nwindows/amd64\n")
        linux = "graphite-meter-client_1.2.3_linux_amd64"
        windows = "graphite-meter-client_1.2.3_windows_amd64"
        self.assertEqual(tui_archives("1.2.3", targets), {
            f"{linux}.tar.gz": (linux, "graphite-meter-client"),
            f"{windows}.zip": (windows, "graphite-meter-client.exe"),
        })
        targets.write_text("linux/amd64\n")
        legal = ("LICENSE", "COPYRIGHT", "THIRD_PARTY_NOTICES.txt", "SOURCE.txt")
        write_tar(self.dist / f"{linux}.tar.gz", {f"{linux}/{name}": b"x" for name in legal})
        with self.assertRaisesRegex(ControlPlaneError, "graphite-meter-client"):
            verify_client_archives(self.dist, "1.2.3", targets)

    def test_release_is_exactly_the_checksummed_source_offer_and_tui_archives(self) -> None:
        def add_extra(dist: Path) -> None:
            (dist / "extra.bin").write_text("x")

        def checksum_extra(dist: Path) -> None:
            add_extra(dist)
            write_checksums(dist)

        def key_in_tui(dist: Path) -> None:
            members = {f"{LINUX}/{name}": b"x" for name in (
                "graphite-meter-client", "LICENSE", "COPYRIGHT", "THIRD_PARTY_NOTICES.txt",
                "SOURCE.txt", "certs/server.key")}
            write_tar(dist / f"{LINUX}.tar.gz", members)
            write_checksums(dist)

        def no_binary(dist: Path) -> None:
            write_tar(dist / f"{LINUX}.tar.gz", {f"{LINUX}/LICENSE": b"x"})
            write_checksums(dist)

        def bad_offer(dist: Path) -> None:
            write_tar(dist / f"{SOURCE}.tar.gz", source_members("1.2.3") | {
                f"{SOURCE}/README.txt": b"source is elsewhere"})
            write_checksums(dist)

        for edit, error in ((None, None), (add_extra, r"release files: .*unexpected=\['extra"),
                            (checksum_extra, r"checksummed release artifacts: .*'extra.bin'"),
                            (key_in_tui, "certificate/key material"),
                            (no_binary, "missing: .*graphite-meter-client'"),
                            (bad_offer, "describe the source offer")):
            dist = self.dist / (edit.__name__ if edit else "valid")
            write_release_assets(dist, "1.2.3")
            if edit is not None:
                edit(dist)
            with self.subTest(error=error):
                if error is None:
                    verify_artifacts("1.2.3", dist)
                else:
                    with self.assertRaisesRegex(ControlPlaneError, error):
                        verify_artifacts("1.2.3", dist)

    def test_the_host_tui_archive_reports_the_release_version(self) -> None:
        write_release_assets(self.dist / "valid", "1.2.3")
        verify_release("1.2.3", self.dist / "valid")
        write_release_assets(self.dist / "stale", "1.2.3", reported="1.2.2")
        with self.assertRaisesRegex(ControlPlaneError, "reports 'graphite-meter-client 1.2.2'"):
            verify_release("1.2.3", self.dist / "stale")

    def test_release_dist_stays_in_the_checkout_or_a_temporary_directory(self) -> None:
        with patch.dict(os.environ, {"RELEASE_DIST": str(self.dist / "dist")}):
            self.assertEqual(release_dist(), self.dist.resolve() / "dist")
        with patch.dict(os.environ):
            os.environ.pop("RELEASE_DIST", None)
            self.assertEqual(release_dist(), Path("go/dist").resolve())
        for outside in ("/", "/etc/../usr/bin"):
            with (self.subTest(outside=outside), patch.dict(os.environ, {"RELEASE_DIST": outside}),
                  self.assertRaisesRegex(ControlPlaneError, "is outside")):
                release_dist()


class OCITests(unittest.TestCase):
    def test_index_requires_linked_provenance_for_each_platform(self) -> None:
        self.assertEqual(validate_index_descriptors(index(*RUNNABLE, *ATTESTED)),
                         [item["digest"] for item in ATTESTED])
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
                    if error is None:
                        self.assertEqual(verify_oci(version, "f" * 40, archive), AMD)
                    else:
                        with self.assertRaisesRegex(ControlPlaneError, error):
                            verify_oci(version, "f" * 40, archive)
                log = (root / "engine.log").read_text()
                for call in (line.split() for line in log.splitlines()):
                    self.assertEqual(call[:5], ["run", "--rm", "--network", "none",
                                                "--entrypoint"])
                    mounts = [call[i + 1] for i, value in enumerate(call) if value == "-v"]
                    self.assertIn(mounts, ([], [f"{archive}:/work/image.oci.tar:ro"]))
                if error is None:
                    self.assertIn(" copy --all oci-archive:/work/image.oci.tar ", log)

    def test_provenance_records_the_release_commit_of_this_repository(self) -> None:
        for remote, repository, commit, tamper, error in (
            (True, "example/repo", "f" * 40, False, None),
            (False, "example/repo", "f" * 40, False, None),
            (True, "example/repo", "e" * 40, False, "records sources \\['e"),
            (False, "example/repo", "e" * 40, False, "records sources \\['e"),
            (True, "example/fork", "f" * 40, False, "example/fork.git"),
            (False, "example/fork", "f" * 40, False, "example/fork"),
            (False, "example/repo", "f" * 40, True, "does not match its digest"),
        ):
            with (tempfile.TemporaryDirectory() as directory,
                  self.subTest(remote=remote, repository=repository, error=error)):
                archive = Path(directory) / "image.oci.tar"
                oci = write_oci(archive, repository, commit, remote=remote, tamper=tamper)
                env = engine(Path(directory), "example/repo", "1.2.3", "f" * 40, oci)
                with patch.dict(os.environ, env):
                    if error is None:
                        verify_oci("1.2.3", "f" * 40, archive)
                    else:
                        with self.assertRaisesRegex(ControlPlaneError, error):
                            verify_oci("1.2.3", "f" * 40, archive)

    def test_engine_is_a_known_name_resolved_on_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            env = engine(Path(directory), "example/repo", "1.2.3", "f" * 40)
            env["PATH"] = str(Path(directory) / "bin")
            for configured, error in (("docker", None), ("", None), ("podman", "requires Docker"),
                                      (str(Path(directory) / "bin" / "docker"), "must be one of"),
                                      ("sh", "must be one of")):
                with (self.subTest(configured=configured),
                      patch.dict(os.environ, env | {"CONTAINER_ENGINE": configured})):
                    if error is None:
                        self.assertEqual(select_engine(), "docker")
                    else:
                        with self.assertRaisesRegex(ControlPlaneError, error):
                            select_engine()

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
