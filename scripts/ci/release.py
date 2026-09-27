#!/usr/bin/env python3
"""Validate release requests, then authorize stable releases and PR prereleases."""

from __future__ import annotations

import argparse
import base64
import binascii
import hashlib
import json
import os
import re
import shutil
import sys
import time
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import TypeVar
from urllib.parse import quote

import verify_oci
import verify_release_assets
from github_api import (
    APICall,
    ControlPlaneError,
    JsonObject,
    api as default_api,
    append_output,
    append_summary,
    expect_array,
    expect_object,
    fail,
    file_sha256,
    int_field,
    object_field,
    runner_path,
    str_field,
)
from trust import (
    SEMVER_NUMBER,
    SHA_RE,
    env,
    env_int,
    env_sha,
    exact_files,
    read_record,
    require_check_run,
    require_checkout,
    require_ci_gate,
    require_control_plane_matches_main,
    require_current_main,
    require_dispatch_run,
    require_exact_current_main,
    require_main_codeql,
    require_pr,
    require_protected_environment,
)

N = SEMVER_NUMBER
TAG_RE = re.compile(rf"v{N}\.{N}\.{N}(-(?:alpha|beta|rc)\.{N})?")
OCI = "graphite-meter.oci.tar"
OCI_LIMIT = 1024 * 1024 * 1024
ASSETS_LIMIT = 2 * OCI_LIMIT
# Seconds between reads while GitHub's read path catches up with a write.
DELAYS = (0.25, 0.5, 1, 2, 4, 8)
T = TypeVar("T")
REQUEST_KEYS = {
    "schemaVersion", "repository", "tag", "sourceSha", "pr", "mode", "requestRunId",
    "requestRunAttempt", "rust",
}


@dataclass(frozen=True)
class Release:
    tag: str
    sha: str
    pr: int
    rust: str = "none"

    @property
    def rust_server(self) -> bool:
        return self.rust in ("server", "both")

    @property
    def stable(self) -> bool:
        return self.pr == 0

    @property
    def version(self) -> str:
        return self.tag[1:]


def assets_sha256(directory: Path) -> str:
    """Hash the sorted name and SHA-256 of every regular file in `directory`."""
    entries = sorted(directory.iterdir())
    exact_files(directory, {entry.name for entry in entries})
    listing = "".join(f"{entry.name}\t{file_sha256(entry)}\n" for entry in entries)
    return hashlib.sha256(listing.encode()).hexdigest()


def parse_release(tag: str, sha: str, pr: int, rust: str = "none") -> Release:
    match = TAG_RE.fullmatch(tag)
    if pr < 0 or match is None or (match.group(1) is None) != (pr == 0):
        fail("stable tags are vMAJOR.MINOR.PATCH; PR prereleases add -{alpha,beta,rc}.N")
    if SHA_RE.fullmatch(sha) is None:
        fail("release source must be a 40-character commit SHA")
    if rust not in ("none", "server", "tui", "both"):
        fail("rust must be none, server, tui or both")
    return Release(tag, sha, pr, rust)


def request_title(mode: str, release: Release, main: str) -> str:
    """The run-name that release-request.yml derives from its dispatch inputs."""
    source = f"PR #{release.pr} @ {release.sha}" if release.pr else "main"
    return f"Release request · {mode} · {release.tag} · {source} · {main} · Rust {release.rust}"


def main_workflow(repository: str, name: str) -> str:
    return f"{repository}/.github/workflows/{name}@refs/heads/main"


def release_tag_target(repository: str, tag: str, *, api: APICall = default_api) -> str | None:
    """Return the commit the exact tag names, through an annotated tag, or None without the tag."""
    refs = expect_array(api(f"repos/{repository}/git/matching-refs/tags/{tag}"), tag)
    exact = [expect_object(ref, tag) for ref in refs
             if isinstance(ref, dict) and ref.get("ref") == f"refs/tags/{tag}"]
    if not exact:
        return None
    if len(exact) != 1:
        fail(f"multiple exact refs unexpectedly match {tag}")
    target = object_field(exact[0], "object", tag)
    if target.get("type") == "tag":
        annotated = api(f"repos/{repository}/git/tags/{str_field(target, 'sha', tag)}")
        target = object_field(expect_object(annotated, tag), "object", tag)
    if target.get("type") != "commit":
        fail(f"{tag} does not reference a commit")
    return str_field(target, "sha", tag)


def require_compatible_release_tag(
    repository: str, tag: str, expected_sha: str, *, api: APICall = default_api,
) -> None:
    """Refuse before publication if the exact tag already names another commit."""
    if (sha := release_tag_target(repository, tag, api=api)) not in (None, expected_sha):
        fail(f"{tag} already exists at {sha}, expected {expected_sha}")


def converge(what: str, probe: Callable[[], T | None]) -> T:
    """Return the first value `probe` reads, waiting for GitHub's read path to show a write."""
    for delay in (0, *DELAYS):
        if delay:
            print(f"::notice::waiting {delay}s for {what}", file=sys.stderr)
            time.sleep(delay)
        if (value := probe()) is not None:
            return value
    fail(f"{what} did not become visible in time")


def require_publishable(
    repository: str, release: Release, *, api: APICall = default_api,
) -> tuple[str, int, str]:
    """Return current main, the CI run and the PR CodeQL check that authorize `release`."""
    sha = release.sha
    if release.stable:
        main = require_exact_current_main(repository, sha, api=api)
        require_compatible_release_tag(repository, release.tag, sha, api=api)
        ci_run_id = require_ci_gate(repository, sha, event="push", branch="main", api=api)
        require_main_codeql(repository, sha, api=api)
        return main, ci_run_id, ""
    pr = release.pr
    branch = require_pr(repository, pr, sha, api=api)
    main = require_current_main(repository, pr, sha, api=api)
    require_control_plane_matches_main(repository, sha, main, api=api)
    ci_run_id = require_ci_gate(
        repository, sha, event="pull_request", branch=branch, pr_number=pr, api=api,
    )
    codeql_id = require_check_run(
        repository, sha, name="CodeQL", app_slug="github-advanced-security", pr_number=pr, api=api,
    )
    return main, ci_run_id, str(codeql_id)


def command_prepare() -> None:
    repository, owner = env("REPOSITORY"), env("REPOSITORY_OWNER")
    main = env_sha("EVENT_SHA")
    if env("EVENT_NAME") != "workflow_dispatch" or env("REF") != "refs/heads/main":
        fail("release requests must be dispatched from main")
    if env("WORKFLOW_REF") != main_workflow(repository, "release-request.yml"):
        fail("workflow is not the release request workflow on main")
    if env("ACTOR") != owner or env("TRIGGERING_ACTOR") != owner:
        fail("only the repository owner may request a release")
    if env_int("REQUEST_RUN_ATTEMPT") != 1:
        fail("workflow reruns are not valid requests; start a fresh dispatch")
    if (mode := env("MODE")) not in ("validate", "publish"):
        fail("mode must be validate or publish")
    pr = env_int("PR") if os.environ.get("PR") else 0
    if not pr and os.environ.get("SHA"):
        fail("stable releases build current main; leave sha empty")
    release = parse_release(env("TAG"), env_sha("SHA") if pr else main, pr,
                            os.environ.get("RUST", "none"))
    out = runner_path("OUT_DIR")
    out.mkdir(parents=True, exist_ok=True)
    request = {
        "schemaVersion": 3, "repository": repository, "tag": release.tag, "rust": release.rust,
        "sourceSha": release.sha, "pr": pr, "mode": mode,
        "requestRunId": env_int("REQUEST_RUN_ID"), "requestRunAttempt": 1,
    }
    (out / "request.json").write_text(json.dumps(request, indent=2, sort_keys=True) + "\n")
    append_output(
        tag=release.tag, version=release.version, sha=release.sha,
        stable=str(release.stable).lower(), remote_sha="" if release.stable else release.sha,
        client_validate="0" if release.stable else "1", rust=release.rust,
        rust_server=str(release.rust_server).lower(),
        rust_tui=str(release.rust in ("tui", "both")).lower(),
    )


def verify_request(request_dir: Path, *, api: APICall = default_api) -> tuple[Release, bool]:
    """Bind the untrusted request artifact to its run, current main and the release rules."""
    repository = env("REPOSITORY")
    publisher = env_sha("PUBLISHER_SHA")
    run_id = env_int("REQUEST_RUN_ID")
    if env("WORKFLOW_REF") != main_workflow(repository, "release.yml"):
        fail("release consumer is not the trusted main workflow")
    require_exact_current_main(repository, publisher, api=api)
    require_checkout(publisher)
    candidate = request_dir / f"release-request-{run_id}"
    request_files = {"request.json", OCI, f"{OCI}.sha256"}
    request_path = candidate / "request.json"
    if request_path.is_symlink() or not request_path.is_file() or request_path.stat().st_size > 1024 * 1024:
        fail("request.json must be a bounded regular file")
    request = read_record(request_path, REQUEST_KEYS, {
        "schemaVersion": 3, "repository": repository, "requestRunId": run_id,
        "requestRunAttempt": 1,
    })
    release = parse_release(str_field(request, "tag", "request"),
                            str_field(request, "sourceSha", "request"),
                            int_field(request, "pr", "request"), str_field(request, "rust", "request"))
    if release.rust_server:
        request_files |= {"graphite-meter-rust.oci.tar", "graphite-meter-rust.oci.tar.sha256"}
    exact_files(candidate, request_files)
    if request["mode"] not in ("validate", "publish"):
        fail("request mode must be validate or publish")
    if release.stable and release.sha != publisher:
        fail("a stable release must build the trusted main commit")
    artifacts = {candidate.name: OCI_LIMIT * (2 if release.rust_server else 1) + 1024 * 1024}
    if release.stable:
        artifacts[f"release-assets-{run_id}"] = ASSETS_LIMIT
    elif release.rust != "none":
        artifacts[f"release-rust-assets-{run_id}"] = ASSETS_LIMIT
    require_dispatch_run(repository, env("REPOSITORY_OWNER"), publisher, run_id,
                         "release-request.yml", request_title(str(request["mode"]), release,
                                                              publisher), artifacts, api=api)
    if (downloaded := {path.name for path in request_dir.iterdir()}) != set(artifacts):
        fail(f"downloaded artifacts are {sorted(downloaded)}; expected {sorted(artifacts)}")
    return release, request["mode"] == "publish"


def command_verify() -> None:
    request_dir, handoff = runner_path("REQUEST_DIR"), runner_path("HANDOFF_DIR")
    release, publish = verify_request(request_dir)
    if publish:
        require_protected_environment(env("REPOSITORY"))
    candidate = request_dir / f"release-request-{env_int('REQUEST_RUN_ID')}"
    if (candidate / OCI).stat().st_size > OCI_LIMIT:
        fail(f"OCI archive exceeds {OCI_LIMIT} bytes")
    digest = file_sha256(candidate / OCI)
    if (candidate / f"{OCI}.sha256").read_text(encoding="utf-8") != f"{digest}  {OCI}\n":
        fail("OCI archive does not match the request checksum")
    manifest = verify_oci.verify(release.version, release.sha, candidate / OCI)
    rust_digest = rust_manifest = ""
    if release.rust_server:
        rust_archive = candidate / "graphite-meter-rust.oci.tar"
        if rust_archive.stat().st_size > OCI_LIMIT:
            fail("Rust OCI archive exceeds size limit")
        rust_digest = file_sha256(rust_archive)
        checksum = (candidate / "graphite-meter-rust.oci.tar.sha256").read_text(encoding="utf-8")
        if checksum != f"{rust_digest}  graphite-meter-rust.oci.tar\n":
            fail("Rust OCI archive does not match the request checksum")
        rust_manifest = verify_oci.verify(release.version + "-rust", release.sha, rust_archive, {"amd64"})
    assets = request_dir / f"release-assets-{env_int('REQUEST_RUN_ID')}"
    if release.stable:
        verify_release_assets.verify_artifacts(release.version, assets, release.rust)
    if not release.stable and release.rust != "none":
        rust_assets = request_dir / f"release-rust-assets-{env_int('REQUEST_RUN_ID')}"
        checksummed = verify_release_assets.verify_checksums(rust_assets)
        verify_release_assets.require_same("Rust source artifacts",
            verify_release_assets.expected_rust_artifacts(release.version, release.rust), checksummed)
        verify_release_assets.verify_release_file_set(rust_assets, checksummed)
        lock_path = f"repos/{env('REPOSITORY')}/contents/rust/Cargo.lock?ref={release.sha}"
        record = expect_object(default_api(lock_path), "Cargo lock")
        content = str_field(record, "content", "Cargo lock")
        if record.get("encoding") != "base64" or len(content) > 4 * 1024 * 1024:
            fail("source Cargo lock is not bounded base64 content")
        try:
            lock = base64.b64decode(content.replace("\n", ""), validate=True)
            lock_sha256 = hashlib.sha256(lock).hexdigest()
        except binascii.Error as exc:
            raise ControlPlaneError("source Cargo lock is invalid base64") from exc
        if release.rust_server:
            verify_release_assets.verify_rust_server_source(rust_assets, release.version, lock_sha256)
        if release.rust in ("tui", "both"):
            verify_release_assets.verify_rust_client_archive(rust_assets, release.version, lock_sha256)
    main, ci_run_id, codeql_id = require_publishable(env("REPOSITORY"), release)
    if main != env("PUBLISHER_SHA"):
        fail("main moved during verification; start a fresh request")

    (handoff / "image").mkdir(parents=True, exist_ok=True)
    shutil.copyfile(candidate / OCI, handoff / "image" / OCI)
    if release.rust_server:
        (handoff / "rust-image").mkdir()
        shutil.copyfile(candidate / "graphite-meter-rust.oci.tar", handoff / "rust-image" / OCI)
    if release.stable:
        shutil.copytree(assets, handoff / "assets")
    append_output(
        tag=release.tag, version=release.version, stable=str(release.stable).lower(),
        publish=str(publish).lower(), sha=release.sha, main_sha=main, pr=release.pr or "",
        oci_sha256=digest, digest=manifest, rust=release.rust,
        rust_oci_sha256=rust_digest, rust_digest=rust_manifest,
        assets_sha256=assets_sha256(handoff / "assets") if release.stable else "",
    )
    append_summary(
        f"### {'Stable release' if release.stable else f'PR #{release.pr} prerelease'} verified"
        f"\n\n`{release.tag}` from `{release.sha}` on main `{main}`: CI run `{ci_run_id}`, "
        f"CodeQL {codeql_id or 'on main'}, OCI SHA-256 `{digest}`. "
        + ("Publication still requires `ghcr-release` approval." if publish
           else "Validation mode cannot reach a write-permission job.")
    )


def command_recheck() -> None:
    main = env_sha("MAIN_SHA")
    pr = env_int("PR") if os.environ.get("PR") else 0
    release = parse_release(env("TAG"), env_sha("SOURCE_SHA"), pr, os.environ.get("RUST", "none"))
    require_checkout(main)
    handoff = runner_path("HANDOFF_DIR")
    exact_files(handoff / "image", {OCI})
    if file_sha256(handoff / "image" / OCI) != env("OCI_SHA256"):
        fail("approved OCI handoff does not match the verified archive")
    if release.stable and assets_sha256(handoff / "assets") != env("ASSETS_SHA256"):
        fail("approved asset handoff does not match the verified assets")
    if release.rust_server:
        exact_files(handoff / "rust-image", {OCI})
        if file_sha256(handoff / "rust-image" / OCI) != env("RUST_OCI_SHA256"):
            fail("approved Rust OCI handoff does not match the verified archive")
    current, ci_run_id, codeql_id = require_publishable(env("REPOSITORY"), release)
    if current != main:
        fail("main moved after verification; start a fresh request")
    append_summary(
        f"### Final release recheck passed\n\n`{release.tag}` from `{release.sha}` is still "
        f"authorized on main `{main}` after approval: CI run `{ci_run_id}`, "
        f"CodeQL {codeql_id or 'on main'}."
    )


def release_assets(repository: str, release_id: int) -> list[JsonObject]:
    pages = default_api(f"repos/{repository}/releases/{release_id}/assets?per_page=100", paginate=True)
    return [expect_object(item, "asset") for page in expect_array(pages, "assets")
            for item in expect_array(page, "assets")]


def asset_digests(repository: str, release_id: int) -> dict[str, str]:
    assets = release_assets(repository, release_id)
    return {str_field(asset, "name", "asset"): str(asset.get("digest") or "") for asset in assets}


def source_notice(release: Release, source: str) -> str:
    return (
        "## Source availability\n\n"
        f"Graphite Meter source for this release is the repository snapshot at tag **{release.tag}** "
        f"(commit **{release.sha}**). GitHub provides that tagged project source below as "
        "**Source code (zip)** and **Source code (tar.gz)**.\n\n"
        "Source for third-party components included in the distributed artifacts is attached as "
        f"**{source}**. Together, the tagged repository source and that archive form the source "
        "offer for this release."
    )


def command_publish() -> None:
    """Publish the verified assets as the stable GitHub Release at its exact tag, idempotently."""
    gh, repository = default_api, env("REPOSITORY")
    release = parse_release(env("TAG"), env_sha("TARGET_SHA"), 0)
    tag, base = release.tag, f"repos/{repository}"
    assets = runner_path("ASSETS_DIR")
    exact_files(assets, names := {entry.name for entry in assets.iterdir()})
    local = {name: "sha256:" + file_sha256(assets / name) for name in names}
    source = f"graphite-meter_{release.version}_third-party-source.tar.gz"
    if source not in local:
        fail(f"release handoff is missing the third-party source asset {source}")
    notice = source_notice(release, source)
    rust_sources = sorted(name for name in names if name.endswith("_rust_third-party-source.tar.gz"))
    if rust_sources:
        notice += "\n\nMatching experimental Rust dependency sources: " + ", ".join(
            f"**{name}**" for name in rust_sources) + "."

    def require_tag() -> None:
        if (sha := converge(f"{tag} visibility", lambda: release_tag_target(repository, tag))) != release.sha:
            fail(f"{tag} resolves to {sha}, expected {release.sha}")

    require_compatible_release_tag(repository, tag, release.sha)
    pages = expect_array(gh(f"{base}/releases?per_page=100", paginate=True), "releases")
    matches = [expect_object(item, "release") for page in pages
               for item in expect_array(page, "releases") if isinstance(item, dict)
               and item.get("tag_name") == tag]
    if len(matches) > 1:
        fail(f"multiple releases unexpectedly use tag {tag}")
    if matches and matches[0].get("prerelease") is not False:
        fail(f"{tag} already exists as a prerelease")
    if matches and matches[0].get("draft") is False:
        if asset_digests(repository, int_field(matches[0], "id", "release")) != local:
            fail(f"{tag} is published but asset names/digests differ")
        require_tag()
        if not str(matches[0].get("body") or "").startswith(notice):
            fail(f"{tag} is published but its source-availability notice is missing or stale")
        print(f"::notice::{tag} is already published with the expected source, assets and notice")
        return
    if not matches:
        draft: JsonObject = {"tag_name": tag, "target_commitish": release.sha, "draft": True,
                             "prerelease": False, "generate_release_notes": True, "body": notice}
        matches = [expect_object(gh(f"{base}/releases", method="POST", body=draft), "release")]
    release_id = int_field(matches[0], "id", "release")

    current = expect_object(gh(f"{base}/releases/{release_id}"), "release")
    if current.get("tag_name") != tag or current.get("draft") is not True:
        fail(f"release {release_id} must remain the {tag} draft during asset upload")
    body = str(current.get("body") or "")
    if not body.startswith(notice):
        if "## Source availability" in body:
            fail(f"{tag} draft contains a stale source-availability notice")
        edit: JsonObject = {"body": f"{notice}\n\n{body}" if body else notice}
        current = expect_object(gh(f"{base}/releases/{release_id}", method="PATCH", body=edit),
                                "release")
    upload = str_field(current, "upload_url", "release").split("{", 1)[0]
    if not upload.startswith("https://uploads.github.com/"):
        fail(f"unexpected release upload URL {upload}")
    # A retry starts from an empty draft so it cannot keep stale files.
    for asset in release_assets(repository, release_id):
        gh(f"{base}/releases/assets/{int_field(asset, 'id', 'asset')}", method="DELETE")
    for name in sorted(local):
        gh(f"{upload}?name={quote(name)}", method="POST", upload=assets / name)
    if asset_digests(repository, release_id) != local:
        fail("draft release asset names/digests do not match verified local files")

    if release_tag_target(repository, tag) is None:
        try:
            gh(f"{base}/git/refs", method="POST", body={"ref": f"refs/tags/{tag}", "sha": release.sha})
        except ControlPlaneError as exc:
            if "already exists" not in str(exc):
                fail(f"GitHub rejected creation of {tag} at {release.sha}: {exc}")
            print(f"::warning::{tag} creation raced with another writer; verifying the winner")
    require_tag()
    try:
        gh(f"{base}/releases/{release_id}", method="PATCH",
           body={"draft": False, "prerelease": False, "make_latest": "legacy"})
    except ControlPlaneError as exc:
        print(f"::warning::publishing {tag} failed ({exc}); reconciling release state")

    def published() -> JsonObject | None:
        try:
            item = expect_object(gh(f"{base}/releases/{release_id}"), "release")
        except ControlPlaneError as exc:
            if "(HTTP 404)" in str(exc):
                return None
            raise
        if item.get("tag_name") != tag or item.get("prerelease") is not False:
            fail(f"release {release_id} is no longer the stable {tag} release")
        return item if item.get("draft") is False else None

    final = converge(f"{tag} publication", published)
    require_tag()
    if not str(final.get("body") or "").startswith(notice):
        fail("published release lost its source-availability notice")
    print(f"::notice::published {tag} with verified SHA-256 assets and source notice")


COMMANDS = {
    "prepare": command_prepare, "verify": command_verify, "recheck": command_recheck,
    "publish": command_publish,
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=COMMANDS)
    try:
        COMMANDS[parser.parse_args().command]()
    except (ControlPlaneError, OSError) as exc:
        raise SystemExit(f"Release refused: {exc}") from exc


if __name__ == "__main__":
    main()
