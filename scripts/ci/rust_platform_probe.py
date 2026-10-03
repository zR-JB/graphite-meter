"""Temporary hosted evidence collector for the Rust 1.99.0 platform review."""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
from pathlib import Path

from scripts.ci.toolchains import rust_channel
from scripts.legal import rust_platform

ROOT = Path(__file__).resolve().parents[2]
TARGETS = ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl", "x86_64-pc-windows-gnu")


def collect() -> None:
    evidence = Path("/evidence")
    evidence.mkdir()
    channel = rust_channel(ROOT)
    subprocess.run(["python3", "-m", "scripts.ci.toolchains", "verify-rust"], cwd=ROOT, check=True)
    subprocess.run(["rustup", "target", "add", "--toolchain", channel, *TARGETS], check=True)
    compiler = subprocess.check_output(["rustc", f"+{channel}", "-vV"], text=True)
    (evidence / "rustc.txt").write_text(compiler)
    (evidence / "profile.txt").write_text("ci: opt-level=3, lto=false, codegen-units=16; evidence only\n")
    packages = subprocess.check_output(["dpkg-query", "-W"], text=True)
    (evidence / "native-packages.txt").write_text("\n".join(
        line for line in packages.splitlines() if re.match(r"(?:gcc|g\+\+|libgcc|libc6|mingw|binutils)", line)
    ) + "\n")
    sysroot = Path(subprocess.check_output(["rustc", f"+{channel}", "--print", "sysroot"], text=True).strip())
    documents = sysroot / "share/doc/rust"
    notices = evidence / "sysroot-notices"
    notices.mkdir()
    shutil.copyfile(documents / "COPYRIGHT-library.html", notices / "COPYRIGHT-library.html")
    for path in sorted((documents / "licenses").iterdir()):
        shutil.copyfile(path, notices / path.name)
    host = next(line.removeprefix("host: ") for line in compiler.splitlines() if line.startswith("host: "))
    standard_inputs = rust_platform.rlibs(sysroot, host) | rust_platform.notice_names(None, sysroot).keys()
    listing, _ = rust_platform.fingerprint(standard_inputs, sysroot, set())
    (evidence / f"{host}-standard-inputs.txt").write_text(listing)
    for target in TARGETS:
        native = sysroot / "lib/rustlib" / target / "lib/self-contained"
        comments = set()
        for path in sorted(native.glob("*")):
            if path.suffix not in (".a", ".o"):
                continue
            result = subprocess.run(["readelf", "-p", ".comment", str(path)],
                                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
            comments.update(line.strip() for line in result.stdout.splitlines()
                            if "GCC:" in line or "clang version" in line)
        (evidence / f"{target}-native-comments.txt").write_text("\n".join(sorted(comments)) + "\n")
        for package in ("graphite-meter-client", "graphite-meter-server"):
            if target.endswith("windows-gnu") and package.endswith("server"):
                continue
            output = ROOT / "rust/target/platform-probe" / target / package
            subprocess.run([
                "python3", "-m", "scripts.legal.rust", "--package", package, "--target", target,
                "--profile", "ci", "--version", "0.0.0-review", "--out", str(output),
                "--reviews", "legal/rust-reviewed-components.json",
                "--supplement", "legal/rust-platform-debian-bookworm.json", "--review-template",
            ], cwd=ROOT, check=True)
            destination = evidence / target / package
            destination.mkdir(parents=True)
            for name in ("platform-candidate.json", "platform-inputs.txt", "inventory.json", "review-errors.json"):
                shutil.copyfile(output / name, destination / name)
            link_map = rust_platform.link_map(output, target, "ci")
            observed = rust_platform.linked(link_map, sysroot, {ROOT / "rust", Path(os.environ["CARGO_HOME"])})
            (destination / "observed-native-inputs.json").write_text(json.dumps(sorted(observed), indent=2) + "\n")
            paths = [str(rust_platform.source(path, sysroot)) for path in observed]
            # Keep every native-object/archive line, without megabytes of application/Cargo symbols.
            native_lines = [line for line in link_map.read_text().splitlines() if any(path in line for path in paths)]
            (destination / "native-link-map.txt").write_text("\n".join(native_lines) + "\n")
            executable = ROOT / "rust/target" / target / "ci" / (package + (".exe" if "windows" in target else ""))
            (destination / "system-imports.json").write_text(
                json.dumps(sorted(rust_platform.imports(executable, target)), indent=2) + "\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    selection = parser.add_mutually_exclusive_group(required=True)
    selection.add_argument("--dockerfile", type=Path)
    selection.add_argument("--collect", action="store_true")
    args = parser.parse_args()
    if args.collect:
        collect()
        return
    # Reuse the exact release builder, snapshot and cross-compiler pins; stop before packaging.
    prefix, separator, _ = (ROOT / "container/Dockerfile.rust").read_text().partition("ARG GM_RUST_DEPENDENCY_CACHE=0\n")
    if not separator:
        raise ValueError("release builder boundary changed")
    args.dockerfile.write_text(prefix + """COPY --from=source / /src/
RUN python3 -m scripts.ci.rust_platform_probe --collect
FROM scratch AS platform-evidence
COPY --from=tui-build /evidence/ /
""")


if __name__ == "__main__":
    main()
