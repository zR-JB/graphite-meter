#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from unittest.mock import patch

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))

from github_api import APICall, JsonObject, JsonValue
from precommit import (
    CheckPlan,
    StagedChange,
    is_tls_path,
    parse_staged_changes,
    plan_checks,
)
from prerelease import (
    PRERELEASE_RE,
    exact_file_set,
    validate_request_run,
)
from verify_oci import (
    VerificationError as OCIVerificationError,
    parse_skopeo_version,
    validate_index_descriptors,
    verify_skopeo_runtime,
    verify_archive_blobs,
    verify as verify_oci_archive,
)
from verify_release_assets import (
    VerificationError as ReleaseVerificationError,
    archive_names,
    expected_release_artifacts,
    verify_checksums,
    verify_client_archives,
    verify_client_version,
    verify_release_file_set,
    verify_server_version,
    verify_third_party_source_archive,
)
from release import (
    SEMVER_RE,
    STABLE_SEMVER_RE,
    require_compatible_release_tag,
    validate_request_context,
)
from trust import (
    TrustError,
    require_ci_gate,
    require_current_main,
    require_exact_current_main,
    require_control_plane_matches_main,
    require_check_run,
    require_main_codeql,
)

MAIN = "1" * 40
HEAD = "2" * 40
OLD_MAIN = "3" * 40


class PipelineTests(unittest.TestCase):
    def test_pr_behind_current_main_is_rejected(self) -> None:
        def fake(path: str, **_: object) -> JsonValue:
            if path.endswith("/commits/main"):
                return {"sha": MAIN}
            if "/compare/" in path:
                return {"behind_by": 3, "merge_base_commit": {"sha": OLD_MAIN}}
            raise AssertionError(path)

        with self.assertRaisesRegex(TrustError, "behind current main"):
            require_current_main("zR-JB/graphite-meter", 101, HEAD, api=fake)

    def test_pr_containing_current_main_is_accepted(self) -> None:
        def fake(path: str, **_: object) -> JsonValue:
            if path.endswith("/commits/main"):
                return {"sha": MAIN}
            if "/compare/" in path:
                return {"behind_by": 0, "merge_base_commit": {"sha": MAIN}}
            raise AssertionError(path)

        self.assertEqual(require_current_main("zR-JB/graphite-meter", 101, HEAD, api=fake), MAIN)

    def test_stable_release_requires_exact_current_main_tip(self) -> None:
        def current_main(path: str, **kwargs: object) -> JsonValue:
            return {"sha": MAIN}

        self.assertEqual(require_exact_current_main("zR-JB/graphite-meter", MAIN, api=current_main), MAIN)
        with self.assertRaisesRegex(TrustError, "no longer current main"):
            require_exact_current_main("zR-JB/graphite-meter", OLD_MAIN, api=current_main)

    def test_prerelease_control_plane_must_match_main(self) -> None:
        def trees(scripts_at_head: str) -> APICall:
            def api(path: str, **_: object) -> JsonValue:
                scripts = scripts_at_head if path.endswith(HEAD) else "b" * 40
                return {"tree": [{"path": ".github", "sha": "a" * 40},
                                 {"path": "scripts", "sha": scripts}, {"path": "go", "sha": HEAD}]}
            return api

        repo = "zR-JB/graphite-meter"
        require_control_plane_matches_main(repo, HEAD, MAIN, api=trees("b" * 40))
        with self.assertRaisesRegex(TrustError, "PR changes scripts"):
            require_control_plane_matches_main(repo, HEAD, MAIN, api=trees("c" * 40))

    def test_main_codeql_uses_newest_result_per_analysis_identity(self) -> None:
        def fake(path: str, *, paginate: bool = False, **_: object) -> JsonValue:
            self.assertTrue(paginate)
            return [[
                {
                    "id": 10, "commit_sha": MAIN, "created_at": "2026-08-15T10:00:00Z",
                    "category": "go", "analysis_key": "default", "environment": "go",
                    "error": "transient upload error", "warning": "", "tool": {"name": "CodeQL"},
                },
                {
                    "id": 11, "commit_sha": MAIN, "created_at": "2026-08-15T10:05:00Z",
                    "category": "go", "analysis_key": "default", "environment": "go",
                    "error": "", "warning": "", "tool": {"name": "CodeQL"},
                },
                {
                    "id": 12, "commit_sha": MAIN, "created_at": "2026-08-15T10:04:00Z",
                    "category": "javascript", "analysis_key": "default", "environment": "js",
                    "error": "", "warning": "", "tool": {"name": "CodeQL"},
                },
            ]]

        require_main_codeql("zR-JB/graphite-meter", MAIN, api=fake)

    def test_main_codeql_rejects_newer_failure_even_if_older_succeeded(self) -> None:
        def fake(path: str, *, paginate: bool = False, **_: object) -> JsonValue:
            self.assertTrue(paginate)
            return [[
                {
                    "id": 20, "commit_sha": MAIN, "created_at": "2026-08-15T10:00:00Z",
                    "category": "go", "analysis_key": "default", "environment": "go",
                    "error": "", "warning": "", "tool": {"name": "CodeQL"},
                },
                {
                    "id": 21, "commit_sha": MAIN, "created_at": "2026-08-15T10:06:00Z",
                    "category": "go", "analysis_key": "default", "environment": "go",
                    "error": "database finalize failed", "warning": "", "tool": {"name": "CodeQL"},
                },
            ]]

        with self.assertRaisesRegex(TrustError, "latest CodeQL analysis"):
            require_main_codeql("zR-JB/graphite-meter", MAIN, api=fake)

    def test_ci_gate_is_bound_to_exact_successful_run_and_gate_job(self) -> None:
        def fake(path: str, *, paginate: bool = False, **_: object) -> JsonValue:
            self.assertTrue(paginate)
            if "/actions/workflows/ci.yml/runs?" in path:
                return [{"workflow_runs": [
                    {"id": 10, "run_number": 10, "run_attempt": 1, "head_sha": HEAD, "head_branch": "fix/test", "event": "pull_request", "status": "completed", "conclusion": "failure", "pull_requests": [{"number": 101}]},
                    {"id": 11, "run_number": 11, "run_attempt": 2, "head_sha": HEAD, "head_branch": "fix/test", "event": "pull_request", "status": "completed", "conclusion": "success", "pull_requests": [{"number": 101}]},
                ]}]
            if "/actions/runs/11/jobs?" in path:
                return [{"jobs": [{"name": "Gate", "status": "completed", "conclusion": "success"}]}]
            raise AssertionError(path)

        self.assertEqual(require_ci_gate("zR-JB/graphite-meter", HEAD, event="pull_request", branch="fix/test", pr_number=101, api=fake), 11)

    def test_ci_gate_rejects_newer_failed_run_even_if_older_run_succeeded(self) -> None:
        def fake(path: str, *, paginate: bool = False, **_: object) -> JsonValue:
            self.assertTrue(paginate)
            if "/actions/workflows/ci.yml/runs?" in path:
                return [{"workflow_runs": [
                    {"id": 10, "run_number": 10, "run_attempt": 1, "head_sha": HEAD, "head_branch": "fix/test", "event": "pull_request", "status": "completed", "conclusion": "success", "pull_requests": [{"number": 101}]},
                    {"id": 11, "run_number": 11, "run_attempt": 1, "head_sha": HEAD, "head_branch": "fix/test", "event": "pull_request", "status": "completed", "conclusion": "failure", "pull_requests": [{"number": 101}]},
                ]}]
            raise AssertionError(f"newer failed CI must stop before job lookup: {path}")

        with self.assertRaisesRegex(TrustError, "latest CI run 11.*failure"):
            require_ci_gate(
                "zR-JB/graphite-meter",
                HEAD,
                event="pull_request",
                branch="fix/test",
                pr_number=101,
                api=fake,
            )

    def test_ci_gate_rejects_successful_run_with_failed_gate(self) -> None:
        def fake(path: str, *, paginate: bool = False, **_: object) -> JsonValue:
            if "/actions/workflows/ci.yml/runs?" in path:
                return [{"workflow_runs": [{"id": 11, "run_number": 11, "run_attempt": 1, "head_sha": HEAD, "head_branch": "main", "event": "push", "status": "completed", "conclusion": "success", "pull_requests": []}]}]
            if "/actions/runs/11/jobs?" in path:
                return [{"jobs": [{"name": "Gate", "status": "completed", "conclusion": "failure"}]}]
            raise AssertionError(path)

        with self.assertRaisesRegex(TrustError, "Gate in CI run"):
            require_ci_gate("zR-JB/graphite-meter", HEAD, event="push", branch="main", api=fake)

    def test_codeql_check_is_bound_to_exact_pr_and_latest_result(self) -> None:
        def fake(path: str, *, paginate: bool = False, **_: object) -> JsonValue:
            self.assertTrue(paginate)
            self.assertIn(f"/commits/{HEAD}/check-runs?", path)
            return [{"check_runs": [
                {
                    "id": 20,
                    "name": "CodeQL",
                    "app": {"slug": "github-advanced-security"},
                    "status": "completed",
                    "conclusion": "success",
                    "completed_at": "2026-08-15T10:00:00Z",
                    "pull_requests": [{"number": 999}],
                },
                {
                    "id": 21,
                    "name": "CodeQL",
                    "app": {"slug": "github-advanced-security"},
                    "status": "completed",
                    "conclusion": "success",
                    "completed_at": "2026-08-15T10:01:00Z",
                    "pull_requests": [{"number": 101}],
                },
            ]}]

        self.assertEqual(
            require_check_run(
                "zR-JB/graphite-meter",
                HEAD,
                name="CodeQL",
                app_slug="github-advanced-security",
                pr_number=101,
                api=fake,
            ),
            21,
        )

    def test_codeql_check_rejects_unbound_or_newer_failed_pr_result(self) -> None:
        def unbound(path: str, *, paginate: bool = False, **_: object) -> JsonValue:
            return [{"check_runs": [{
                "id": 30,
                "name": "CodeQL",
                "app": {"slug": "github-advanced-security"},
                "status": "completed",
                "conclusion": "success",
                "completed_at": "2026-08-15T10:00:00Z",
                "pull_requests": [],
            }]}]

        with self.assertRaisesRegex(TrustError, "CodeQL for PR #101.*missing"):
            require_check_run(
                "zR-JB/graphite-meter", HEAD, name="CodeQL",
                app_slug="github-advanced-security", pr_number=101, api=unbound
            )

        def failed(path: str, *, paginate: bool = False, **_: object) -> JsonValue:
            return [{"check_runs": [
                {
                    "id": 31, "name": "CodeQL",
                    "app": {"slug": "github-advanced-security"},
                    "status": "completed", "conclusion": "success",
                    "completed_at": "2026-08-15T10:00:00Z",
                    "pull_requests": [{"number": 101}],
                },
                {
                    "id": 32, "name": "CodeQL",
                    "app": {"slug": "github-advanced-security"},
                    "status": "completed", "conclusion": "failure",
                    "completed_at": "2026-08-15T10:01:00Z",
                    "pull_requests": [{"number": 101}],
                },
            ]}]

        with self.assertRaisesRegex(TrustError, "CodeQL for PR #101.*failure"):
            require_check_run(
                "zR-JB/graphite-meter", HEAD, name="CodeQL",
                app_slug="github-advanced-security", pr_number=101, api=failed
            )

    def test_stable_release_preflights_existing_tag_target(self) -> None:
        def absent(path: str, **_: object) -> JsonValue:
            self.assertIn("matching-refs/tags/v1.2.3", path)
            return []

        require_compatible_release_tag("zR-JB/graphite-meter", "v1.2.3", MAIN, api=absent)

        def wrong(path: str, **_: object) -> JsonValue:
            self.assertIn("matching-refs/tags/v1.2.3", path)
            return [{"ref": "refs/tags/v1.2.3", "object": {"type": "commit", "sha": HEAD}}]

        with self.assertRaisesRegex(SystemExit, "already exists"):
            require_compatible_release_tag("zR-JB/graphite-meter", "v1.2.3", MAIN, api=wrong)

        tag_object = "4" * 40
        def annotated(path: str, **_: object) -> JsonValue:
            if "matching-refs" in path:
                return [{"ref": "refs/tags/v1.2.3", "object": {"type": "tag", "sha": tag_object}}]
            if path.endswith(f"/git/tags/{tag_object}"):
                return {"object": {"type": "commit", "sha": MAIN}}
            raise AssertionError(path)

        require_compatible_release_tag("zR-JB/graphite-meter", "v1.2.3", MAIN, api=annotated)

    def test_release_semver_contracts(self) -> None:
        for value in ("v0.5.2", "v0.5.2-alpha.0", "v10.12.30-rc.7"):
            self.assertIsNotNone(SEMVER_RE.fullmatch(value), value)
        self.assertIsNotNone(STABLE_SEMVER_RE.fullmatch("v0.5.2"))
        for value in ("v0.5.2-alpha.0", "0.5.2", "v0.5", "v0.5.2-preview.1", "v01.5.2", "v0.05.2", "v0.5.02"):
            self.assertIsNone(STABLE_SEMVER_RE.fullmatch(value), value)
        for value in ("v01.5.2-alpha.0", "v0.5.2-alpha.00", "v0.05.2-rc.1"):
            self.assertIsNone(SEMVER_RE.fullmatch(value), value)
            self.assertIsNone(PRERELEASE_RE.fullmatch(value), value)

    def test_candidate_artifact_file_set_rejects_extras_and_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            (root / "candidate.json").write_text("{}")
            exact_file_set(root, {"candidate.json"}, "candidate")
            (root / "extra").write_text("x")
            with self.assertRaisesRegex(SystemExit, "candidate files"):
                exact_file_set(root, {"candidate.json"}, "candidate")
            (root / "extra").unlink()
            (root / "candidate.json").unlink()
            with tempfile.NamedTemporaryFile() as target:
                (root / "candidate.json").symlink_to(target.name)
                with self.assertRaisesRegex(SystemExit, "not a regular file"):
                    exact_file_set(root, {"candidate.json"}, "candidate")

    def test_precommit_plan_uses_exact_component_gates(self) -> None:
        for path in ("api/routes.txt", "api/preflight.schema.json", "api/wire.testvectors.txt", "mise.toml", "mise.lock"):
            with self.subTest(path=path):
                self.assertEqual(plan_checks((path,)), CheckPlan(pipeline=False, recipes=("check",)))
        self.assertEqual(
            plan_checks(("go/internal/server/listeners.go",)),
            CheckPlan(
                pipeline=False,
                recipes=("check-generated", "server-check", "server-test", "legal-check"),
            ),
        )
        self.assertEqual(
            plan_checks((".github/workflows/ci.yml",)),
            CheckPlan(pipeline=True, recipes=()),
        )
        self.assertEqual(
            plan_checks(("mise.toml", "go/internal/server/listeners.go")),
            CheckPlan(pipeline=False, recipes=("check",)),
        )
        self.assertTrue(is_tls_path("tmp/cert.pem"))
        self.assertFalse(is_tls_path("go/internal/server/tls.go"))

    def test_precommit_staged_change_parser_keeps_deletions_and_rename_sources(self) -> None:
        raw = (
            b"M\0go/internal/server/listeners.go\0"
            b"D\0.github/workflows/old.yml\0"
            b"R100\0.github/workflows/ci.yml\0docs/ci-example.yml\0"
        )
        self.assertEqual(
            parse_staged_changes(raw),
            (
                StagedChange("go/internal/server/listeners.go", deleted=False),
                StagedChange(".github/workflows/old.yml", deleted=True),
                StagedChange(".github/workflows/ci.yml", deleted=True),
                StagedChange("docs/ci-example.yml", deleted=False),
            ),
        )
        plan = plan_checks(tuple(change.path for change in parse_staged_changes(raw)))
        self.assertTrue(plan.pipeline, "deleted/renamed workflow paths must still select pipeline checks")

    def test_checksum_verifier_accepts_file_and_rejects_path_escape(self) -> None:
        import hashlib

        with tempfile.TemporaryDirectory() as td:
            dist = pathlib.Path(td)
            payload = dist / "artifact.bin"
            payload.write_bytes(b"graphite-meter")
            digest = hashlib.sha256(payload.read_bytes()).hexdigest()
            (dist / "checksums.txt").write_text(f"{digest}  artifact.bin\n")
            checksummed = verify_checksums(dist)
            self.assertEqual(checksummed, {"artifact.bin"})
            verify_release_file_set(dist, checksummed)

            (dist / "extra.bin").write_bytes(b"not checksummed")
            with self.assertRaisesRegex(ReleaseVerificationError, "unchecksummed=extra.bin"):
                verify_release_file_set(dist, checksummed)
            (dist / "extra.bin").unlink()

            (dist / "checksums.txt").write_text(f"{digest}  ../artifact.bin\n")
            with self.assertRaisesRegex(ReleaseVerificationError, "unsafe checksums.txt path"):
                verify_checksums(dist)

            (dist / "checksums.txt").write_text(f"{digest}  artifact\tname.bin\n")
            with self.assertRaisesRegex(ReleaseVerificationError, "unsafe release artifact name"):
                verify_checksums(dist)

    def test_release_archive_verifier_rejects_traversal_and_links(self) -> None:
        import io
        import tarfile
        import zipfile

        with tempfile.TemporaryDirectory() as td:
            directory = pathlib.Path(td)
            bad_tar = directory / "bad.tar.gz"
            with tarfile.open(bad_tar, "w:gz") as archive:
                info = tarfile.TarInfo("../escape")
                payload = b"x"
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))
            with self.assertRaisesRegex(ReleaseVerificationError, "unsafe archive path"):
                archive_names(bad_tar)

            link_tar = directory / "link.tar.gz"
            with tarfile.open(link_tar, "w:gz") as archive:
                info = tarfile.TarInfo("bundle/link")
                info.type = tarfile.SYMTYPE
                info.linkname = "../../outside"
                archive.addfile(info)
            with self.assertRaisesRegex(ReleaseVerificationError, "link/device"):
                archive_names(link_tar)

            bad_zip = directory / "bad.zip"
            with zipfile.ZipFile(bad_zip, "w") as archive:
                archive.writestr("..\\escape", b"x")
            with self.assertRaisesRegex(ReleaseVerificationError, "unsafe archive path"):
                archive_names(bad_zip)

    def test_third_party_source_verifier_accepts_upstream_test_keys_but_not_manual_keys(self) -> None:
        import io
        import tarfile

        with tempfile.TemporaryDirectory() as td:
            dist = pathlib.Path(td)
            version = "1.2.3"
            root = f"graphite-meter_{version}_third-party-source"
            path = dist / f"graphite-meter_{version}_third-party-source.tar.gz"

            def write_archive(manual_name: str = "manual.txt") -> None:
                with tarfile.open(path, "w:gz") as archive:
                    members = {
                        f"{root}/README.txt": (
                            "Graphite Meter third-party source.\n"
                            "Use Source code (tar.gz) or Source code (zip).\n"
                            "This archive does not duplicate Graphite Meter's own repository source.\n"
                        ).encode(),
                        f"{root}/LEGAL_INVENTORY.json": b'{"server":[],"tui":[],"container":[]}\n',
                        f"{root}/PROVENANCE.json": b"[]\n",
                        f"{root}/third_party/go/github.com_quic-go_quic-go_at_v0.61.0/internal/testdata/priv.key": b"public upstream test fixture\n",
                        f"{root}/third_party/npm/example/cert.pem": b"public upstream test fixture\n",
                        f"{root}/third_party/manual/sample/{manual_name}": b"manual source\n",
                    }
                    for name, payload in members.items():
                        info = tarfile.TarInfo(name)
                        info.size = len(payload)
                        archive.addfile(info, io.BytesIO(payload))

            write_archive()
            verify_third_party_source_archive(dist, version)

            write_archive("private.key")
            with self.assertRaisesRegex(
                ReleaseVerificationError,
                "certificate/key material outside upstream dependency source",
            ):
                verify_third_party_source_archive(dist, version)

    def test_third_party_source_verifier_rejects_project_source_duplication(self) -> None:
        import io
        import tarfile

        with tempfile.TemporaryDirectory() as td:
            dist = pathlib.Path(td)
            version = "1.2.3"
            root = f"graphite-meter_{version}_third-party-source"
            path = dist / f"graphite-meter_{version}_third-party-source.tar.gz"
            with tarfile.open(path, "w:gz") as archive:
                members = {
                    f"{root}/README.txt": (
                        "Use Source code (tar.gz) or Source code (zip).\n"
                        "This archive does not duplicate Graphite Meter's own repository source.\n"
                    ).encode(),
                    f"{root}/LEGAL_INVENTORY.json": b'{"server":[],"tui":[],"container":[]}\n',
                    f"{root}/PROVENANCE.json": b"[]\n",
                    f"{root}/third_party/manual/sample/source.txt": b"x\n",
                    f"{root}/project/LICENSE": b"duplicate project source\n",
                }
                for name, payload in members.items():
                    info = tarfile.TarInfo(name)
                    info.size = len(payload)
                    archive.addfile(info, io.BytesIO(payload))
            with self.assertRaisesRegex(ReleaseVerificationError, "unexpected non-third-party source paths"):
                verify_third_party_source_archive(dist, version)

    def test_release_verifier_requires_built_client_and_server_outputs(self) -> None:
        previous = pathlib.Path.cwd()
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            (root / "client/dist").mkdir(parents=True)
            (root / "go").mkdir()
            try:
                os.chdir(root)
                with self.assertRaisesRegex(ReleaseVerificationError, "client version metadata is missing"):
                    verify_client_version("1.2.3")
                (root / "client/dist/version.json").write_text(
                    '{"version":"1.2.3","label":"prod","revision":"abc1234"}\n', encoding="utf-8"
                )
                verify_client_version("1.2.3")
                with self.assertRaisesRegex(ReleaseVerificationError, "server binary is missing"):
                    verify_server_version("1.2.3")
                (root / "go/graphite-meter").write_text("placeholder", encoding="utf-8")
                with patch("verify_release_assets.subprocess.run") as run_version:
                    run_version.return_value.returncode = 0
                    run_version.return_value.stdout = "1.2.3\n"
                    run_version.return_value.stderr = ""
                    verify_server_version("1.2.3")
                    run_version.assert_called_once()
                    self.assertEqual(run_version.call_args.args[0][-1], "--version")
            finally:
                os.chdir(previous)

    def test_tui_release_archive_requires_expected_binary(self) -> None:
        import io
        import tarfile

        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            dist = root / "dist"
            dist.mkdir()
            targets = root / "targets.txt"
            targets.write_text("linux/amd64\n", encoding="utf-8")
            base = "graphite-meter-client_1.2.3_linux_amd64"
            archive_path = dist / f"{base}.tar.gz"
            with tarfile.open(archive_path, "w:gz") as archive:
                for name in ("LICENSE", "COPYRIGHT", "THIRD_PARTY_NOTICES.txt", "SOURCE.txt"):
                    payload = b"x"
                    info = tarfile.TarInfo(f"{base}/{name}")
                    info.size = len(payload)
                    archive.addfile(info, io.BytesIO(payload))
            with self.assertRaisesRegex(ReleaseVerificationError, "graphite-meter-client"):
                verify_client_archives(dist, "1.2.3", targets)

    def test_zip_release_archive_rejects_special_file_entries(self) -> None:
        import stat
        import zipfile

        with tempfile.TemporaryDirectory() as td:
            path = pathlib.Path(td) / "special.zip"
            with zipfile.ZipFile(path, "w") as archive:
                info = zipfile.ZipInfo("bundle/device")
                info.create_system = 3
                info.external_attr = (stat.S_IFCHR | 0o600) << 16
                archive.writestr(info, b"")
            with self.assertRaisesRegex(ReleaseVerificationError, "unsupported special entry"):
                archive_names(path)

    def test_release_artifact_set_is_derived_from_supported_targets(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            targets = pathlib.Path(td) / "targets.txt"
            targets.write_text("linux/amd64\nwindows/amd64\n", encoding="utf-8")
            self.assertEqual(
                expected_release_artifacts("1.2.3", targets),
                {
                    "graphite-meter_1.2.3_third-party-source.tar.gz",
                    "graphite-meter-client_1.2.3_linux_amd64.tar.gz",
                    "graphite-meter-client_1.2.3_windows_amd64.zip",
                },
            )

    def test_oci_index_requires_linked_buildkit_provenance(self) -> None:
        amd_digest = "sha256:" + "a" * 64
        arm_digest = "sha256:" + "b" * 64
        manifest_type = "application/vnd.oci.image.manifest.v1+json"
        index: JsonValue = {
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [
                {"mediaType": manifest_type, "digest": amd_digest, "platform": {"os": "linux", "architecture": "amd64"}},
                {"mediaType": manifest_type, "digest": arm_digest, "platform": {"os": "linux", "architecture": "arm64"}},
                {
                    "mediaType": manifest_type,
                    "digest": "sha256:" + "c" * 64,
                    "platform": {"os": "unknown", "architecture": "unknown"},
                    "annotations": {
                        "vnd.docker.reference.type": "attestation-manifest",
                        "vnd.docker.reference.digest": amd_digest,
                    },
                },
                {
                    "mediaType": manifest_type,
                    "digest": "sha256:" + "d" * 64,
                    "platform": {"os": "unknown", "architecture": "unknown"},
                    "annotations": {
                        "vnd.docker.reference.type": "attestation-manifest",
                        "vnd.docker.reference.digest": arm_digest,
                    },
                },
            ]
        }
        from github_api import expect_object

        self.assertEqual(
            validate_index_descriptors(expect_object(index, "test index")),
            {"amd64": amd_digest, "arm64": arm_digest},
        )

        manifests = expect_object(index, "test index")["manifests"]
        assert isinstance(manifests, list)
        bad_missing: JsonObject = {
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": manifests[:-1],
        }
        with self.assertRaisesRegex(OCIVerificationError, "one provenance attestation"):
            validate_index_descriptors(bad_missing)

        bad_link: JsonObject = {
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [*manifests[:-1], {
            "mediaType": manifest_type,
            "digest": "sha256:" + "e" * 64,
            "platform": {"os": "unknown", "architecture": "unknown"},
            "annotations": {
                "vnd.docker.reference.type": "attestation-manifest",
                "vnd.docker.reference.digest": "sha256:" + "f" * 64,
            },
        }]}
        with self.assertRaisesRegex(OCIVerificationError, "one provenance attestation"):
            validate_index_descriptors(bad_link)

        bad_extra: JsonObject = {
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [*manifests, {
            "mediaType": manifest_type,
            "digest": "sha256:" + "e" * 64,
            "platform": {"os": "linux", "architecture": "s390x"},
        }]}
        with self.assertRaisesRegex(OCIVerificationError, "unexpected platform"):
            validate_index_descriptors(bad_extra)

    def test_skopeo_runtime_contract_uses_pinned_image_and_strict_version(self) -> None:
        from unittest.mock import patch

        calls: list[tuple[str, ...]] = []

        def fake_run(*args: str) -> str:
            calls.append(args)
            return "skopeo version 1.22.2"

        with patch.dict(
            "os.environ",
            {
                "SKOPEO_IMAGE": "quay.io/containers/skopeo:v1.22.2-immutable@sha256:" + "a" * 64,
                "SKOPEO_VERSION": "1.22.2",
            },
            clear=False,
        ), patch("verify_oci.select_engine", return_value="docker"), patch("verify_oci.run", side_effect=fake_run):
            engine, image = verify_skopeo_runtime()
        self.assertEqual(engine, "docker")
        self.assertTrue(image.endswith("a" * 64))
        self.assertEqual(calls[0][-1], "--version")
        self.assertIn("--network", calls[0])
        self.assertIn("none", calls[0])

    def _run_promotion(self, digest: str, series: str, latest: str) -> subprocess.CompletedProcess[str]:
        text = (ROOT / ".github/workflows/_promote-oci.yml").read_text(encoding="utf-8")
        step = text.split("name: Promote verified stable image without rollback", 1)[1]
        script = textwrap.dedent(step.split("run: |\n", 1)[1])
        shim = 'docker() { while [ "$1" != -ec ]; do [ "$1" = -e ] && export "$2"; shift; done; sh -ec "$2"; }\n'
        with tempfile.TemporaryDirectory() as td:
            skopeo = pathlib.Path(td) / "skopeo"
            skopeo.write_text('#!/bin/sh\n[ "$1" = inspect ] && printf "%s\\n" "$FAKE_DIGEST"\nexit 0\n')
            skopeo.chmod(0o755)
            env = dict(
                os.environ, PATH=f"{td}{os.pathsep}{os.environ['PATH']}", FAKE_DIGEST=digest,
                REGISTRY_TOKEN="token", REPOSITORY="Owner/Repo", REGISTRY_ACTOR="owner", VERSION="1.2.3",
                SERIES="1.2", PROMOTE_SERIES=series, PROMOTE_LATEST=latest, SKOPEO_IMAGE="skopeo",
            )
            return subprocess.run(["bash", "-c", shim + script], env=env, capture_output=True, text=True)

    def test_promotion_rejects_a_source_digest_with_trailing_data(self) -> None:
        result = self._run_promotion("sha256:" + "a" * 64 + "evil", "true", "true")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("source tag returned an invalid digest", result.stderr)
        self.assertNotIn("promoted", result.stdout)

    def test_promotion_of_an_older_series_succeeds_without_latest(self) -> None:
        result = self._run_promotion("sha256:" + "a" * 64, "true", "false")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("promoted ghcr.io/owner/repo:1.2", result.stdout)
        self.assertNotIn(":latest", result.stdout)

    def test_skopeo_version_parser_accepts_supported_output_shapes(self) -> None:
        self.assertEqual(parse_skopeo_version("skopeo version 1.22.2"), "1.22.2")
        self.assertEqual(
            parse_skopeo_version("skopeo version 1.22.2 commit: abcdef0123456789"),
            "1.22.2",
        )
        self.assertEqual(parse_skopeo_version("skopeo version 1.22.2-custom"), "1.22.2-custom")
        with self.assertRaisesRegex(OCIVerificationError, "unexpected Skopeo --version output"):
            parse_skopeo_version("skopeo 1.22.2")

    def test_oci_verifier_rejects_symlink_archive_before_container_use(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            target = root / "image.oci.tar.real"
            target.write_bytes(b"placeholder")
            archive = root / "image.oci.tar"
            archive.symlink_to(target.name)
            with self.assertRaisesRegex(OCIVerificationError, "not a regular file"):
                verify_oci_archive("1.2.3", HEAD, archive)

    def test_oci_verifier_forces_full_copy_without_host_writable_mount(self) -> None:
        from unittest.mock import patch

        calls: list[tuple[str, ...]] = []

        def fake_run(*args: str) -> str:
            calls.append(args)
            return ""

        with tempfile.TemporaryDirectory() as td:
            archive = pathlib.Path(td) / "image.oci.tar"
            archive.write_bytes(b"placeholder")
            with patch("verify_oci.run", side_effect=fake_run):
                verify_archive_blobs("docker", "skopeo@example", archive)

        self.assertEqual(len(calls), 1)
        call = calls[0]
        self.assertIn("copy", call)
        self.assertIn("--all", call)
        self.assertIn("--network", call)
        self.assertIn("none", call)
        self.assertIn("oci:/tmp/graphite-meter-verified:verified", call)
        mounts = [call[index + 1] for index, value in enumerate(call[:-1]) if value == "-v"]
        self.assertEqual(len(mounts), 1, "OCI verification must mount only the archive")
        self.assertTrue(mounts[0].endswith(":/work/image.oci.tar:ro"))

    def test_prerelease_request_run_is_bound_to_exact_current_main_and_owner(self) -> None:
        request_run_id = 6001
        workflow = 7001
        run: JsonObject = {
            "id": request_run_id,
            "workflow_id": workflow,
            "event": "workflow_dispatch",
            "head_branch": "main",
            "head_sha": MAIN,
            "status": "completed",
            "conclusion": "success",
            "run_attempt": 1,
            "actor": {"login": "zR-JB"},
            "triggering_actor": {"login": "zR-JB"},
        }

        def fake(path: str, *, paginate: bool = False, **_: object) -> JsonValue:
            if path.endswith("/actions/workflows/prerelease-request.yml"):
                return {"id": workflow}
            if path.endswith(f"/actions/runs/{request_run_id}"):
                return run
            if f"/actions/runs/{request_run_id}/artifacts" in path and paginate:
                return [{"artifacts": [{
                    "name": f"prerelease-candidate-{request_run_id}",
                    "expired": False,
                    "size_in_bytes": 4096,
                }]}]
            raise AssertionError((path, paginate))

        self.assertEqual(
            validate_request_run(
                "zR-JB/graphite-meter", "zR-JB", MAIN, request_run_id, api=fake
            ),
            f"prerelease-candidate-{request_run_id}",
        )

        run["head_branch"] = "feature/stale-pipeline"
        with self.assertRaisesRegex(SystemExit, "not dispatched from main"):
            validate_request_run(
                "zR-JB/graphite-meter", "zR-JB", MAIN, request_run_id, api=fake
            )

    def test_stable_request_consumer_binds_exact_main_run_and_artifact(self) -> None:
        request_run_id = 4242
        workflow_id = 31337
        ci_run_id = 5151
        request_dir = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, request_dir)
        (request_dir / "request.json").write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "repository": "zR-JB/graphite-meter",
                    "sourceSha": MAIN,
                    "version": "v1.2.3",
                    "mode": "validate",
                    "requestRunId": request_run_id,
                    "requestRunAttempt": 1,
                }
            ),
            encoding="utf-8",
        )

        request_run: JsonObject = {
            "id": request_run_id,
            "workflow_id": workflow_id,
            "event": "workflow_dispatch",
            "head_branch": "main",
            "head_sha": MAIN,
            "status": "completed",
            "conclusion": "success",
            "run_attempt": 1,
            "actor": {"login": "zR-JB"},
            "triggering_actor": {"login": "zR-JB"},
        }

        def fake(path: str, *, paginate: bool = False, **_: object) -> JsonValue:
            if path.endswith("/commits/main"):
                return {"sha": MAIN}
            if path.endswith("/actions/workflows/release-request.yml"):
                return {"id": workflow_id}
            if path.endswith(f"/actions/runs/{request_run_id}"):
                return request_run
            if f"/actions/runs/{request_run_id}/artifacts" in path and paginate:
                return [{"artifacts": [{
                    "name": f"stable-release-request-{request_run_id}",
                    "expired": False,
                    "size_in_bytes": 1024,
                }]}]
            if "/git/matching-refs/tags/v1.2.3" in path:
                return []
            if "/actions/workflows/ci.yml/runs" in path and paginate:
                return [{"workflow_runs": [{
                    "id": ci_run_id,
                    "head_sha": MAIN,
                    "head_branch": "main",
                    "event": "push",
                    "status": "completed",
                    "conclusion": "success",
                    "run_number": 8,
                    "run_attempt": 1,
                    "updated_at": "2026-08-15T12:00:00Z",
                }]}]
            if f"/actions/runs/{ci_run_id}/jobs" in path and paginate:
                return [{"jobs": [{
                    "name": "Gate",
                    "status": "completed",
                    "conclusion": "success",
                }]}]
            if "/code-scanning/analyses" in path and paginate:
                return [[{
                    "id": 99,
                    "commit_sha": MAIN,
                    "tool": {"name": "CodeQL"},
                    "category": "/language:go",
                    "analysis_key": ".github/workflows/codeql.yml:analyze",
                    "environment": "{}",
                    "created_at": "2026-08-15T12:01:00Z",
                    "error": "",
                    "warning": "",
                }]]
            raise AssertionError((path, paginate))

        env = {
            "REPOSITORY": "zR-JB/graphite-meter",
            "REPOSITORY_OWNER": "zR-JB",
            "PUBLISHER_SHA": MAIN,
            "WORKFLOW_REF": "zR-JB/graphite-meter/.github/workflows/release.yml@refs/heads/main",
            "REQUEST_RUN_ID": str(request_run_id),
            "REQUEST_DIR": str(request_dir),
        }
        with patch.dict(os.environ, env, clear=False), patch("release.git", return_value=MAIN):
            context = validate_request_context(api=fake)
        self.assertEqual(context.sha, MAIN)
        self.assertEqual(context.ci_run_id, ci_run_id)
        self.assertFalse(context.publish)

        request_run["head_branch"] = "feature/release-rewrite"
        with patch.dict(os.environ, env, clear=False), patch("release.git", return_value=MAIN):
            with self.assertRaisesRegex(SystemExit, "not dispatched from main"):
                validate_request_context(api=fake)

class PythonTypeGateTests(unittest.TestCase):
    def test_python_gate_rejects_a_type_error_and_accepts_its_correction(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "scripts/ci").mkdir(parents=True)
            for name in ("mise.toml", "mise.lock"):
                shutil.copy2(ROOT / name, root / name)
            (root / "go").mkdir()
            shutil.copy2(ROOT / "go/go.mod", root / "go/go.mod")
            shutil.copy2(ROOT / "scripts/ci/toolchains.py", root / "scripts/ci/toolchains.py")
            # Run the actual offline task with shared prepared mise tools.
            probe = root / "scripts/type_error_probe.py"
            command = ["mise", "run", "python-check"]
            env = os.environ | {"MISE_TRUSTED_CONFIG_PATHS": str(root), "MISE_AUTO_INSTALL": "0"}
            probe.write_text('def answer() -> int:\n    return "wrong"\n')
            result = subprocess.run(command, cwd=root, capture_output=True, text=True, env=env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("type_error_probe.py", result.stdout + result.stderr)
            self.assertIn("invalid-return-type", result.stdout + result.stderr)
            probe.write_text('def answer() -> int:\n    return 42\n')
            result = subprocess.run(command, cwd=root, capture_output=True, text=True, env=env)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class CodeQLCheckOrderingTests(unittest.TestCase):
    def test_overlapping_checks_use_start_order_and_block_unfinished_work(self) -> None:
        workflow = (ROOT / ".github/workflows/_publish-oci.yml").read_text(encoding="utf-8")
        expression = workflow.split("latest_codeql=$(jq -c --argjson pr \"$PR_NUMBER\" '", 1)[1].split("' <<<\"$check_pages\")", 1)[0]
        self.assertIn("check-runs?per_page=100&filter=all", workflow)
        for status, conclusion, started, completed, allowed in (
            ("in_progress", None, "2026-09-05T10:04:00Z", None, False),
            ("queued", None, None, None, False),
            ("completed", "failure", "2026-09-05T10:04:00Z", "2026-09-05T10:04:30Z", False),
            ("completed", "success", "2026-09-05T10:04:00Z", "2026-09-05T10:04:30Z", True),
        ):
            with self.subTest(status=status, conclusion=conclusion):
                common: dict[str, JsonValue] = {
                    "name": "CodeQL", "app": {"slug": "github-advanced-security"},
                    "pull_requests": [{"number": 101}],
                }
                pages: JsonValue = [{"check_runs": [
                    {**common, "id": 41, "status": "completed", "conclusion": "success",
                     "started_at": "2026-09-05T10:00:00Z", "completed_at": "2026-09-05T10:05:00Z"},
                    {**common, "id": 42, "status": status, "conclusion": conclusion,
                     "started_at": started, "completed_at": completed},
                ]}]

                def api(path: str, **_: object) -> JsonValue:
                    self.assertIn("filter=all", path)
                    return pages

                if allowed:
                    self.assertEqual(require_check_run(
                        "example/repo", HEAD, name="CodeQL", app_slug="github-advanced-security",
                        pr_number=101, api=api,
                    ), 42)
                else:
                    with self.assertRaises(TrustError):
                        require_check_run(
                            "example/repo", HEAD, name="CodeQL", app_slug="github-advanced-security",
                            pr_number=101, api=api,
                        )
                # Execute the real privileged workflow's selector on the same
                # fixtures so Python and the checkout-free final recheck agree.
                result = subprocess.run(
                    ["jq", "-c", "--argjson", "pr", "101", expression + " | .id"],
                    input=json.dumps(pages), text=True, capture_output=True, check=True,
                )
                self.assertEqual(result.stdout.strip(), "42")


if __name__ == "__main__":
    unittest.main()
