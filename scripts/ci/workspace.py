#!/usr/bin/env python3
"""Read the Rust workspace members, shipped targets and platform record from rust/Cargo.toml, and name release files.

    python3 scripts/ci/workspace.py targets {tui,server}
    python3 scripts/ci/workspace.py target {tui,server} PLATFORM
"""

import argparse
import re
import tomllib
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MEMBER = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*")
PLATFORM = re.compile(r"[a-z0-9]+/[a-z0-9]+")
TARGET = re.compile(r"[a-z0-9_]+(?:-[a-z0-9_]+){2,3}")
RECORD = re.compile(r"legal/[\w.-]+\.json")
N = r"(?:0|[1-9][0-9]*)"
# Any build's version, such as a local build's short commit; it names files.
VERSION = re.compile(r"[A-Za-z0-9][A-Za-z0-9.+-]*")
STABLE = re.compile(rf"{N}\.{N}\.{N}")
# Publishes only its image, whose SOURCE.txt names the repository.
PRERELEASE = re.compile(rf"{N}\.{N}\.{N}-(?:alpha|beta|rc)\.{N}")
KINDS = ("tui", "server")


@dataclass(frozen=True)
class Workspace:
    members: tuple[str, ...]  # directories under rust/
    tui: dict[str, str]  # platform -> Rust target
    server: dict[str, str]  # image platform -> Rust target
    platform_record: Path  # relative to the repository root


def load(root: Path = ROOT) -> Workspace:
    rust = root / "rust"
    workspace = tomllib.loads((rust / "Cargo.toml").read_text())["workspace"]
    config = workspace["metadata"]["graphite-meter"]
    members = tuple(workspace["members"])
    if invalid := [member for member in members if not isinstance(member, str) or MEMBER.fullmatch(member) is None]:
        raise ValueError(f"rust/Cargo.toml members {invalid!r} must be plain directory names")

    def shipped(kind: str) -> dict[str, str]:
        targets = {platform: config["targets"].get(platform) for platform in config[kind]}
        valid = (PLATFORM.fullmatch(platform) and isinstance(target, str) and TARGET.fullmatch(target)
                 for platform, target in targets.items())
        if not targets or not all(valid):
            raise ValueError(f"rust/Cargo.toml {kind} must list platforms that each name a Rust target")
        return targets

    record = config["platform-record"]
    if not isinstance(record, str) or RECORD.fullmatch(record) is None:
        raise ValueError("rust/Cargo.toml platform-record must name a JSON file in legal/")
    return Workspace(members, shipped("tui"), shipped("server"), Path(record))


def build_name(package: str, platform: str) -> str:
    """One shipped build, such as `graphite-meter-client_linux_amd64`."""
    return f"{package}_{platform.replace('/', '_')}"


def release_archive(package: str, version: str, platform: str) -> tuple[str, str, str]:
    """The release archive of `package` for `platform`, its root directory and its executable."""
    base = f"{package}_{version}_{platform.replace('/', '_')}"
    if platform.startswith("windows/"):
        return f"{base}.zip", base, f"{package}.exe"
    return f"{base}.tar.gz", base, package


def offer_name(version: str) -> str:
    """The release's one source offer; its files lie in a directory named like it without `.tar.gz`."""
    return f"graphite-meter_{version}_third-party-source.tar.gz"


def build_offer(package: str, platform: str) -> str:
    """The part of the source offer one build writes, which release staging merges."""
    return f"{build_name(package, platform)}_third-party-source.tar.gz"


def source_url(repository: str, version: str) -> str:
    """The source a build of `version` names: a stable release's tag, otherwise the repository."""
    return f"{repository}/tree/v{version}" if STABLE.fullmatch(version) else repository


def source_txt(repository: str, version: str, build: str, target: str) -> str:
    """SOURCE.txt of a reviewed build, in its archive and, for the server, its image."""
    if PRERELEASE.fullmatch(version):
        return f"{repository}\n"
    return (f"Graphite Meter source: {source_url(repository, version)}\nMatching release: v{version}\n"
            f"Dependency source archive: {offer_name(version)}\nBuild: {build} ({target})\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("targets", help="every Rust target of a kind").add_argument("kind", choices=KINDS)
    one = commands.add_parser("target", help="the Rust target of one platform")
    one.add_argument("kind", choices=KINDS)
    one.add_argument("platform")
    args = parser.parse_args()
    try:
        workspace = load()
    except (ValueError, KeyError, OSError) as exc:
        parser.exit(1, f"workspace: {exc}\n")
    targets = {"tui": workspace.tui, "server": workspace.server}[args.kind]
    if args.command == "targets":
        print("\n".join(targets.values()))
    elif args.platform in targets:
        print(targets[args.platform])
    else:
        parser.exit(1, f"workspace: no {args.kind} platform {args.platform!r}\n")


if __name__ == "__main__":
    main()
