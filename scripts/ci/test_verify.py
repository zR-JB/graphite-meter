from __future__ import annotations

import hashlib
import io
import json
import os
import stat
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

from github_api import JsonObject
from verify_oci import (
    VerificationError as OCIError,
    parse_skopeo_version,
    validate_index_descriptors,
    verify as verify_oci,
)
from verify_release_assets import (
    VerificationError,
    archive_names,
    tui_archives,
    verify_checksums,
    verify_client_archives,
    verify_client_version,
    verify_release_file_set,
    verify_server_version,
    verify_third_party_source_archive,
)

MANIFEST = "application/vnd.oci.image.manifest.v1+json"
AMD, ARM = "sha256:" + "a" * 64, "sha256:" + "b" * 64


def write_tar(path: Path, members: dict[str, bytes]) -> None:
    with tarfile.open(path, "w:gz") as archive:
        for name, payload in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))


def descriptor(os_name: str, arch: str, digest: str, attests: str | None = None) -> JsonObject:
    item: JsonObject = {"mediaType": MANIFEST, "digest": digest,
                        "platform": {"os": os_name, "architecture": arch}}
    if attests is not None:
        item["annotations"] = {"vnd.docker.reference.type": "attestation-manifest",
                               "vnd.docker.reference.digest": attests}
    return item


def index(*manifests: JsonObject) -> JsonObject:
    return {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": list(manifests)}


RUNNABLE = (descriptor("linux", "amd64", AMD), descriptor("linux", "arm64", ARM))
ATTESTED = (descriptor("unknown", "unknown", "sha256:" + "c" * 64, AMD),
            descriptor("unknown", "unknown", "sha256:" + "d" * 64, ARM))


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
        with self.assertRaisesRegex(VerificationError, r"unexpected=\['extra.bin'\]"):
            verify_release_file_set(self.dist, {"artifact.bin"})
        for line, error in ((f"{digest}  ../artifact.bin", "unsafe"),
                            (f"{digest}  artifact\tname.bin", "unsafe"),
                            ("0" * 64 + "  artifact.bin", "checksum mismatch"),
                            ("", "empty")):
            (self.dist / "checksums.txt").write_text(line + "\n" if line else "")
            with self.subTest(line=line), self.assertRaisesRegex(VerificationError, error):
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
            with self.subTest(name=name), self.assertRaisesRegex(VerificationError, error):
                archive_names(path)

    def test_third_party_source_offer_excludes_project_source_and_manual_keys(self) -> None:
        root = "graphite-meter_1.2.3_third-party-source"
        readme = (b"Use Source code (tar.gz) or Source code (zip). This archive does not "
                  b"duplicate Graphite Meter's own repository source.\n")
        base = {
            f"{root}/README.txt": readme,
            f"{root}/LEGAL_INVENTORY.json": b'{"server":[],"tui":[],"container":[]}',
            f"{root}/PROVENANCE.json": b"[]",
            f"{root}/third_party/go/quic-go/internal/testdata/priv.key": b"upstream fixture",
            f"{root}/third_party/npm/example/cert.pem": b"upstream fixture",
            f"{root}/third_party/manual/sample/source.txt": b"manual source",
        }
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
                    with self.assertRaisesRegex(VerificationError, error):
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
        with self.assertRaisesRegex(VerificationError, "graphite-meter-client"):
            verify_client_archives(self.dist, "1.2.3", targets)

    def test_built_client_and_server_report_the_release_version(self) -> None:
        previous = Path.cwd()
        self.addCleanup(os.chdir, previous)
        os.chdir(self.dist)
        (self.dist / "client/dist").mkdir(parents=True)
        with self.assertRaisesRegex(VerificationError, "client version metadata is missing"):
            verify_client_version("1.2.3")
        (self.dist / "client/dist/version.json").write_text(
            json.dumps({"version": "1.2.3", "label": "prod", "revision": "abc1234"}))
        verify_client_version("1.2.3")
        with self.assertRaisesRegex(VerificationError, "server binary is missing"):
            verify_server_version("1.2.3")
        (self.dist / "go").mkdir()
        (self.dist / "go/graphite-meter").write_text("#!/bin/sh\necho 1.2.3\n")
        (self.dist / "go/graphite-meter").chmod(0o755)
        verify_server_version("1.2.3")
        with self.assertRaisesRegex(VerificationError, "expected '1.2.4'"):
            verify_server_version("1.2.4")


class OCITests(unittest.TestCase):
    def test_index_requires_linked_provenance_for_each_platform(self) -> None:
        self.assertEqual(validate_index_descriptors(index(*RUNNABLE, *ATTESTED)),
                         {"amd64": AMD, "arm64": ARM})
        stray = descriptor("unknown", "unknown", "sha256:" + "e" * 64, "sha256:" + "f" * 64)
        for manifests, error in (
            ((*RUNNABLE, ATTESTED[0]), "one provenance attestation"),
            ((*RUNNABLE, ATTESTED[0], stray), "one provenance attestation"),
            ((*RUNNABLE, *ATTESTED, descriptor("linux", "s390x", AMD)), "unexpected"),
            ((*RUNNABLE, *ATTESTED, RUNNABLE[0]), "duplicate"),
            ((RUNNABLE[0], ATTESTED[0]), "linux/amd64 and linux/arm64"),
        ):
            with self.subTest(error=error), self.assertRaisesRegex(OCIError, error):
                validate_index_descriptors(index(*manifests))

    def test_skopeo_version_output_shapes(self) -> None:
        for output, version in (("skopeo version 1.22.2", "1.22.2"),
                                ("skopeo version 1.22.2 commit: abcdef0123", "1.22.2"),
                                ("skopeo version 1.22.2-custom", "1.22.2-custom")):
            self.assertEqual(parse_skopeo_version(output), version)
        with self.assertRaisesRegex(OCIError, "unexpected Skopeo --version output"):
            parse_skopeo_version("skopeo 1.22.2")

    def test_verification_runs_offline_with_only_the_archive_mounted_read_only(self) -> None:
        env = {"SKOPEO_IMAGE": "quay.io/containers/skopeo:v1.22.2@sha256:" + "a" * 64,
               "SKOPEO_VERSION": "1.22.2", "REPOSITORY": "example/repo"}
        labels = {"org.opencontainers.image.source": "https://github.com/example/repo",
                  "org.opencontainers.image.revision": "f" * 40,
                  "org.opencontainers.image.version": "1.2.3",
                  "org.opencontainers.image.licenses": "AGPL-3.0-or-later"}
        calls: list[tuple[str, ...]] = []

        def run(*args: str) -> str:
            calls.append(args)
            if "--version" in args:
                return "skopeo version 1.22.2"
            if "--raw" in args:
                return json.dumps(index(*RUNNABLE, *ATTESTED))
            return json.dumps(labels) if "--format" in args else ""

        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "image.oci.tar"
            archive.symlink_to(directory)
            with self.assertRaisesRegex(OCIError, "not a regular file"):
                verify_oci("1.2.3", "f" * 40, archive)
            archive.unlink()
            archive.write_bytes(b"placeholder")
            with (patch.dict(os.environ, env), patch("verify_oci.run", side_effect=run),
                  patch("verify_oci.select_engine", return_value="docker")):
                verify_oci("1.2.3", "f" * 40, archive)
                with self.assertRaisesRegex(OCIError, "image.version"):
                    verify_oci("1.2.4", "f" * 40, archive)
        self.assertTrue(any("copy" in call and "--all" in call for call in calls))
        for call in calls:
            self.assertEqual(call[:5], ("docker", "run", "--rm", "--network", "none"))
            mounts = [call[i + 1] for i, value in enumerate(call) if value == "-v"]
            self.assertTrue(all(mount.endswith(":/work/image.oci.tar:ro") for mount in mounts))
            self.assertLessEqual(len(mounts), 1)


if __name__ == "__main__":
    unittest.main()
