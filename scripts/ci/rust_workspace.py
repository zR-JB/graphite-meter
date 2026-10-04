#!/usr/bin/env python3
"""Read the Rust workspace members, shipped targets and platform record from rust/Cargo.toml."""

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


@dataclass(frozen=True)
class Workspace:
    members: dict[str, str]  # directory under rust/ -> package name
    tui: dict[str, str]  # platform -> Rust target
    server: dict[str, str]  # image platform -> Rust target
    platform_record: Path  # relative to the repository root


def load(root: Path = ROOT) -> Workspace:
    rust = root / "rust"
    workspace = tomllib.loads((rust / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
    config = workspace["metadata"]["graphite-meter"]
    members: dict[str, str] = {}
    for member in workspace["members"]:
        if not isinstance(member, str) or MEMBER.fullmatch(member) is None:
            raise ValueError(f"rust/Cargo.toml member {member!r} must be a plain directory name")
        manifest = tomllib.loads((rust / member / "Cargo.toml").read_text(encoding="utf-8"))
        members[member] = manifest["package"]["name"]

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


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("targets",))
    parser.add_argument("kind", choices=("tui", "server"))
    args = parser.parse_args()
    try:
        workspace = load()
    except (ValueError, KeyError, OSError) as exc:
        parser.exit(1, f"rust_workspace: {exc}\n")
    print("\n".join({"tui": workspace.tui, "server": workspace.server}[args.kind].values()))


if __name__ == "__main__":
    main()
