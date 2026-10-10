#!/usr/bin/env python3
"""Read repository tool pins and validate literals required before code can run."""
from __future__ import annotations

import argparse
import hashlib
import platform
import re
import subprocess
import tomllib
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TOOL_KEYS = {
    "bun": "bun", "python": "python", "go": "go",
    "gitleaks": "aqua:gitleaks/gitleaks",
    "staticcheck": "go:honnef.co/go/tools/cmd/staticcheck",
    "govulncheck": "go:golang.org/x/vuln/cmd/govulncheck",
    "ty": "aqua:astral-sh/ty",
    "actionlint": "aqua:rhysd/actionlint",
    "zizmor": "aqua:zizmorcore/zizmor",
    "cargo-deny": "aqua:EmbarkStudios/cargo-deny",
    "cargo-nextest": "aqua:nextest-rs/nextest/cargo-nextest",
}
PIN_PATTERNS = {
    "browser": {"chrome": r"\d+\.\d+\.\d+\.\d+"},
    "images": {
        "skopeo": r"quay\.io/containers/skopeo:v\d+\.\d+\.\d+(?:-immutable)?@sha256:[0-9a-f]{64}",
        "binfmt": r"docker\.io/tonistiigi/binfmt@sha256:[0-9a-f]{64}",
        "bun": r"docker\.io/oven/bun:\d+\.\d+\.\d+@sha256:[0-9a-f]{64}",
        "golang": r"docker\.io/library/golang:\d+\.\d+\.\d+@sha256:[0-9a-f]{64}",
        "python": r"docker\.io/library/python:\d+\.\d+\.\d+-slim-bookworm@sha256:[0-9a-f]{64}",
        "rust": r"docker\.io/library/rust:\d+\.\d+\.\d+-bookworm@sha256:[0-9a-f]{64}",
    },
}


def rust_channel(root: Path = ROOT) -> str:
    toolchain = tomllib.loads((root / "rust/rust-toolchain.toml").read_text(encoding="utf-8"))["toolchain"]
    channel = toolchain.get("channel")
    if not isinstance(channel, str) or re.fullmatch(r"\d+\.\d+\.\d+", channel) is None:
        raise ValueError("rust/rust-toolchain.toml must select an exact Rust release")
    return channel


def load_pins(root: Path = ROOT) -> dict[str, dict[str, str]]:
    data = tomllib.loads((root / "mise.toml").read_text(encoding="utf-8"))
    tools = data.get("tools", {})
    if not isinstance(tools, dict) or tools.keys() != set(TOOL_KEYS.values()):
        raise ValueError("mise.toml [tools] must contain exactly the reviewed runtime and checker backends")
    pins: dict[str, dict[str, str]] = {"tools": {}}
    for name, key in TOOL_KEYS.items():
        value = tools[key]
        # A Go-built tool may pin a commit's pseudo-version until a release carries the fix it needs.
        exact = r"\d+\.\d+\.\d+" + (r"(?:-0\.dev\.0\.\d{14}-[0-9a-f]{12})?" if key.startswith("go:") else "")
        if not isinstance(value, str) or re.fullmatch(exact, value) is None:
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
    for name, runtime in (("bun", "bun"), ("golang", "go"), ("python", "python")):
        if image_version(pins["images"][name]) != pins["runtime"][runtime]:
            raise ValueError(f"mise.toml images.{name} must use the tools.{runtime} version")
    manifest = metadata.get("rust_manifest_sha256")
    if not isinstance(manifest, str) or re.fullmatch(r"[0-9a-f]{64}", manifest) is None:
        raise ValueError("mise.toml vars.rust_manifest_sha256 must be a SHA-256")
    pins["rust"] = {"channel": rust_channel(root), "manifest": manifest}
    if image_version(pins["images"]["rust"]) != pins["rust"]["channel"]:
        raise ValueError("mise.toml images.rust must use the rust/rust-toolchain.toml channel")
    return pins


def image_version(image: str) -> str:
    """The release in an image tag such as `python:3.14.7-slim-bookworm@sha256:...`."""
    return image.split(":")[1].split("@")[0].split("-")[0]


def pin(name: str, root: Path = ROOT) -> str:
    section, key = name.split(".", 1)
    return load_pins(root)[section][key]


def rust_downloads(manifest: bytes) -> dict[tuple[str, str], dict[str, object]]:
    """Every package archive's URL and SHA-256 in a channel manifest, in each compression rustup may pick."""
    packages = tomllib.loads(manifest.decode()).get("pkg", {})
    return {(name, target): {key: value for key, value in item.items()
                             if re.fullmatch(r"(?:\w+_)?(?:url|hash)", key)}
            for name, package in packages.items() for target, item in package.get("target", {}).items()}


def check_rust_manifest(installed: Path, manifest: bytes, root: Path = ROOT) -> None:
    """Require the pinned channel manifest and rustup's rewritten installed copy to name the same archives."""
    if hashlib.sha256(manifest).hexdigest() != pin("rust.manifest", root):
        raise ValueError("the Rust channel manifest does not match mise.toml's rust_manifest_sha256")
    if rust_downloads(installed.read_bytes()) != rust_downloads(manifest):
        raise ValueError(f"rustup installed Rust {rust_channel(root)} from another than the pinned manifest")


def verify_rust_toolchain(root: Path = ROOT) -> None:
    channel = rust_channel(root)
    url = f"https://static.rust-lang.org/dist/channel-rust-{channel}.toml"
    with urllib.request.urlopen(url, timeout=60) as reply:
        manifest = reply.read(64 * 1024 * 1024)
    sysroot = subprocess.run(["rustc", f"+{channel}", "--print", "sysroot"], capture_output=True, text=True,
                             check=True).stdout.strip()
    check_rust_manifest(Path(sysroot) / "lib/rustlib/multirust-channel-manifest.toml", manifest, root)


def runtime_pins(root: Path = ROOT) -> dict[str, str]:
    return load_pins(root)["runtime"]


def literal_updates(root: Path = ROOT) -> dict[Path, str]:
    """Return expected source for unavoidable pre-execution literals; never execute pin data."""
    pins, runtimes = load_pins(root), runtime_pins(root)
    replacements = {
        "go/go.mod": [(r"(?m)^go \S+$", f"go {runtimes['go']}")],
        "container/Dockerfile": [
            (r"(?m)^FROM docker\.io/oven/bun:\S+ AS client$",
             f"FROM {pins['images']['bun']} AS client"),
            (r"(?m)^FROM docker\.io/library/golang:\S+ AS server$",
             f"FROM {pins['images']['golang']} AS server"),
        ],
        "container/Dockerfile.rust": [
            (r"(?m)^(FROM --platform=\$BUILDPLATFORM )docker\.io/library/python:\S+( AS python)$",
             rf"\g<1>{pins['images']['python']}\g<2>"),
            (r"(?m)^(FROM --platform=\$BUILDPLATFORM )docker\.io/library/rust:\S+( AS rust-amd64)$",
             rf"\g<1>{pins['images']['rust']}\g<2>"),
            (r"(?m)^(FROM --platform=\$BUILDPLATFORM )docker\.io/oven/bun:\S+( AS browser)$",
             rf"\g<1>{pins['images']['bun']}\g<2>"),
            # The CA roots come from Go's builder image.
            (r"(?m)^FROM docker\.io/library/golang:\S+ AS ca-certificates$",
             f"FROM {pins['images']['golang']} AS ca-certificates"),
        ],
    }
    # setup-qemu registers arm64 emulation with this image, privileged.
    for name in (".github/workflows/ci.yml", ".github/workflows/release-request.yml"):
        replacements[name] = [
            (r"(?m)^(\s*image: )docker.io/tonistiigi/binfmt@\S+$", rf"\g<1>{pins['images']['binfmt']}"),
        ]
    replacements[".github/workflows/release.yml"] = [
        (r"(?m)^(\s*SKOPEO_IMAGE: )quay.io/containers/skopeo:\S+$", rf"\g<1>{pins['images']['skopeo']}"),
    ]
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
    parser.add_argument("command",
                        choices=("get", "check", "sync", "doctor", "python-target", "verify-rust"))
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
            case "verify-rust":
                verify_rust_toolchain()
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as exc:
        parser.exit(1, f"toolchains: {exc}\n")


if __name__ == "__main__":
    main()
