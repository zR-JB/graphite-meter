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
import tempfile
import time
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import TypeVar
from urllib.parse import quote

from . import github_api as gh, verify_oci, verify_release_assets
from .trust import (
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
    def rust_tui(self) -> bool:
        return self.rust in ("tui", "both")

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
    listing = "".join(f"{entry.name}\t{gh.file_sha256(entry)}\n" for entry in entries)
    return hashlib.sha256(listing.encode()).hexdigest()


def parse_release(tag: str, sha: str, pr: int, rust: str = "none") -> Release:
    match = TAG_RE.fullmatch(tag)
    if pr < 0 or match is None or (match.group(1) is None) != (pr == 0):
        gh.fail("stable tags are vMAJOR.MINOR.PATCH; PR prereleases add -{alpha,beta,rc}.N")
    if SHA_RE.fullmatch(sha) is None:
        gh.fail("release source must be a 40-character commit SHA")
    if rust not in ("none", "server", "tui", "both"):
        gh.fail("rust must be none, server, tui or both")
    return Release(tag, sha, pr, rust)


def request_title(mode: str, release: Release, main: str) -> str:
    """The run-name that release-request.yml derives from its dispatch inputs."""
    source = f"PR #{release.pr} @ {release.sha}" if release.pr else "main"
    return f"Release request · {mode} · {release.tag} · {source} · {main} · Rust {release.rust}"


def main_workflow(repository: str, name: str) -> str:
    return f"{repository}/.github/workflows/{name}@refs/heads/main"


def release_tag_target(repository: str, tag: str) -> str | None:
    """Return the commit the exact tag names, through an annotated tag, or None without the tag."""
    refs = gh.expect_array(gh.api(f"repos/{repository}/git/matching-refs/tags/{tag}"), tag)
    exact = [gh.expect_object(ref, tag) for ref in refs
             if isinstance(ref, dict) and ref.get("ref") == f"refs/tags/{tag}"]
    if not exact:
        return None
    if len(exact) != 1:
        gh.fail(f"multiple exact refs unexpectedly match {tag}")
    target = gh.object_field(exact[0], "object", tag)
    if target.get("type") == "tag":
        annotated = gh.api(f"repos/{repository}/git/tags/{gh.str_field(target, 'sha', tag)}")
        target = gh.object_field(gh.expect_object(annotated, tag), "object", tag)
    if target.get("type") != "commit":
        gh.fail(f"{tag} does not reference a commit")
    return gh.str_field(target, "sha", tag)


def require_compatible_release_tag(repository: str, tag: str, expected_sha: str) -> None:
    """Refuse before publication if the exact tag already names another commit."""
    if (sha := release_tag_target(repository, tag)) not in (None, expected_sha):
        gh.fail(f"{tag} already exists at {sha}, expected {expected_sha}")


def converge(what: str, probe: Callable[[], T | None]) -> T:
    """Return the first value `probe` reads, waiting for GitHub's read path to show a write."""
    for delay in (0, *DELAYS):
        if delay:
            print(f"::notice::waiting {delay}s for {what}", file=sys.stderr)
            time.sleep(delay)
        if (value := probe()) is not None:
            return value
    gh.fail(f"{what} did not become visible in time")


def require_publishable(repository: str, release: Release) -> tuple[str, int, str]:
    """Return current main, the CI run and the PR CodeQL check that authorize `release`."""
    sha = release.sha
    if release.stable:
        main = require_exact_current_main(repository, sha)
        require_compatible_release_tag(repository, release.tag, sha)
        ci_run_id = require_ci_gate(repository, sha, event="push", branch="main")
        require_main_codeql(repository, sha)
        return main, ci_run_id, ""
    pr = release.pr
    branch = require_pr(repository, pr, sha)
    main = require_current_main(repository, pr, sha)
    require_control_plane_matches_main(repository, sha, main)
    ci_run_id = require_ci_gate(repository, sha, event="pull_request", branch=branch, pr_number=pr)
    codeql_id = require_check_run(
        repository, sha, name="CodeQL", app_slug="github-advanced-security", pr_number=pr,
    )
    return main, ci_run_id, str(codeql_id)


def command_prepare() -> None:
    repository, owner = env("REPOSITORY"), env("REPOSITORY_OWNER")
    main = env_sha("EVENT_SHA")
    if env("EVENT_NAME") != "workflow_dispatch" or env("REF") != "refs/heads/main":
        gh.fail("release requests must be dispatched from main")
    if env("WORKFLOW_REF") != main_workflow(repository, "release-request.yml"):
        gh.fail("workflow is not the release request workflow on main")
    if env("ACTOR") != owner or env("TRIGGERING_ACTOR") != owner:
        gh.fail("only the repository owner may request a release")
    if env_int("REQUEST_RUN_ATTEMPT") != 1:
        gh.fail("workflow reruns are not valid requests; start a fresh dispatch")
    if (mode := env("MODE")) not in ("validate", "publish"):
        gh.fail("mode must be validate or publish")
    pr = env_int("PR") if os.environ.get("PR") else 0
    if not pr and os.environ.get("SHA"):
        gh.fail("stable releases build current main; leave sha empty")
    release = parse_release(env("TAG"), env_sha("SHA") if pr else main, pr,
                            os.environ.get("RUST", "none"))
    out = gh.runner_path("OUT_DIR")
    out.mkdir(parents=True, exist_ok=True)
    request = {
        "schemaVersion": 3, "repository": repository, "tag": release.tag, "rust": release.rust,
        "sourceSha": release.sha, "pr": pr, "mode": mode,
        "requestRunId": env_int("REQUEST_RUN_ID"), "requestRunAttempt": 1,
    }
    (out / "request.json").write_text(json.dumps(request, indent=2, sort_keys=True) + "\n")
    gh.append_output(
        tag=release.tag, version=release.version, sha=release.sha,
        stable=str(release.stable).lower(), remote_sha="" if release.stable else release.sha,
        client_validate="0" if release.stable else "1", rust=release.rust,
        rust_server=str(release.rust_server).lower(),
        rust_tui=str(release.rust_tui).lower(),
    )


def verify_request(request_dir: Path) -> tuple[Release, bool]:
    """Bind the untrusted request artifact to its run, current main and the release rules."""
    repository = env("REPOSITORY")
    publisher = env_sha("PUBLISHER_SHA")
    run_id = env_int("REQUEST_RUN_ID")
    if env("WORKFLOW_REF") != main_workflow(repository, "release.yml"):
        gh.fail("release consumer is not the trusted main workflow")
    require_exact_current_main(repository, publisher)
    require_checkout(publisher)
    candidate = request_dir / f"release-request-{run_id}"
    request_files = {"request.json", OCI, f"{OCI}.sha256"}
    request_path = candidate / "request.json"
    if request_path.is_symlink() or not request_path.is_file() or request_path.stat().st_size > 1024 * 1024:
        gh.fail("request.json must be a bounded regular file")
    request = read_record(request_path, REQUEST_KEYS, {
        "schemaVersion": 3, "repository": repository, "requestRunId": run_id,
        "requestRunAttempt": 1,
    })
    release = parse_release(gh.str_field(request, "tag", "request"),
                            gh.str_field(request, "sourceSha", "request"),
                            gh.int_field(request, "pr", "request"), gh.str_field(request, "rust", "request"))
    if release.rust_server:
        request_files |= {"graphite-meter-rust.oci.tar", "graphite-meter-rust.oci.tar.sha256"}
    exact_files(candidate, request_files)
    if request["mode"] not in ("validate", "publish"):
        gh.fail("request mode must be validate or publish")
    if release.stable and release.sha != publisher:
        gh.fail("a stable release must build the trusted main commit")
    artifacts = {candidate.name: OCI_LIMIT * (2 if release.rust_server else 1) + 1024 * 1024}
    if release.stable:
        artifacts[f"release-assets-{run_id}"] = ASSETS_LIMIT
    if release.rust != "none":
        artifacts[f"release-rust-assets-{run_id}"] = ASSETS_LIMIT
    if release.rust_tui:
        artifacts[f"release-rust-darwin-{run_id}"] = ASSETS_LIMIT
    require_dispatch_run(repository, env("REPOSITORY_OWNER"), publisher, run_id,
                         "release-request.yml", request_title(str(request["mode"]), release,
                                                              publisher), artifacts)
    if (downloaded := {path.name for path in request_dir.iterdir()}) != set(artifacts):
        gh.fail(f"downloaded artifacts are {sorted(downloaded)}; expected {sorted(artifacts)}")
    return release, request["mode"] == "publish"


def command_verify() -> None:
    request_dir, handoff = gh.runner_path("REQUEST_DIR"), gh.runner_path("HANDOFF_DIR")
    release, publish = verify_request(request_dir)
    if publish:
        require_protected_environment(env("REPOSITORY"))
    run_id, assets = env_int("REQUEST_RUN_ID"), request_dir / "verified-assets"
    candidate = request_dir / f"release-request-{run_id}"
    if (candidate / OCI).stat().st_size > OCI_LIMIT:
        gh.fail(f"OCI archive exceeds {OCI_LIMIT} bytes")
    digest = gh.file_sha256(candidate / OCI)
    if (candidate / f"{OCI}.sha256").read_text(encoding="utf-8") != f"{digest}  {OCI}\n":
        gh.fail("OCI archive does not match the request checksum")
    manifest = verify_oci.verify(release.version, release.sha, candidate / OCI)
    rust_digest = rust_manifest = ""
    if release.rust_server:
        rust_archive = candidate / "graphite-meter-rust.oci.tar"
        if rust_archive.stat().st_size > OCI_LIMIT:
            gh.fail("Rust OCI archive exceeds size limit")
        rust_digest = gh.file_sha256(rust_archive)
        checksum = (candidate / "graphite-meter-rust.oci.tar.sha256").read_text(encoding="utf-8")
        if checksum != f"{rust_digest}  graphite-meter-rust.oci.tar\n":
            gh.fail("Rust OCI archive does not match the request checksum")
        rust_manifest = verify_oci.verify(release.version + "-rust", release.sha, rust_archive)
    if release.stable:
        verify_release_assets.verify_artifacts(release.version, request_dir / f"release-assets-{run_id}")
        verify_release_assets.merge(request_dir / f"release-assets-{run_id}", assets)
    if release.rust != "none":
        lock_sha256 = None
        if not release.stable:
            lock_path = f"repos/{env('REPOSITORY')}/contents/rust/Cargo.lock?ref={release.sha}"
            record = gh.expect_object(gh.api(lock_path), "Cargo lock")
            content = gh.str_field(record, "content", "Cargo lock")
            if record.get("encoding") != "base64" or len(content) > 4 * 1024 * 1024:
                gh.fail("source Cargo lock is not bounded base64 content")
            try:
                lock_sha256 = hashlib.sha256(base64.b64decode(content.replace("\n", ""), validate=True)).hexdigest()
            except binascii.Error as exc:
                raise gh.ControlPlaneError("source Cargo lock is invalid base64") from exc
        parts = [request_dir / f"release-rust-{part}-{run_id}" for part in ("assets", "darwin")[:1 + release.rust_tui]]
        verify_release_assets.verify_rust(parts, assets, release.version, *verify_release_assets.rust_builds(release.rust),
                                          release.sha, env("REPOSITORY"), lock_sha256)
    if assets.exists():
        gh.write_checksums(assets)
    main, ci_run_id, codeql_id = require_publishable(env("REPOSITORY"), release)
    if main != env("PUBLISHER_SHA"):
        gh.fail("main moved during verification; start a fresh request")

    (handoff / "image").mkdir(parents=True, exist_ok=True)
    shutil.copyfile(candidate / OCI, handoff / "image" / OCI)
    if release.rust_server:
        (handoff / "rust-image").mkdir()
        shutil.copyfile(candidate / "graphite-meter-rust.oci.tar", handoff / "rust-image" / OCI)
    if assets.exists():
        shutil.copytree(assets, handoff / "assets")
    gh.append_output(
        tag=release.tag, version=release.version, stable=str(release.stable).lower(),
        publish=str(publish).lower(), sha=release.sha, main_sha=main, pr=release.pr or "",
        oci_sha256=digest, digest=manifest, rust=release.rust,
        rust_oci_sha256=rust_digest, rust_digest=rust_manifest,
        assets_sha256=assets_sha256(handoff / "assets") if assets.exists() else "",
    )
    gh.append_summary(
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
    handoff = gh.runner_path("HANDOFF_DIR")
    exact_files(handoff / "image", {OCI})
    if gh.file_sha256(handoff / "image" / OCI) != env("OCI_SHA256"):
        gh.fail("approved OCI handoff does not match the verified archive")
    if (release.stable or release.rust != "none") and assets_sha256(handoff / "assets") != env("ASSETS_SHA256"):
        gh.fail("approved asset handoff does not match the verified assets")
    if release.rust_server:
        exact_files(handoff / "rust-image", {OCI})
        if gh.file_sha256(handoff / "rust-image" / OCI) != env("RUST_OCI_SHA256"):
            gh.fail("approved Rust OCI handoff does not match the verified archive")
    current, ci_run_id, codeql_id = require_publishable(env("REPOSITORY"), release)
    if current != main:
        gh.fail("main moved after verification; start a fresh request")
    gh.append_summary(
        f"### Final release recheck passed\n\n`{release.tag}` from `{release.sha}` is still "
        f"authorized on main `{main}` after approval: CI run `{ci_run_id}`, "
        f"CodeQL {codeql_id or 'on main'}."
    )


def release_assets(repository: str, release_id: int) -> list[gh.JsonObject]:
    pages = gh.api(f"repos/{repository}/releases/{release_id}/assets?per_page=100", paginate=True)
    return [gh.expect_object(item, "asset") for page in gh.expect_array(pages, "assets")
            for item in gh.expect_array(page, "assets")]


def asset_digests(repository: str, release_id: int) -> dict[str, str]:
    assets = release_assets(repository, release_id)
    return {gh.str_field(asset, "name", "asset"): str(asset.get("digest") or "") for asset in assets}


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
    """Publish verified native artifacts at their exact source tag, idempotently."""
    repository = env("REPOSITORY")
    pr = env_int("PR") if os.environ.get("PR") else 0
    release = parse_release(env("TAG"), env_sha("SOURCE_SHA") if pr else env_sha("TARGET_SHA"), pr,
                            os.environ.get("RUST", "none"))
    if not release.stable and release.rust == "none":
        gh.fail("PR GitHub Releases require an explicit Rust artifact opt-in")
    tag, base = release.tag, f"repos/{repository}"
    prerelease = not release.stable
    assets = gh.runner_path("ASSETS_DIR")
    exact_files(assets, names := {entry.name for entry in assets.iterdir()})
    local = {name: "sha256:" + gh.file_sha256(assets / name) for name in names}
    source = f"graphite-meter_{release.version}_third-party-source.tar.gz"
    rust_sources = sorted(name for name in names if name.endswith("_rust_third-party-source.tar.gz"))
    if release.stable:
        if source not in local:
            gh.fail(f"release handoff is missing the third-party source asset {source}")
        notice = source_notice(release, source)
    else:
        if not rust_sources:
            gh.fail("Rust prerelease handoff is missing its dependency source offer")
        notice = source_notice(release, rust_sources.pop(0))
    if rust_sources:
        notice += "\n\nMatching experimental Rust dependency sources: " + ", ".join(
            f"**{name}**" for name in rust_sources) + "."

    def require_tag() -> None:
        if (sha := converge(f"{tag} visibility", lambda: release_tag_target(repository, tag))) != release.sha:
            gh.fail(f"{tag} resolves to {sha}, expected {release.sha}")

    require_compatible_release_tag(repository, tag, release.sha)
    pages = gh.expect_array(gh.api(f"{base}/releases?per_page=100", paginate=True), "releases")
    matches = [gh.expect_object(item, "release") for page in pages
               for item in gh.expect_array(page, "releases") if isinstance(item, dict)
               and item.get("tag_name") == tag]
    if len(matches) > 1:
        gh.fail(f"multiple releases unexpectedly use tag {tag}")
    if matches and matches[0].get("prerelease") is not prerelease:
        gh.fail(f"{tag} already exists with a different release kind")
    if matches and matches[0].get("draft") is False:
        if asset_digests(repository, gh.int_field(matches[0], "id", "release")) != local:
            gh.fail(f"{tag} is published but asset names/digests differ")
        require_tag()
        if not str(matches[0].get("body") or "").startswith(notice):
            gh.fail(f"{tag} is published but its source-availability notice is missing or stale")
        print(f"::notice::{tag} is already published with the expected source, assets and notice")
        return
    if not matches:
        draft: gh.JsonObject = {"tag_name": tag, "target_commitish": release.sha, "draft": True,
                             "prerelease": prerelease, "generate_release_notes": True, "body": notice}
        matches = [gh.expect_object(gh.api(f"{base}/releases", method="POST", body=draft), "release")]
    release_id = gh.int_field(matches[0], "id", "release")

    current = gh.expect_object(gh.api(f"{base}/releases/{release_id}"), "release")
    if current.get("tag_name") != tag or current.get("draft") is not True:
        gh.fail(f"release {release_id} must remain the {tag} draft during asset upload")
    body = str(current.get("body") or "")
    if not body.startswith(notice):
        if "## Source availability" in body:
            gh.fail(f"{tag} draft contains a stale source-availability notice")
        edit: gh.JsonObject = {"body": f"{notice}\n\n{body}" if body else notice}
        current = gh.expect_object(gh.api(f"{base}/releases/{release_id}", method="PATCH", body=edit),
                                "release")
    upload = gh.str_field(current, "upload_url", "release").split("{", 1)[0]
    if not upload.startswith("https://uploads.github.com/"):
        gh.fail(f"unexpected release upload URL {upload}")
    # A retry starts from an empty draft so it cannot keep stale files.
    for asset in release_assets(repository, release_id):
        gh.api(f"{base}/releases/assets/{gh.int_field(asset, 'id', 'asset')}", method="DELETE")
    for name in sorted(local):
        gh.api(f"{upload}?name={quote(name)}", method="POST", upload=assets / name)
    if asset_digests(repository, release_id) != local:
        gh.fail("draft release asset names/digests do not match verified local files")

    if release_tag_target(repository, tag) is None:
        try:
            gh.api(f"{base}/git/refs", method="POST", body={"ref": f"refs/tags/{tag}", "sha": release.sha})
        except gh.ControlPlaneError as exc:
            if "already exists" not in str(exc):
                gh.fail(f"GitHub rejected creation of {tag} at {release.sha}: {exc}")
            print(f"::warning::{tag} creation raced with another writer; verifying the winner")
    require_tag()
    try:
        gh.api(f"{base}/releases/{release_id}", method="PATCH",
           body={"draft": False, "prerelease": prerelease,
                 "make_latest": "legacy" if release.stable else "false"})
    except gh.ControlPlaneError as exc:
        print(f"::warning::publishing {tag} failed ({exc}); reconciling release state")

    def published() -> gh.JsonObject | None:
        try:
            item = gh.expect_object(gh.api(f"{base}/releases/{release_id}"), "release")
        except gh.ControlPlaneError as exc:
            if "(HTTP 404)" in str(exc):
                return None
            raise
        if item.get("tag_name") != tag or item.get("prerelease") is not prerelease:
            gh.fail(f"release {release_id} is no longer the expected {tag} release")
        return item if item.get("draft") is False else None

    final = converge(f"{tag} publication", published)
    require_tag()
    if not str(final.get("body") or "").startswith(notice):
        gh.fail("published release lost its source-availability notice")
    print(f"::notice::published {tag} with verified SHA-256 assets and source notice")


def rust_request_builds() -> tuple[list[str], list[str]]:
    """The builds of the RUST selection; CI narrows its server platforms with RUST_SERVER."""
    server, tui = verify_release_assets.rust_builds(env("RUST"))
    if narrowed := os.environ.get("RUST_SERVER", "").split():
        if not set(narrowed) <= set(server):
            gh.fail(f"RUST_SERVER must name server platforms that {env('RUST')} builds")
        server = narrowed
    return server, tui


def command_stage_rust() -> None:
    """Stage a request's Docker-built Rust artifacts for upload; the macOS job stages its own."""
    verify_release_assets.stage_rust(gh.runner_path("RUST_EXPORT"), gh.runner_path("RUST_ASSETS"), env("VERSION"),
                                     *rust_request_builds())


def command_check_rust() -> None:
    """Verify staged Rust artifacts of this CI checkout as the release verifies a request's (no macOS part)."""
    server, tui = rust_request_builds()
    staged = gh.runner_path("RUST_ASSETS")
    with tempfile.TemporaryDirectory(dir=staged.parent) as merged:
        verify_release_assets.verify_rust(
            [staged], Path(merged) / "assets", env("VERSION"), server,
            [platform for platform in tui if not platform.startswith("darwin/")], env_sha("GITHUB_SHA"),
            env("GITHUB_REPOSITORY"))
    print(f"Rust release artifacts verified: {sorted(path.name for path in staged.iterdir())}")


COMMANDS = {
    "prepare": command_prepare, "verify": command_verify, "recheck": command_recheck,
    "publish": command_publish, "stage-rust": command_stage_rust, "check-rust": command_check_rust,
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=COMMANDS)
    try:
        COMMANDS[parser.parse_args().command]()
    except (gh.ControlPlaneError, OSError) as exc:
        raise SystemExit(f"Release refused: {exc}") from exc


if __name__ == "__main__":
    main()
