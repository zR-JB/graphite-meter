#!/usr/bin/env python3
"""Read the Rust workspace members, shipped targets and platform record from rust/Cargo.toml.

    python3 scripts/ci/rust_workspace.py targets {tui,server}
    python3 scripts/ci/rust_workspace.py target {tui,server} PLATFORM

It also names the Rust release files, which tooling on both sides of a release reads.
"""

from __future__ import annotations

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
KINDS = ("tui", "server")


@dataclass(frozen=True)
class Workspace:
    members: tuple[str, ...]  # directories under rust/
    tui: dict[str, str]  # platform -> Rust target
    server: dict[str, str]  # image platform -> Rust target
    platform_record: Path  # relative to the repository root


def load(root: Path = ROOT) -> Workspace:
    rust = root / "rust"
    workspace = tomllib.loads((rust / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
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


def release_name(package: str, version: str, platform: str) -> str:
    """The stem of a Rust release file of `package` for `platform` (GOOS/GOARCH): Go's name with a `_rust` marker."""
    return f"{package}_{version}_{platform.replace('/', '_')}_rust"


def offer_name(package: str, version: str, platform: str) -> str:
    """The source offer of that build; its files lie in a directory named like it without `.tar.gz`, as Go's."""
    return f"{release_name(package, version, platform)}_third-party-source.tar.gz"


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
        parser.exit(1, f"rust_workspace: {exc}\n")
    targets = {"tui": workspace.tui, "server": workspace.server}[args.kind]
    if args.command == "targets":
        print("\n".join(targets.values()))
    elif args.platform in targets:
        print(targets[args.platform])
    else:
        parser.exit(1, f"rust_workspace: no {args.kind} platform {args.platform!r}\n")


if __name__ == "__main__":
    main()
