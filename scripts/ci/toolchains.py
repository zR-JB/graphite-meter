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
TOOL_KEYS = {
    "bun": "bun", "python": "python", "go": "go",
    "gitleaks": "aqua:gitleaks/gitleaks",
    "staticcheck": "aqua:dominikh/go-tools/staticcheck",
    "govulncheck": "go:golang.org/x/vuln/cmd/govulncheck",
    "ty": "aqua:astral-sh/ty",
}
PIN_PATTERNS = {
    "browser": {"chrome": r"\d+\.\d+\.\d+\.\d+"},
    "images": {
        "gitleaks": r"ghcr\.io/gitleaks/gitleaks@sha256:[0-9a-f]{64}",
        "skopeo": r"quay\.io/containers/skopeo:v\d+\.\d+\.\d+-immutable@sha256:[0-9a-f]{64}",
        "binfmt": r"docker\.io/tonistiigi/binfmt@sha256:[0-9a-f]{64}",
    },
}


def load_pins(root: Path = ROOT) -> dict[str, dict[str, str]]:
    data = tomllib.loads((root / "mise.toml").read_text(encoding="utf-8"))
    tools = data.get("tools", {})
    if not isinstance(tools, dict) or tools.keys() != set(TOOL_KEYS.values()):
        raise ValueError("mise.toml [tools] must contain exactly the reviewed runtime and checker backends")
    pins: dict[str, dict[str, str]] = {"tools": {}}
    for name, key in TOOL_KEYS.items():
        value = tools[key]
        if not isinstance(value, str) or re.fullmatch(r"\d+\.\d+\.\d+", value) is None:
            raise ValueError(f"mise.toml tools.{name} must select an exact major.minor.patch version")
        pins["tools"][name] = value
    metadata = data.get("vars", {})
    mise_version = metadata.get("mise_version")
    if not isinstance(mise_version, str) or re.fullmatch(r"\d{4}\.\d+\.\d+", mise_version) is None:
        raise ValueError("mise.toml vars.mise_version must select an exact release")
    pins["tools"]["mise"] = mise_version
    for section, patterns in PIN_PATTERNS.items():
        prefix = "browser" if section == "browser" else "image"
        values = {name: metadata.get(f"{prefix}_{name}") for name in patterns}
        pins[section] = {}
        for name, pattern in patterns.items():
            value = values[name]
            if not isinstance(value, str) or re.fullmatch(pattern, value) is None:
                raise ValueError(f"mise.toml {section}.{name} must be an exact version or image digest")
            pins[section][name] = value
    pins["runtime"] = {name: pins["tools"][name] for name in ("bun", "python", "go")}
    return pins


def pin(name: str, root: Path = ROOT) -> str:
    section, key = name.split(".", 1)
    return load_pins(root)[section][key]


def skopeo_version(root: Path = ROOT) -> str:
    return pin("images.skopeo", root).split(":v", 1)[1].split("-immutable@", 1)[0]


def runtime_pins(root: Path = ROOT) -> dict[str, str]:
    return load_pins(root)["runtime"]


def literal_updates(root: Path = ROOT) -> dict[Path, str]:
    """Return expected source for unavoidable pre-execution literals; never execute pin data."""
    pins, runtimes = load_pins(root), runtime_pins(root)
    replacements = {
        "go/go.mod": [(r"(?m)^go \S+$", f"go {runtimes['go']}")],
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
    # Mise must bootstrap before Python can read project metadata. Keep this
    # unavoidable literal checked and synced in every direct action invocation.
    for path in (root / ".github").rglob("*.yml"):
        content = path.read_text(encoding="utf-8")
        if "uses: jdx/mise-action@" in content:
            name = str(path.relative_to(root))
            replacements.setdefault(name, []).append((
                r"(?m)^(\s*version: )\d{4}\.\d+\.\d+$",
                rf"\g<1>{pins['tools']['mise']}",
            ))
    updates: dict[Path, str] = {}
    for name, patterns in replacements.items():
        path = root / name
        original = path.read_text(encoding="utf-8")
        expected = original
        for pattern, replacement in patterns:
            expected, count = re.subn(pattern, replacement, expected)
            required_count = original.count("uses: jdx/mise-action@") if "version: " in pattern else 1
            if count != required_count:
                raise ValueError(f"{name}: expected {required_count} toolchain literal(s) for {pattern}")
        if original != expected:
            updates[path] = expected
    return updates


def check(root: Path = ROOT) -> None:
    config = tomllib.loads((root / "mise.toml").read_text(encoding="utf-8"))
    if config.get("tool_config", {}).get("locked") is not True:
        raise ValueError("mise.toml must require locked tool installation")
    lock = tomllib.loads((root / "mise.lock").read_text(encoding="utf-8"))
    for name, version in config["tools"].items():
        entries = lock.get("tools", {}).get(name, [])
        if len(entries) != 1 or entries[0].get("version") != version:
            raise ValueError(f"mise.lock does not match {name}; run mise lock")
    updates = literal_updates(root)
    if updates:
        paths = ", ".join(str(path.relative_to(root)) for path in updates)
        raise ValueError(f"toolchain literals drift in {paths}; run mise run toolchain-sync")


def doctor(root: Path = ROOT) -> None:
    check(root)
    expected = runtime_pins(root) | {"mise": pin("tools.mise", root)}
    commands = {
        "bun": (["bun", "--version"], "", root),
        "go": (["go", "env", "GOVERSION"], "go", root / "go"),
        "mise": (["mise", "--version"], "", root),
    }
    actual = {"python": platform.python_version()}
    for name, (command, prefix, directory) in commands.items():
        result = subprocess.run(command, cwd=directory, capture_output=True, text=True, check=True)
        actual[name] = result.stdout.strip().removeprefix(prefix)
    actual["go"] = actual["go"].split("-", 1)[0]
    actual["mise"] = actual["mise"].split()[0]
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
