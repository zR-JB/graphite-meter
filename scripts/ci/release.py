#!/usr/bin/env python3
"""Validate release requests, then authorize stable releases and main or PR prereleases."""

import argparse
import hashlib
import json
import os
import shutil
import sys
import time
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import quote

import assets
import github_api as gh
import screenshots
from trust import (
    SHA_RE,
    env,
    env_int,
    env_sha,
    exact_files,
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
from workspace import PRERELEASE, STABLE, load, offer_name

REQUEST_LIMIT = 2 * assets.OCI_LIMIT
# The release request's jobs; the last writes its one artifact.
VALIDATE_JOB, ASSEMBLE_JOB = "Validate request", "Assemble request"
# Seconds between reads while GitHub's read path catches up with a write.
DELAYS = (0.25, 0.5, 1, 2, 4, 8)
REQUEST_KEYS = {"schemaVersion", "repository", "tag", "sourceSha", "pr", "mode", "requestRunId", "requestRunAttempt"}


@dataclass(frozen=True)
class Release:
    tag: str
    sha: str
    pr: int

    @property
    def stable(self) -> bool:
        return STABLE.fullmatch(self.version) is not None

    @property
    def version(self) -> str:
        return self.tag[1:]


def parse_release(tag: str, sha: str, pr: int) -> Release:
    version = tag.removeprefix("v")
    stable = STABLE.fullmatch(version) is not None
    if pr < 0 or not tag.startswith("v") or not (stable or PRERELEASE.fullmatch(version)) or (stable and pr):
        gh.fail("stable tags are vMAJOR.MINOR.PATCH; prereleases, of main or a PR, add -{alpha,beta,rc}.N")
    if SHA_RE.fullmatch(sha) is None:
        gh.fail("release source must be a 40-character commit SHA")
    return Release(tag, sha, pr)


def request_title(mode: str, release: Release) -> str:
    """The run-name that release-request.yml derives from its dispatch inputs."""
    return f"{release.tag} · {mode} · {f'PR #{release.pr}@{release.sha}' if release.pr else 'main'}"


def image_job(platform: str) -> str:
    return f"Build server image ({platform})"


def client_job(platform: str) -> str:
    return f"Build client ({platform})"


def request_jobs() -> set[str]:
    """Every job of a request run; a prerelease's client jobs skip their builds."""
    workspace = load()
    return {VALIDATE_JOB, ASSEMBLE_JOB, *map(image_job, workspace.server), *map(client_job, workspace.tui)}


def built_from(release: Release) -> str:
    return f"`{release.sha}` of PR #{release.pr}" if release.pr else f"`{release.sha}` on main"


def checks(ci_run_id: int, codeql_id: str) -> str:
    return f"CI Gate in run {ci_run_id}, CodeQL {f'check {codeql_id}' if codeql_id else 'on main'}"


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


def converge[T](what: str, probe: Callable[[], T | None]) -> T:
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
    if not release.pr:
        main = require_exact_current_main(repository, sha)
        if release.stable:
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


BREAKING = "breaking change"


def breaking_since(repository: str, sha: str) -> tuple[str, list[int]]:
    """The latest release's version and the PRs labelled BREAKING merged after it, up to `sha`."""
    latest = gh.str_field(gh.expect_object(gh.api(f"repos/{repository}/releases/latest"), "latest release"),
                          "tag_name", "latest release")
    if STABLE.fullmatch(latest.removeprefix("v")) is None or not latest.startswith("v"):
        gh.fail(f"the latest release {latest} is not a vMAJOR.MINOR.PATCH tag")
    pages = gh.expect_array(gh.api(gh.query(f"repos/{repository}/compare/{latest}...{sha}", per_page=100),
                                   paginate=True), "comparison pages")
    commits = [commit for page in pages
               for commit in gh.expect_array(gh.expect_object(page, "comparison").get("commits"), "comparison commits")]
    breaking = set()
    for commit in commits:
        commit_sha = gh.str_field(gh.expect_object(commit, "commit"), "sha", "commit")
        for value in gh.expect_array(gh.api(f"repos/{repository}/commits/{commit_sha}/pulls"), "commit pulls"):
            pull = gh.expect_object(value, "pull")
            labels = gh.expect_array(pull.get("labels"), "pull labels")
            if any(gh.str_field(gh.expect_object(label, "label"), "name", "label") == BREAKING for label in labels):
                breaking.add(gh.int_field(pull, "number", "pull"))
    return latest.removeprefix("v"), sorted(breaking)


def require_breaking_bump(previous: str, version: str, breaking: list[int]) -> None:
    """After a breaking change, a 0.x release raises the minor version and a later one the major."""
    old, new = (tuple(int(part) for part in value.split(".")) for value in (previous, version))
    needed = (0, old[1] + 1, 0) if old[0] == 0 else (old[0] + 1, 0, 0)
    if breaking and new < needed:
        changes = ", ".join(f"#{number}" for number in breaking)
        gh.fail(f"{changes} labelled '{BREAKING}' since v{previous}; tag v{'.'.join(map(str, needed))} or later")


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
    sha = env_sha("SHA") if os.environ.get("SHA") else ""
    release = parse_release(env("TAG"), sha or main, pr)
    if release.stable and sha:
        gh.fail("stable releases build current main; leave sha empty")
    if not release.stable and not sha:
        gh.fail("prereleases name the exact commit they build in sha")
    if not pr and sha and sha != main:
        gh.fail(f"a prerelease without a PR builds main, which is {main}; sha must match")
    if release.stable:
        previous, breaking = breaking_since(repository, main)
        require_breaking_bump(previous, release.version, breaking)
    if release.stable and (stale := screenshots.stale(release.tag)):
        gh.fail(stale)
    if pr:
        require_pr(repository, pr, sha)
    out = gh.runner_path("OUT_DIR")
    out.mkdir(parents=True, exist_ok=True)
    request = {
        "schemaVersion": 4, "repository": repository, "tag": release.tag, "sourceSha": release.sha, "pr": pr,
        "mode": mode, "requestRunId": env_int("REQUEST_RUN_ID"), "requestRunAttempt": 1,
    }
    (out / "request.json").write_text(json.dumps(request, indent=2, sort_keys=True) + "\n")
    gh.append_summary(gh.table([("Tag", f"`{release.tag}`"), ("Mode", mode), ("Commit", built_from(release))]))
    gh.append_output(
        tag=release.tag, version=release.version, sha=release.sha,
        stable=str(release.stable).lower(), remote_sha=release.sha if release.pr else "",
        client_validate="0" if release.stable else "1",
    )


def verify_request(request_dir: Path) -> tuple[Release, bool]:
    """Bind the untrusted request artifact to its run on the publishing main commit and to the release rules."""
    repository = env("REPOSITORY")
    publisher = env_sha("PUBLISHER_SHA")
    run_id = env_int("REQUEST_RUN_ID")
    if env("WORKFLOW_REF") != main_workflow(repository, "release.yml"):
        gh.fail("release consumer is not the trusted main workflow")
    require_checkout(publisher)
    candidate = request_dir / f"release-request-{run_id}"
    if (downloaded := {path.name for path in request_dir.iterdir()}) != {candidate.name}:
        gh.fail(f"downloaded artifacts are {sorted(downloaded)}; expected {candidate.name}")
    path = candidate / "request.json"
    request = gh.expect_object(gh.decode_json(path.read_text(), path.name), path.name)
    if set(request) != REQUEST_KEYS:
        gh.fail(f"request.json keys are {sorted(request)}; expected {sorted(REQUEST_KEYS)}")
    trusted: gh.JsonObject = {"schemaVersion": 4, "repository": repository, "requestRunId": run_id,
                              "requestRunAttempt": 1}
    for key, value in trusted.items():
        if type(request[key]) is not type(value) or request[key] != value:
            gh.fail(f"request.json {key} does not match its trusted request run")
    release = parse_release(gh.str_field(request, "tag", "request"),
                            gh.str_field(request, "sourceSha", "request"),
                            gh.int_field(request, "pr", "request"))
    if request["mode"] not in ("validate", "publish"):
        gh.fail("request mode must be validate or publish")
    if not release.pr and release.sha != publisher:
        gh.fail("a release without a PR must build the trusted main commit")
    exact_files(candidate, {"request.json", *assets.staged_files(release.version)})
    require_dispatch_run(repository, env("REPOSITORY_OWNER"), publisher, run_id, "release-request.yml",
                         request_title(str(request["mode"]), release), request_jobs(), ASSEMBLE_JOB,
                         candidate.name, REQUEST_LIMIT)
    return release, request["mode"] == "publish"


def published_assets(repository: str, release_id: int) -> list[gh.JsonObject]:
    pages = gh.api(f"repos/{repository}/releases/{release_id}/assets?per_page=100", paginate=True)
    return [gh.expect_object(item, "asset") for page in gh.expect_array(pages, "assets")
            for item in gh.expect_array(page, "assets")]


def asset_digests(repository: str, release_id: int) -> dict[str, str]:
    return {gh.str_field(asset, "name", "asset"): str(asset.get("digest") or "")
            for asset in published_assets(repository, release_id)}


def local_digests(directory: Path) -> dict[str, str]:
    """Each file's digest as GitHub reports a release asset's."""
    exact_files(directory, names := {entry.name for entry in directory.iterdir()})
    return {name: "sha256:" + gh.file_sha256(directory / name) for name in names}


def assets_sha256(directory: Path) -> str:
    """One digest of every file's name and digest in `directory`."""
    return hashlib.sha256(json.dumps(local_digests(directory), sort_keys=True).encode()).hexdigest()


def tag_release(repository: str, tag: str) -> gh.JsonObject | None:
    """The release at `tag`, if there is one."""
    pages = gh.expect_array(gh.api(f"repos/{repository}/releases?per_page=100", paginate=True), "releases")
    matches = [gh.expect_object(item, "release") for page in pages
               for item in gh.expect_array(page, "releases") if isinstance(item, dict) and item.get("tag_name") == tag]
    if len(matches) > 1:
        gh.fail(f"multiple releases unexpectedly use tag {tag}")
    return matches[0] if matches else None


def require_same_published_assets(repository: str, release: Release, files: Path) -> None:
    """A stable release already published at its tag holds exactly `files`: the image goes out before the Release,
    and must not name a source offer it would never carry."""
    existing = tag_release(repository, release.tag)
    if existing is not None and existing.get("draft") is False and asset_digests(
            repository, gh.int_field(existing, "id", "release")) != local_digests(files):
        gh.fail(f"{release.tag} is already published with other assets")


def write_checksums(directory: Path) -> None:
    """Write checksums.txt, as sha256sum lists them, for every other file in `directory`."""
    names = sorted(path.name for path in directory.iterdir() if path.name != "checksums.txt")
    listing = "".join(f"{gh.file_sha256(directory / name)}  {name}\n" for name in names)
    (directory / "checksums.txt").write_text(listing)


def command_verify() -> None:
    request_dir, handoff = gh.runner_path("REQUEST_DIR"), gh.runner_path("HANDOFF_DIR")
    release, publish = verify_request(request_dir)
    if publish:
        require_protected_environment(env("REPOSITORY"))
    candidate = request_dir / f"release-request-{env_int('REQUEST_RUN_ID')}"
    digest, manifest = assets.verify(release.version, release.sha, candidate)
    # The Release's assets under one checksums.txt.
    release_files = request_dir / "release-files"
    if release.stable:
        release_files.mkdir()
        for name in sorted(assets.release_assets(release.version)):
            shutil.copyfile(candidate / name, release_files / name)
        write_checksums(release_files)
    main, ci_run_id, codeql_id = require_publishable(env("REPOSITORY"), release)
    if main != env("PUBLISHER_SHA"):
        gh.fail("main moved during verification; start a fresh request")
    if release.stable:
        require_same_published_assets(env("REPOSITORY"), release, release_files)

    (handoff / "image").mkdir(parents=True, exist_ok=True)
    shutil.copyfile(candidate / assets.OCI, handoff / "image" / assets.OCI)
    if release.stable:
        shutil.copytree(release_files, handoff / "assets")
    gh.append_output(
        tag=release.tag, version=release.version, stable=str(release.stable).lower(),
        publish=str(publish).lower(), sha=release.sha, main_sha=main, pr=release.pr or "",
        oci_sha256=digest, digest=manifest,
        assets_sha256=assets_sha256(handoff / "assets") if release.stable else "",
    )
    gh.append_summary(gh.table([
        ("Tag", f"`{release.tag}`"),
        ("Mode", "publish, after `ghcr-release` approval" if publish else "validate, publishes nothing"),
        ("Commit", built_from(release)), ("Main", f"`{main}`"), ("Image", f"`{manifest}`"),
        ("Checks", f"request run {env_int('REQUEST_RUN_ID')}, files, image, {checks(ci_run_id, codeql_id)}"),
    ]))
    assets.summarize(candidate, release.version)


def command_recheck() -> None:
    main = env_sha("MAIN_SHA")
    pr = env_int("PR") if os.environ.get("PR") else 0
    release = parse_release(env("TAG"), env_sha("SOURCE_SHA"), pr)
    require_checkout(main)
    handoff = gh.runner_path("HANDOFF_DIR")
    exact_files(handoff / "image", {assets.OCI})
    if gh.file_sha256(handoff / "image" / assets.OCI) != env("OCI_SHA256"):
        gh.fail("approved OCI handoff does not match the verified archive")
    if release.stable and assets_sha256(handoff / "assets") != env("ASSETS_SHA256"):
        gh.fail("approved asset handoff does not match the verified assets")
    if release.stable:
        require_same_published_assets(env("REPOSITORY"), release, handoff / "assets")
    current, ci_run_id, codeql_id = require_publishable(env("REPOSITORY"), release)
    if current != main:
        gh.fail("main moved after verification; start a fresh request")
    gh.append_summary(gh.table([
        ("Tag", f"`{release.tag}`"), ("Commit", built_from(release)), ("Main", f"`{main}`"),
        ("Checks", f"handoff digests, {checks(ci_run_id, codeql_id)}"),
    ]))


def source_notice(release: Release) -> str:
    """The release body's source notice: the tagged repository and the third-party source offer."""
    return (
        f"## Source\n\nThe source for this release is the repository at tag **{release.tag}** (commit "
        f"**{release.sha}**), attached by GitHub as Source code (zip/tar.gz). The source of the third-party "
        "components in the image and the server and client archives is attached as "
        f"**{offer_name(release.version)}**."
    )


def command_publish() -> None:
    """Publish the verified assets as the stable GitHub Release at its exact tag, idempotently."""
    repository = env("REPOSITORY")
    release = parse_release(env("TAG"), env_sha("TARGET_SHA"), 0)
    tag, base = release.tag, f"repos/{repository}"
    files = gh.runner_path("ASSETS_DIR")
    local = local_digests(files)
    if (expected := {*assets.release_assets(release.version), "checksums.txt"}) != set(local):
        gh.fail(f"release handoff holds {sorted(local)}; expected {sorted(expected)}")
    notice = source_notice(release)

    def require_tag() -> None:
        if (sha := converge(f"{tag} visibility", lambda: release_tag_target(repository, tag))) != release.sha:
            gh.fail(f"{tag} resolves to {sha}, expected {release.sha}")

    require_compatible_release_tag(repository, tag, release.sha)
    existing = tag_release(repository, tag)
    if existing is not None and existing.get("prerelease") is not False:
        gh.fail(f"{tag} already exists as a prerelease")
    if existing is not None and existing.get("draft") is False:
        if asset_digests(repository, gh.int_field(existing, "id", "release")) != local:
            gh.fail(f"{tag} is published but asset names/digests differ")
        require_tag()
        if not str(existing.get("body") or "").startswith(notice):
            gh.fail(f"{tag} is published but its source notice is missing or stale")
        print(f"::notice::{tag} is already published with the expected source, assets and notice")
        return
    if existing is None:
        draft: gh.JsonObject = {"tag_name": tag, "target_commitish": release.sha, "draft": True,
                                "prerelease": False, "generate_release_notes": True, "body": notice}
        existing = gh.expect_object(gh.api(f"{base}/releases", method="POST", body=draft), "release")
    release_id = gh.int_field(existing, "id", "release")

    current = gh.expect_object(gh.api(f"{base}/releases/{release_id}"), "release")
    if current.get("tag_name") != tag or current.get("draft") is not True:
        gh.fail(f"release {release_id} must remain the {tag} draft during asset upload")
    body = str(current.get("body") or "")
    if not body.startswith(notice):
        if "## Source" in body:
            gh.fail(f"{tag} draft contains a stale source notice")
        edit: gh.JsonObject = {"body": f"{notice}\n\n{body}" if body else notice}
        current = gh.expect_object(gh.api(f"{base}/releases/{release_id}", method="PATCH", body=edit),
                                "release")
    upload = gh.str_field(current, "upload_url", "release").split("{", 1)[0]
    if not upload.startswith("https://uploads.github.com/"):
        gh.fail(f"unexpected release upload URL {upload}")
    # A retry starts from an empty draft so it cannot keep stale files.
    for asset in published_assets(repository, release_id):
        gh.api(f"{base}/releases/assets/{gh.int_field(asset, 'id', 'asset')}", method="DELETE")
    for name in sorted(local):
        gh.api(f"{upload}?name={quote(name)}", method="POST", upload=files / name)
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
               body={"draft": False, "prerelease": False, "make_latest": "legacy"})
    except gh.ControlPlaneError as exc:
        print(f"::warning::publishing {tag} failed ({exc}); reconciling release state")

    def published() -> gh.JsonObject | None:
        try:
            item = gh.expect_object(gh.api(f"{base}/releases/{release_id}"), "release")
        except gh.ControlPlaneError as exc:
            if "(HTTP 404)" in str(exc):
                return None
            raise
        if item.get("tag_name") != tag or item.get("prerelease") is not False:
            gh.fail(f"release {release_id} is no longer the stable {tag} release")
        return item if item.get("draft") is False else None

    final = converge(f"{tag} publication", published)
    require_tag()
    if not str(final.get("body") or "").startswith(notice):
        gh.fail("published release lost its source notice")
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
    except (gh.ControlPlaneError, OSError) as exc:
        raise SystemExit(f"Release refused: {exc}") from exc


if __name__ == "__main__":
    main()
