#!/usr/bin/env python3
"""Read repository tool pins and validate literals required before code can run."""
from __future__ import annotations

import argparse
import platform
import re
import subprocess
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PIN_PATTERNS = {
    "tools": {
        "just": r"\d+\.\d+\.\d+",
        "gitleaks": r"v\d+\.\d+\.\d+",
        "staticcheck": r"\d{4}\.\d+\.\d+",
        "govulncheck": r"v\d+\.\d+\.\d+",
        "ty": r"\d+\.\d+\.\d+",
    },
    "browser": {"chrome": r"\d+\.\d+\.\d+\.\d+"},
    "images": {
        "gitleaks": r"ghcr\.io/gitleaks/gitleaks@sha256:[0-9a-f]{64}",
        "skopeo": r"quay\.io/containers/skopeo:v\d+\.\d+\.\d+-immutable@sha256:[0-9a-f]{64}",
        "binfmt": r"docker\.io/tonistiigi/binfmt@sha256:[0-9a-f]{64}",
    },
}


def load_pins(root: Path = ROOT) -> dict[str, dict[str, str]]:
    data = tomllib.loads((root / "tools.toml").read_text(encoding="utf-8"))
    if data.keys() != PIN_PATTERNS.keys():
        raise ValueError("tools.toml must contain exactly tools, browser and images")
    pins: dict[str, dict[str, str]] = {}
    for section, patterns in PIN_PATTERNS.items():
        values = data[section]
        if not isinstance(values, dict) or values.keys() != patterns.keys():
            raise ValueError(f"tools.toml [{section}] must contain exactly {', '.join(patterns)}")
        pins[section] = {}
        for name, pattern in patterns.items():
            value = values[name]
            if not isinstance(value, str) or re.fullmatch(pattern, value) is None:
                raise ValueError(f"tools.toml {section}.{name} must be an exact version or image digest")
            pins[section][name] = value
    return pins


def pin(name: str, root: Path = ROOT) -> str:
    section, key = name.split(".", 1)
    return load_pins(root)[section][key]


def skopeo_version(root: Path = ROOT) -> str:
    return pin("images.skopeo", root).split(":v", 1)[1].split("-immutable@", 1)[0]


def runtime_pins(root: Path = ROOT) -> dict[str, str]:
    versions = {name: (root / f".{name}-version").read_text().strip() for name in ("bun", "python")}
    go = re.search(r"(?m)^go (\S+)$", (root / "go/go.mod").read_text())
    if go is None:
        raise ValueError("go/go.mod must select an exact Go version")
    versions["go"] = go[1]
    for name, version in versions.items():
        if re.fullmatch(r"\d+\.\d+\.\d+", version) is None:
            raise ValueError(f"{name} must select an exact major.minor.patch runtime")
    return versions


def literal_updates(root: Path = ROOT) -> dict[Path, str]:
    """Return expected source for unavoidable pre-execution literals; never execute pin data."""
    pins, runtimes = load_pins(root), runtime_pins(root)
    replacements = {
        "container/Dockerfile": [
            (r"(?m)^ARG BUN_VERSION=\S+$", f"ARG BUN_VERSION={runtimes['bun']}"),
            (r"(?m)^FROM docker.io/library/golang:\S+ AS server$", f"FROM docker.io/library/golang:{runtimes['go']} AS server"),
        ],
        ".github/actions/build-oci/action.yml": [
            (r"(?m)^(\s*image: )docker.io/tonistiigi/binfmt@\S+$", rf"\g<1>{pins['images']['binfmt']}"),
        ],
    }
    for name in ("ci", "release", "prerelease-publish", "_publish-oci", "_promote-oci"):
        replacements[f".github/workflows/{name}.yml"] = [
            (r"(?m)^(\s*SKOPEO_IMAGE: )quay.io/containers/skopeo:\S+$", rf"\g<1>{pins['images']['skopeo']}"),
        ]
        if name in ("ci", "release", "prerelease-publish"):
            version = skopeo_version(root)
            replacements[f".github/workflows/{name}.yml"].append(
                (r"(?m)^(\s*SKOPEO_VERSION: )\d+\.\d+\.\d+$", rf"\g<1>{version}")
            )
    updates: dict[Path, str] = {}
    for name, patterns in replacements.items():
        path = root / name
        original = path.read_text(encoding="utf-8")
        expected = original
        for pattern, replacement in patterns:
            expected, count = re.subn(pattern, replacement, expected)
            if count != 1:
                raise ValueError(f"{name}: expected one toolchain literal for {pattern}")
        if original != expected:
            updates[path] = expected
    return updates


def check(root: Path = ROOT) -> None:
    updates = literal_updates(root)
    if updates:
        paths = ", ".join(str(path.relative_to(root)) for path in updates)
        raise ValueError(f"toolchain literals drift in {paths}; run just toolchain-sync")


def doctor(root: Path = ROOT) -> None:
    check(root)
    expected = runtime_pins(root) | {"just": pin("tools.just", root)}
    commands = {
        "bun": (["bun", "--version"], "", root),
        "go": (["go", "env", "GOVERSION"], "go", root / "go"),
        "just": (["just", "--version"], "just ", root),
    }
    actual = {"python": platform.python_version()}
    for name, (command, prefix, directory) in commands.items():
        result = subprocess.run(command, cwd=directory, capture_output=True, text=True, check=True)
        actual[name] = result.stdout.strip().removeprefix(prefix)
    actual["go"] = actual["go"].split("-", 1)[0]
    mismatch = []
    for name, version in expected.items():
        print(f"{name}: {actual[name]} (expected {version})")
        if actual[name] != version:
            mismatch.append(name)
    if mismatch:
        raise ValueError(f"toolchain mismatch: {', '.join(mismatch)}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("get", "check", "sync", "doctor", "python-target"))
    parser.add_argument("name", nargs="?")
    args = parser.parse_args()
    try:
        match args.command:
            case "get":
                if not args.name:
                    parser.error("get requires a section.name")
                print(pin(args.name))
            case "python-target":
                print(".".join(runtime_pins()["python"].split(".")[:2]))
            case "check":
                check()
            case "sync":
                for path, content in literal_updates().items():
                    path.write_text(content, encoding="utf-8")
                    print(path.relative_to(ROOT))
            case "doctor":
                doctor()
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as exc:
        parser.exit(1, f"toolchains: {exc}\n")


if __name__ == "__main__":
    main()
