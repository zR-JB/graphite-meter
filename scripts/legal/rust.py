"""Third-party notices of a Rust binary, collected around one real Cargo build of it.

    python3 -m scripts.legal.rust --package NAME (--target TRIPLE | --development) --out DIR [...]

prepare: the crates the build may compile (a conservative superset that keeps build scripts and procedural
macros), their reviewed notices, the platform's reviewed notices, and snapshots of the inputs these derive from,
in the directory rust/legal's build side reads as GM_RUST_LEGAL_DIR. build_verify: one `cargo rustc` build
that embeds them, then checks that it compiled only prepared crates with unchanged notices, linked and imported
only reviewed platform inputs, and embedded exactly the prepared report. source_offer: the third-party source
of a reviewed build and the SOURCE.txt that names it. --development notices need every dependency review but no
platform record, so any host builds them; they say they are unreviewed, and so does the executable
(legal/README.md).
"""
from __future__ import annotations

import argparse
import gzip
import json
import os
import re
import subprocess
import sys
import tarfile
import tempfile
import zlib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ..ci.github_api import ControlPlaneError, local_path
from ..ci.rust_workspace import ROOT, load, offer_name
from ..ci.toolchains import rust_channel
from . import rust_platform as platform
from .artifacts import about, add_bytes, add_tree, legal_header, notices, release_source
from .check_rust_reviews import layout
from .discovery import discover_browser
from .model import (Component, LegalError, Project, Provenance, Review, array, manual_files, manual_sources,
                    marshal, read_json, sha256)
from .review import add_provenance, component_key, prepare_scopes
from .rust_inventory import Metadata, candidates, cargo_metadata, compiled, components, prepared

PACKAGES = ("graphite-meter-client", "graphite-meter-server")
PROFILES = ("ci", "release")
VERSION = re.compile(r"[A-Za-z0-9][A-Za-z0-9.+-]*")
# Opens the notices of a --development build; rust/legal keeps it in that build's executable too.
DEVELOPMENT = "UNREVIEWED DEVELOPMENT BUILD"
DEVELOPMENT_NOTICE = (f"{DEVELOPMENT}\n\nThis build's host generated these notices. They cover the dependencies it "
                      "may compile, but not its Rust standard library, C runtime or system libraries, which only the "
                      "reviewed platform records of the release builders cover. Do not distribute this build.\n\n")
DEVELOPMENT_PLATFORM = ("This development build's Rust standard library, C runtime and system libraries are not "
                        "reviewed, so their notices are not included.\n")
# Inputs the notices derive from; rust/legal refuses a build whose copies differ.
INPUTS = ("LICENSE", "legal/project.json", "legal/rust-forks.json", "legal/rust-notice-sources.json",
          "legal/rust-provenance.json", "legal/rust-reviewed-components.json", "rust/Cargo.lock", "rust/Cargo.toml",
          "rust/rust-toolchain.toml")
SERVER_INPUTS = ("legal/provenance.json", "legal/reviewed-components.json")


@dataclass
class Build:
    package: str
    target: str | None  # None: a development build for the host
    profile: str
    version: str
    out: Path
    browser: tuple[Path, Path] | None = None  # the server's browser build and its module scan

    @property
    def development(self) -> bool:
        return self.target is None


@dataclass
class Prepared:
    host: str
    target: str
    sysroot: Path
    metadata: Metadata
    selected: set[str]
    components: list[Component]
    platform: str
    record: dict[str, Any] | None
    reviews: list[Review]
    provenance: list[Provenance]
    browser: list[Component] = field(default_factory=list[Component])


def write_changed(path: Path, data: bytes) -> None:
    """Write `data` unless `path` holds it already, so Cargo sees unchanged inputs unchanged."""
    if not path.is_file() or path.read_bytes() != data:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)


def stage_browser(source: Path, destination: Path) -> None:
    """Mirror the browser build into `destination`, except `legal/`, which the server's notices own."""
    if not (source / "index.html").is_file():
        raise LegalError(f"the browser build {source} has no index.html")
    files = {}
    for path in source.rglob("*"):
        if path.is_symlink():
            raise LegalError(f"the browser build holds a symbolic link: {path}")
        if path.is_file() and path.relative_to(source).parts[0] != "legal":
            files[path.relative_to(source)] = path
    for path in sorted(destination.rglob("*"), reverse=True):
        relative = path.relative_to(destination)
        if relative.parts[0] != "legal" and (path.is_symlink() or path.is_file() and relative not in files):
            path.unlink()
        elif relative.parts[0] != "legal" and path.is_dir() and not any(path.iterdir()):
            path.rmdir()
    for relative, path in files.items():
        write_changed(destination / relative, path.read_bytes())


def report(project: Project, version: str, body: str, development: bool) -> bytes:
    """A binary's `-legal` report: Go's TUI report layout, opened by the development notice if unreviewed."""
    header = legal_header(project, release_source(project, version)[1], (ROOT / "LICENSE").read_bytes())
    return (DEVELOPMENT_NOTICE.encode() if development else b"") + header + b"\n" + body.encode()


def platform_section(text: str) -> str:
    return "\nRust sysroot and platform notices\n\n" + text


def checked_platform_notice(record: dict[str, Any] | None, facts: dict[str, Any]) -> str:
    try:
        return platform.notice(record, **facts)
    except (LegalError, OSError) as error:
        candidate, listing = platform.candidate(record, **facts)
        raise LegalError(f"{error}\nUnreviewed platform record of this build:\n{marshal(candidate).decode()}"
                         "Its noticesSha256 hashes this listing of its notices:\n"
                         + listing.removesuffix("\n")) from error


def toolchain() -> tuple[str, Path, str]:
    """The pinned toolchain's host triple, sysroot and rustc path."""
    rust = ROOT / "rust"
    described = subprocess.check_output(["rustc", "-vV"], cwd=rust, text=True)
    host = next(line.removeprefix("host: ") for line in described.splitlines() if line.startswith("host: "))
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], cwd=rust, text=True).strip())
    return host, sysroot, subprocess.check_output(["rustup", "which", "rustc"], cwd=rust, text=True).strip()


def prepare(build: Build) -> Prepared:
    """Write every notice and input snapshot the build embeds and checks."""
    host, sysroot, rustc = toolchain()
    target = build.target or host
    record = None
    if not build.development:
        record = platform.record(ROOT / load().platform_record, target)
        if record is not None:
            platform.fetch_notices(ROOT, record)
    metadata = cargo_metadata(host, target)
    selected = prepared(metadata, build.package, host, target)
    reviews = [Review.parse(item) for item in array(read_json(ROOT / "legal/rust-reviewed-components.json"))]
    provenance = manual_sources(ROOT, build.package)
    found, _, failures = components(metadata, {identity: {} for identity in selected}, reviews, provenance)
    if failures:
        raise LegalError("Rust dependency notices need review (--review-template prints candidates):\n"
                         + "\n".join(f"  {name} {version}: {error}" for (name, version, _), error in failures.items()))
    facts = {"target": target, "sysroot": sysroot, "inputs": set(), "libraries": set()}
    text = DEVELOPMENT_PLATFORM if build.development else checked_platform_notice(record, facts)
    state = Prepared(host, target, sysroot, metadata, selected, found, text, record, reviews, provenance)
    project = Project.read(ROOT)
    body = notices(found) + platform_section(text)
    if build.browser is not None:
        body = browser_notices(build, state, project)
    write_changed(build.out / "LEGAL.txt", report(project, build.version, body, build.development))
    inputs = [*INPUTS, *(SERVER_INPUTS if build.browser else ()),
              *(file.name for entry in provenance for file in entry.localLegalFiles if not file.name.startswith("/"))]
    inputs += [str(Path(item["manifest_path"]).resolve().relative_to(ROOT)) for item in metadata["packages"]
               if item["id"] in metadata["workspace_members"]]
    if record is not None:
        inputs.append(load().platform_record.as_posix())
        inputs += [name for name in platform.own_notices(record) if not name.startswith("/")]
    for relative in sorted(set(inputs)):
        write_changed(build.out / "inputs" / relative, (ROOT / relative).read_bytes())
    write_changed(build.out / "inputs.txt", "".join(f"{name}\n" for name in sorted(set(inputs))).encode())
    for name, value in (("package.txt", build.package), ("target.txt", target), ("rustc-path.txt", rustc)):
        write_changed(build.out / name, value.encode())
    return state


def browser_notices(build: Build, state: Prepared, project: Project) -> str:
    """Stage the server's browser build with its legal files and return the notices both share."""
    assert build.browser is not None
    assets, scan = build.browser
    browser_reviews = [Review.parse(item) for item in array(read_json(ROOT / "legal/reviewed-components.json"))]
    scopes = {"server/browser": add_provenance(ROOT, discover_browser(scan, browser_reviews), state.provenance,
                                               "server/browser")}
    shipped = {component_key(component) for component in scopes["server/browser"]}
    # The image adds what Go's image adds to its server, such as the CA roots.
    scopes["container"] = [component for component in add_provenance(ROOT, scopes["server/browser"],
                                                                      state.provenance, "container")
                           if component_key(component) not in shipped]
    prepare_scopes(scopes, browser_reviews, "check")
    state.browser = scopes["server/browser"]
    staged = build.out / "browser-assets"
    stage_browser(assets, staged)
    shared = ((DEVELOPMENT_NOTICE if build.development else "") + notices(state.components + state.browser)
              + platform_section(state.platform))
    version, source_url = release_source(project, build.version)
    write_changed(staged / "legal/LICENSE.txt", (ROOT / "LICENSE").read_bytes())
    write_changed(staged / "legal/THIRD_PARTY_NOTICES.txt", shared.encode())
    write_changed(staged / "legal/about.json", about(project, version, source_url, state.components + state.browser))
    if not build.development:
        image = notices(state.components + state.browser + scopes["container"]) + platform_section(state.platform)
        write_changed(build.out / "IMAGE_NOTICES.txt", report(project, build.version, image, False))
    return shared


def build_verify(build: Build, state: Prepared) -> tuple[Path, list[Component]]:
    """Build the binary once with the prepared notices and check what it compiled, linked and embedded."""
    rust = ROOT / "rust"
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GM_RUST_")}
    environment |= {"CARGO_TARGET_DIR": str(rust / "target"), "GM_RUST_LEGAL_DIR": str(build.out),
                    "GM_ENGINE_VERSION": f"{build.version}-rust"}
    if build.browser is not None:
        environment["GM_RUST_ASSET_DIR"] = str(build.out / "browser-assets")
    command = ["cargo", "rustc", "--locked", "--package", build.package, "--bin", build.package,
               "--profile", build.profile, "--message-format=json-render-diagnostics"]
    link_map = None
    if build.target is not None:
        cargo_home = os.environ.get("CARGO_HOME") or str(Path.home() / ".cargo")
        # A distributed build names neither the checkout nor Cargo's home.
        environment["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(
            f"--remap-path-prefix={path}={name}" for path, name in ((ROOT, "/src"), (cargo_home, "/cargo")))
        link_map = platform.link_map(build.out, build.target, build.profile)
        command += ["--target", build.target, "--", f"-Clink-arg=-Wl,-Map={link_map}"]
    output = subprocess.run(command, cwd=rust, env=environment, stdout=subprocess.PIPE, check=True, text=True).stdout
    messages = [json.loads(line) for line in output.splitlines() if line.startswith("{")]
    root = next(item["id"] for item in state.metadata["packages"]
                if item["name"] == build.package and item["id"] in state.metadata["workspace_members"])
    builds = compiled(messages, root, build.package)
    if unprepared := sorted(builds.keys() - state.selected):
        raise LegalError(f"the build compiled packages absent from the prepared notices: {unprepared}")
    actual, inventory, failures = components(state.metadata, builds, state.reviews, state.provenance)
    expected = {component_key(item): item.json() for item in state.components}
    if failures or any(expected.get(component_key(item)) != item.json() for item in actual):
        raise LegalError("the build compiled dependencies with missing or changed notices"
                         + "".join(f"\n  {name} {version}: {error}" for (name, version, _), error in failures.items()))
    executable = Path(next(message["executable"] for message in messages if message.get("executable")
                           and message["package_id"] == root))
    if link_map is not None:
        directories = {Path(state.metadata["target_directory"])} | {
            Path(item["manifest_path"]).parent for item in state.metadata["packages"]}
        facts = {"target": state.target, "sysroot": state.sysroot,
                 "inputs": platform.linked(link_map, state.sysroot, directories),
                 "libraries": platform.imports(executable, state.target)}
        if checked_platform_notice(state.record, facts) != state.platform:
            raise LegalError("the linked executable's platform notices differ from the prepared ones")
    out_dir = next(message["out_dir"] for message in messages
                   if message.get("reason") == "build-script-executed" and message["package_id"] == root)
    payload, binary = (Path(out_dir) / "legal.zlib").read_bytes(), executable.read_bytes()
    if payload not in binary or zlib.decompress(payload) != (build.out / "LEGAL.txt").read_bytes():
        raise LegalError("the executable does not embed the prepared notices")
    if (DEVELOPMENT.encode() in binary) != build.development:
        raise LegalError(f"only a development build's executable may carry {DEVELOPMENT!r}")
    write_changed(build.out / "inventory.json", marshal({
        "schemaVersion": 1, "package": build.package, "target": state.target, "profile": build.profile,
        "scope": "compiled Cargo inputs, including build scripts and procedural macros",
        "cargoLockSha256": sha256((rust / "Cargo.lock").read_bytes()), "components": inventory}))
    return executable, actual


def source_notice(project: Project, version: str, offer: str, target: str) -> str:
    """SOURCE.txt of a reviewed build, in its TUI archive or image: its source, release, source offer and target."""
    return (f"Graphite Meter source: {release_source(project, version)[1]}\nMatching release: v{version}\n"
            f"Dependency source archive: {offer}\nRust target: {target}\n")


def source_offer(build: Build, state: Prepared, actual: list[Component]) -> None:
    """The source offer, named by offer_name, of the compiled crates' and browser packages' sources and the manual
    material, as Go's under one directory; and the SOURCE.txt that names it."""
    assert build.target is not None
    shipped = load().server if build.package == "graphite-meter-server" else load().tui
    platform_name = next(name for name, target in shipped.items() if target == build.target)
    offer = offer_name(build.package, build.version, platform_name)
    root = offer.removesuffix(".tar.gz")
    # One offer per directory, so the image build copies exactly this one.
    for stale in build.out.glob("*_third-party-source.tar.gz"):
        stale.unlink()
    with tempfile.TemporaryDirectory(prefix="rust-sources-") as scratch:
        vendor = Path(scratch) / "vendor"
        subprocess.run(["cargo", "vendor", "--locked", "--versioned-dirs", str(vendor)], cwd=ROOT / "rust",
                       stdout=subprocess.DEVNULL, check=True, timeout=600)
        with (build.out / offer).open("wb") as raw, \
                gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed, \
                tarfile.open(fileobj=compressed, mode="w") as archive:
            for component in actual:
                name = f"{component.name}-{component.version}"
                add_tree(archive, vendor / name, f"{root}/third_party/cargo/{name}", vendor / name)
            for component in state.browser:
                if component.source_path is not None:
                    add_tree(archive, component.source_path,
                             f"{root}/third_party/{component.ecosystem}/{component.name}-{component.version}",
                             component.source_path)
            for name in ("inventory.json", "LEGAL.txt"):
                add_bytes(archive, f"{root}/{name}", (build.out / name).read_bytes())
            add_bytes(archive, f"{root}/legal/rust-forks.json", (ROOT / "legal/rust-forks.json").read_bytes())
            # Entries can share a file, such as one license for two fonts; the archive holds it once.
            for path in dict.fromkeys(path for entry in state.provenance for path in manual_files(entry)):
                add_tree(archive, ROOT / path, f"{root}/{path}", ROOT / path)
    write_changed(build.out / "SOURCE.txt", source_notice(Project.read(ROOT), build.version, offer,
                                                          build.target).encode())


def collect(build: Build) -> Path:
    """Prepare, build and verify, then write a reviewed build's source offer; returns the executable."""
    # rust/rust-toolchain.toml selects the toolchain unless the caller's environment overrides it.
    os.environ["RUSTUP_TOOLCHAIN"] = rust_channel(ROOT)
    build.out.mkdir(parents=True, exist_ok=True)
    try:
        state = prepare(build)
        executable, actual = build_verify(build, state)
        if not build.development:
            source_offer(build, state, actual)
    except BaseException:
        # A later build must not embed notices whose checks failed.
        (build.out / "LEGAL.txt").unlink(missing_ok=True)
        raise
    return executable


def review_template(build: Build) -> bytes:
    """Pending reviews, in the reviews file's layout, of the crates the build may compile that lack one."""
    os.environ["RUSTUP_TOOLCHAIN"] = rust_channel(ROOT)
    host, _, _ = toolchain()
    target = build.target or host
    metadata = cargo_metadata(host, target)
    selected = prepared(metadata, build.package, host, target)
    found, _, failures = components(metadata, {identity: {} for identity in selected},
                                    [Review.parse(item) for item in
                                     array(read_json(ROOT / "legal/rust-reviewed-components.json"))],
                                    manual_sources(ROOT, build.package))
    return layout(candidates(found, failures))


def parse(arguments: list[str] | None = None) -> tuple[Build, bool]:
    # Only the exact --development selects development notices; the workflow policy looks for it.
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter,
                                     allow_abbrev=False)
    parser.add_argument("--package", choices=PACKAGES, required=True)
    kind = parser.add_mutually_exclusive_group(required=True)
    kind.add_argument("--target", help="a shipped Rust target, built with its reviewed platform record")
    kind.add_argument("--development", action="store_true", help="unreviewed notices of a build for this host")
    parser.add_argument("--profile", choices=PROFILES, default="release")
    parser.add_argument("--version", default="development", help="the release version whose tag is the source")
    parser.add_argument("--out", type=Path, required=True, help="GM_RUST_LEGAL_DIR, inside the checkout")
    parser.add_argument("--browser-assets", type=Path, help="the server's production browser build")
    parser.add_argument("--browser-scan", type=Path, help="the module scan of that browser build")
    parser.add_argument("--review-template", action="store_true", help="print pending reviews and build nothing")
    args = parser.parse_args(arguments)
    workspace = load()
    shipped = workspace.server if args.package == "graphite-meter-server" else workspace.tui
    if args.target is not None and args.target not in shipped.values():
        parser.error(f"{args.package} ships for {', '.join(shipped.values())}")
    if VERSION.fullmatch(args.version) is None:
        parser.error("--version must be a release identifier")
    if (args.package == "graphite-meter-server") != (args.browser_assets is not None and args.browser_scan is not None):
        parser.error("the server, and only the server, needs --browser-assets and --browser-scan")
    target = next((value for value in shipped.values() if value == args.target), None)
    browser = None
    if args.browser_assets is not None and args.browser_scan is not None:
        browser = (local_path(args.browser_assets, ROOT), local_path(args.browser_scan, ROOT))
    out = local_path(args.out, ROOT)
    if not out.is_relative_to(ROOT):
        parser.error("--out must lie inside the checkout")
    return Build(PACKAGES[PACKAGES.index(args.package)], target, PROFILES[PROFILES.index(args.profile)],
                 args.version, out, browser), args.review_template


def main() -> None:
    build, template = parse()
    if template:
        sys.stdout.buffer.write(review_template(build))
        return
    executable = collect(build)
    kind = "development" if build.development else "reviewed"
    print(f"Rust {build.package} [{build.profile} profile, {kind} notices]: {executable}")


if __name__ == "__main__":
    try:
        main()
    except (ControlPlaneError, LegalError, OSError, ValueError, KeyError, zlib.error,
            subprocess.CalledProcessError) as error:
        sys.exit(f"Rust notices: {error}")
