"""Fake boundaries for control-plane tests: GitHub CLI, Git, a Skopeo engine and assets."""

from __future__ import annotations

import hashlib
import io
import json
import os
import sys
import tarfile
import zipfile
from pathlib import Path
from typing import cast

from github_api import APICall, JsonObject, JsonValue
from verify_release_assets import TARGETS, TUI_FILES, tui_archives

AMD, ARM = "sha256:" + "a" * 64, "sha256:" + "b" * 64
INDEX_TYPE = "application/vnd.oci.image.index.v1+json"
MANIFEST_TYPE = "application/vnd.oci.image.manifest.v1+json"
SKOPEO_VERSION = "1.22.3"
# Serves `gh api` from exact argv; a list answers successive calls, repeating its last item.
GH = """import json, os, sys
path = os.environ["FAKE_GH"]
state = json.load(open(path))
call = " ".join(sys.argv[1:])
if call not in state["responses"]:
    sys.exit(f"gh: Not Found (HTTP 404): {call}")
answers = state["responses"][call]
state["calls"].append(call)
json.dump(state, open(path, "w"))
index = state["calls"].count(call) - 1
print(json.dumps(answers[min(index, len(answers) - 1)]))
"""
ENGINE = """#!/bin/sh
printf '%s\\n' "$*" >>"$FAKE_ENGINE_LOG"
case "$*" in
  *" --version") echo "skopeo version $FAKE_SKOPEO_VERSION" ;;
  *" inspect --raw "*) printf '%s\\n' "$FAKE_INDEX" ;;
  *"{{json .Labels}}"*) printf '%s\\n' "$FAKE_LABELS" ;;
  *"{{.Digest}}"*) echo "$FAKE_DIGEST" ;;
  *" copy --all "*) ;;
  *) echo "unexpected engine call: $*" >&2; exit 1 ;;
esac
"""


class Paged(list[object]):
    """A response GitHub must serve through `--paginate --slurp`."""


class Answers(list[object]):
    """Successive responses to one call; the last one repeats."""


def pages(*items: object) -> Paged:
    return Paged(items)


def answers(body: object) -> tuple[bool, list[object]]:
    """Return whether a response is paginated and its successive JSON answers."""
    items = list(body) if isinstance(body, Answers) else [body]
    paged = {isinstance(item, Paged) for item in items}
    if len(paged) != 1:
        raise AssertionError("successive answers must agree on pagination")
    return paged.pop(), [list(item) if isinstance(item, Paged) else item for item in items]


def fake(responses: dict[str, object]) -> APICall:
    """Answer exactly the given API paths, with pagination only where GitHub needs it."""
    served: dict[str, int] = {}

    def api(path: str, *, paginate: bool = False) -> JsonValue:
        if path not in responses:
            raise AssertionError(f"unexpected API call {path}")
        paged, items = answers(responses[path])
        if paginate != paged:
            raise AssertionError(f"{path} must {'' if paged else 'not '}be paginated")
        served[path] = served.get(path, -1) + 1
        return cast(JsonValue, items[min(served[path], len(items) - 1)])

    return api


def gh(directory: Path, responses: dict[str, object]) -> dict[str, str]:
    """Put a fake `gh` on PATH that answers exactly the given API paths."""
    calls = {}
    for path, body in responses.items():
        paged, items = answers(body)
        calls[("api --paginate --slurp " if paged else "api ") + path] = items
    state = directory / "gh.json"
    state.write_text(json.dumps({"responses": calls, "calls": []}))
    script = directory / "bin" / "gh"
    script.parent.mkdir(exist_ok=True)
    script.write_text(f"#!{sys.executable} -IS\n{GH}")
    script.chmod(0o755)
    return {"FAKE_GH": str(state), "GH_TOKEN": "test-token",
            "PATH": f"{script.parent}{os.pathsep}{os.environ['PATH']}"}


def git_head(directory: Path, sha: str) -> dict[str, str]:
    """Point `git rev-parse HEAD` at `sha` through a minimal GIT_DIR."""
    git = directory / "git"
    (git / "objects").mkdir(parents=True)
    (git / "refs").mkdir()
    (git / "HEAD").write_text(sha + "\n")
    return {"GIT_DIR": str(git)}


def index(*manifests: JsonObject) -> JsonObject:
    return {"schemaVersion": 2, "mediaType": INDEX_TYPE, "manifests": list(manifests)}


def descriptor(os_name: str, arch: str, digest: str, attests: str | None = None) -> JsonObject:
    item: JsonObject = {"mediaType": MANIFEST_TYPE, "digest": digest,
                               "platform": {"os": os_name, "architecture": arch}}
    if attests is not None:
        item["annotations"] = {"vnd.docker.reference.type": "attestation-manifest",
                               "vnd.docker.reference.digest": attests}
    return item


RUNNABLE = (descriptor("linux", "amd64", AMD), descriptor("linux", "arm64", ARM))
ATTESTED = (descriptor("unknown", "unknown", "sha256:" + "c" * 64, AMD),
            descriptor("unknown", "unknown", "sha256:" + "d" * 64, ARM))


def engine(directory: Path, repository: str, version: str, revision: str) -> dict[str, str]:
    """Serve a valid two-platform image with provenance from a fake `docker` on PATH."""
    script = directory / "bin" / "docker"
    script.parent.mkdir(exist_ok=True)
    script.write_text(ENGINE)
    script.chmod(0o755)
    labels = {"org.opencontainers.image.source": f"https://github.com/{repository}",
              "org.opencontainers.image.revision": revision,
              "org.opencontainers.image.version": version,
              "org.opencontainers.image.licenses": "AGPL-3.0-or-later"}
    return {
        "CONTAINER_ENGINE": "docker", "FAKE_ENGINE_LOG": str(directory / "engine.log"),
        "PATH": f"{script.parent}{os.pathsep}{os.environ['PATH']}",
        "SKOPEO_IMAGE": "quay.io/containers/skopeo:v1.22.3@sha256:" + "e" * 64,
        "SKOPEO_VERSION": SKOPEO_VERSION, "FAKE_SKOPEO_VERSION": SKOPEO_VERSION,
        "FAKE_INDEX": json.dumps(index(*RUNNABLE, *ATTESTED)), "FAKE_LABELS": json.dumps(labels),
        "FAKE_DIGEST": AMD, "REPOSITORY": repository,
    }


def write_tar(path: Path, members: dict[str, bytes]) -> None:
    with tarfile.open(path, "w:gz") as archive:
        for name, payload in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))


def source_members(version: str) -> dict[str, bytes]:
    root = f"graphite-meter_{version}_third-party-source"
    readme = (b"Use Source code (tar.gz) or Source code (zip). This archive does not "
              b"duplicate Graphite Meter's own repository source.\n")
    return {
        f"{root}/README.txt": readme,
        f"{root}/LEGAL_INVENTORY.json": b'{"server":[],"tui":[],"container":[]}',
        f"{root}/PROVENANCE.json": b"[]",
        f"{root}/third_party/go/quic-go/internal/testdata/priv.key": b"upstream fixture",
        f"{root}/third_party/manual/sample/source.txt": b"manual source",
    }


def write_checksums(dist: Path) -> None:
    lines = [f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
             for path in sorted(dist.iterdir()) if path.name != "checksums.txt"]
    (dist / "checksums.txt").write_text("".join(lines))


def write_release_assets(dist: Path, version: str) -> None:
    """Write the complete native release a stable request uploads."""
    dist.mkdir(parents=True, exist_ok=True)
    write_tar(dist / f"graphite-meter_{version}_third-party-source.tar.gz",
              source_members(version))
    for name, (base, binary) in tui_archives(version, TARGETS).items():
        members = {f"{base}/{file}": b"x" for file in (binary, *TUI_FILES)}
        if name.endswith(".zip"):
            with zipfile.ZipFile(dist / name, "w") as archive:
                for member, payload in members.items():
                    archive.writestr(member, payload)
        else:
            write_tar(dist / name, members)
    write_checksums(dist)
