# Legal pipeline

This guide is for maintainers changing dependencies or distributed artifacts. For the project's license, see
[LICENSE](../LICENSE) and [COPYRIGHT](../COPYRIGHT). Build prerequisites and the normal gates are in the
[development guide](../docs/DEVELOPMENT.md).

Every shipped binary embeds the notices of exactly what it compiled and linked, and every release attaches the source
of those components. The standard-library Python package `scripts/legal` owns review policy, fingerprints, notices,
inventories and source offers. It installs nothing and looks nothing up online, apart from the pinned notice sources
below.

| Change                                 | Action                                                                       |
| -------------------------------------- | ---------------------------------------------------------------------------- |
| Application or documentation only      | None beyond the normal gate.                                                 |
| Crate version, unchanged legal files   | None: a registry review covers its crate across versions.                    |
| New component or changed legal facts   | Review it and add a record (below); `mise run rust-check` refuses it until then. |

## Review files

- `rust-reviewed-components.json`: one approved review per line for each crate a shipped binary compiles. A registry
  review covers its crate name and source across versions while the license expression, modification status and
  legal-file fingerprints stay the same; a git review binds the exact version and fork revision. Unused approvals may
  remain. `python3 -m scripts.legal.check_rust_reviews --format` rewrites the file in its layout.
- `rust-provenance.json` and `manual/rust/`: legal texts, with their SHA-256 and review notes, for crates that do not
  package their own.
- `reviewed-components.json` and `provenance.json`: npm packages the browser bundle includes (none today), the fonts
  it ships and the image's CA roots.
- `rust-platform-debian-bookworm.json`: one reviewed record per shipped target for what the toolchain and builder link
  beyond Cargo's packages: native inputs, system-library imports, the notice texts that cover them, and
  `noticesSha256`, their fingerprint.
- `rust-notice-sources.json`: pinned upstream URLs and SHA-256 of notice texts the builder lacks, such as musl's and
  LLVM's, bound to the Rust release in `rustVersion`.
- `rust-forks.json`: the reviewed forks, described below.
- `project.json`: the project's name, repository and copyright holder and years; `COPYRIGHT` must match it, which
  `mise run pipeline-test` checks.

New components are never approved by matching a familiar license template: inspect the exact upstream revision and
its legal files, then add the record. A failed check prints what it found; for a build,
`python3 -m scripts.legal.rust --package NAME --development --out DIR --review-template` prints pending review
records of the crates that lack one.

`mise run rust-check` runs `scripts.legal.check_rust_reviews`. The client ships on every TUI platform and the server
on every server platform listed in `[workspace.metadata.graphite-meter]` of `rust/Cargo.toml`. For each build the
check lists the compiled crates with `cargo tree` over normal and build edges and requires an approved review for
each, and every shipped target needs an approved platform record. The static x86_64 Linux server and client compile
at most 143 and 150 crates, a dependency policy rather than a license requirement.

## Updating pinned forks

The workspace builds noq and h2 from forks that carry Graphite Meter patches on a stable upstream release. Each pin in
`rust/Cargo.toml` has a record in `rust-forks.json`: the fork branch and full-SHA `rev`, the upstream tag and its
commit `base`, `diffSha256` (the SHA-256 of `git diff-tree -r --no-renames --full-index base rev`), the changed
packages and files, and each commit's subject, purpose and origin.

To move a fork, carry the patches onto the new upstream release on its `graphite-meter/<crate>-v<version>` branch and
review the upstream base, each patch's origin, the changed paths and their licenses. Then update `rust/Cargo.toml`,
`rust/Cargo.lock` and the record. `mise run rust-check` requires a record for every git package in the lock and a lock
package for every record; CI also runs `python3 -m scripts.legal.check_git_sources --verify`, which fetches each
branch and tag and checks ancestry, the change-set digest and scope, and the commit list.

## Notices and source offers

`python3 -m scripts.legal.rust` collects a binary's notices around one real Cargo build of it, in three steps:

1. Prepare. `cargo tree` for the target and the host gives a conservative set of crates the build may compile,
   build scripts and procedural macros included. Each needs its approved review or manual provenance; the target
   needs its approved platform record, whose missing notice texts are fetched from `rust-notice-sources.json` into
   `legal/manual/` and checked by SHA-256. The step writes the directory the build reads as `GM_LEGAL_DIR`:
   `LEGAL.txt` (the `--legal` report) with its SHA-256 in `LEGAL.sha256`, `package.txt`, `target.txt`,
   `rustc-path.txt`, and `inputs.txt` with copies of those inputs under `inputs/`. For the server it also stages the
   browser build in `browser-assets/` with its `legal/` files (the license, the notices and `about.json`, whose
   components link their source, upstream and changes) and, for a reviewed build, writes the image's
   `IMAGE_NOTICES.txt`.
2. Build and verify. One `cargo rustc` build embeds the notices; the build script refuses a directory prepared for
   another package, target or compiler, or whose input copies differ. The collector then requires that the build
   compiled only prepared crates with unchanged legal files, linked only the record's native inputs, imported only its
   system libraries, embedded exactly `LEGAL.txt` and, for a reviewed build, carries
   `graphite-meter reviewed notices sha256:` with its SHA-256 in plain text, which release verification compares with
   the notices shipped beside the executable. It then writes `inventory.json` of the compiled crates and the server's
   browser packages.
3. Source offer. A reviewed build writes its part of the release's one source offer,
   `graphite-meter_VERSION_third-party-source.tar.gz`: the compiled crates' and browser packages' sources under
   `third_party/`, its inventory and report under `builds/<package>_<os>_<arch>/`, the fork records and the manual
   material. Release staging merges the parts of every shipped build into that one archive. The build also writes the
   `SOURCE.txt` its client archive or image ships, which names the release source, the source offer and the build. A
   prerelease publishes no tag or source offer, so its `SOURCE.txt` names the repository.

`--development` notices, which local builds (`scripts.build`, through `mise run dev`, `tui`, `server-build`,
`tui-build` and `e2e-server`) embed, need the dependency reviews but no platform record, so any host builds them. Their
report and their executable carry `UNREVIEWED DEVELOPMENT BUILD`: such a build is not distributable. A reviewed
build's executable must not carry the marker, the workflow policy keeps development builds out of everything CI and
releases run, and release verification refuses an image or archive whose executable or notices carry it.
