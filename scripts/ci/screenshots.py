#!/usr/bin/env python3
"""Record and check which release and which sources the README screenshots show (docs/assets/screenshots.json),
and keep the README captions naming that release.

    python3 scripts/ci/screenshots.py stamp TAG
    python3 scripts/ci/screenshots.py check TAG
    python3 scripts/ci/screenshots.py warn
"""

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

from workspace import ROOT, VERSION

RECORD = Path("docs/assets/screenshots.json")
README = Path("README.md")
RENDER = "screenshots"  # the mise task that renders the images; CI only checks them
CAPTION = re.compile(r"(?m)^(<sub>(?:Browser app|Terminal client) )(\S+) ")
# What draws the images: the browser app and its build, the terminal client, the pipeline and its harness.
SOURCES = ("client/src", "client/public", "client/index.html", "client/build.ts", "client/bundle.ts", "client/auth",
           "client/e2e/fleet.ts", "client/e2e/webview.ts", "client/e2e/servers.ts", "client/scripts", "rust/client/src",
           "rust/client/examples/screenshot.rs")


def ui_hash(root: Path = ROOT) -> str:
    """SHA-256 over the path and content of every tracked or new, unignored file under SOURCES."""
    listed = subprocess.check_output(["git", "ls-files", "-z", "-c", "-o", "--exclude-standard", "--", *SOURCES],
                                     cwd=root).decode().split("\0")
    digest = hashlib.sha256()
    for name in sorted(set(listed)):
        if name and (root / name).is_file():
            digest.update(f"{name}\0{hashlib.sha256((root / name).read_bytes()).hexdigest()}\n".encode())
    return digest.hexdigest()


def version_of(tag: str) -> str:
    version = tag.removeprefix("v")
    if VERSION.fullmatch(version) is None:
        raise ValueError(f"{tag!r} is not a release version")
    return version


def stamp(tag: str, root: Path = ROOT) -> None:
    record = {"version": version_of(tag), "ui_hash": ui_hash(root)}
    (root / RECORD).write_text(json.dumps(record, indent=2) + "\n")
    readme = root / README
    readme.write_text(CAPTION.sub(rf"\g<1>{record['version']} ", readme.read_text()))


def stale(tag: str, root: Path = ROOT) -> str | None:
    """Why the screenshots cannot ship with `tag`, or None when they show it and the sources still draw them."""
    try:
        record = json.loads((root / RECORD).read_text())
    except (OSError, ValueError):
        record = {}
    if record != {"version": version_of(tag), "ui_hash": ui_hash(root)}:
        return f"Screenshots are stale: run mise run {RENDER} {tag} and merge it before releasing"
    if captions := {match[2] for match in CAPTION.finditer((root / README).read_text())} - {record["version"]}:
        return f"README captions name {', '.join(sorted(captions))}: run mise run {RENDER} {tag}"
    return None


def drifted(root: Path = ROOT) -> str | None:
    """A CI warning annotation when the UI changed since the screenshots were rendered, else None."""
    try:
        record = json.loads((root / RECORD).read_text())
    except (OSError, ValueError):
        record = {}
    if record.get("ui_hash") == ui_hash(root):
        return None
    return ("::warning title=Screenshots are out of date::The UI changed since the README screenshots were rendered; "
            f"run mise run {RENDER} before the next release")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=("stamp", "check", "warn"))
    parser.add_argument("tag", nargs="?")
    args = parser.parse_args()
    if args.command == "warn":
        if annotation := drifted():
            print(annotation)
        return
    if args.tag is None:
        parser.error(f"{args.command} needs a TAG")
    try:
        if args.command == "stamp":
            stamp(args.tag)
        elif message := stale(args.tag):
            sys.exit(message)
    except ValueError as error:
        sys.exit(str(error))


if __name__ == "__main__":
    main()
