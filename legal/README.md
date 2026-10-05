# Legal pipeline

This guide is for maintainers changing dependencies or distributed artifacts. For the project's
license, see [LICENSE](../LICENSE) and [COPYRIGHT](../COPYRIGHT). Build prerequisites and the
normal gates are in the [development guide](../docs/DEVELOPMENT.md).

| Change                                        | Action                                                                                  |
| --------------------------------------------- | --------------------------------------------------------------------------------------- |
| Application or documentation only             | Run the normal gate; no inventory regeneration unless distributed dependencies change.  |
| Dependency version with unchanged legal files | Regenerate and validate using [routine dependency update](#routine-dependency-update).  |
| New component or changed legal facts          | Follow [maintainer review](#new-dependency-or-changed-legal-facts) before regeneration. |

## Ownership and execution

The standard-library Python package `scripts/legal` owns review policy,
fingerprints, provenance validation, scoped inventories, notices, drift checks,
and deterministic third-party source archives. None of this requires compiling
or running a Go generator.

Dependency discovery remains tied to the actual build inputs: Python invokes
`go list -deps -json` for the supported server and TUI targets and `go env
GOVERSION` to verify the pinned toolchain. A temporary production Vite build
reports the browser modules that reach the bundle. Changing a product language
therefore requires changing its dependency discovery adapter, while the review
and artifact logic stays in Python. The Go runtime package only embeds the
generated TUI legal report.

All Python tooling and tests are checked with the standalone `ty` binary through
`mise run python-check`. `mise run setup` prepares the version-pinned checker; the
legal commands themselves need only the Python runtime in `mise.toml` and the relevant dependency
discovery tools. They perform no package installation or online license lookup.

## Normal development

No legal action is required beyond the normal fast gate:

```bash
mise run check
```

## Routine dependency update

```bash
mise run legal-generate
mise run ci
```

If the component's complete legal-file fingerprint is unchanged, the existing
review is reusable; generation refreshes its reviewed version and all shipped
legal metadata. A changed license expression, notice, or legal-file byte still
requires maintainer review.

CI and stable release builds run `legal-check` and fail on stale outputs, so a
dependency update, including a Dependabot PR, needs the regenerated files
committed. Stable builds then only stamp the release version into them.

## New dependency or changed legal facts

For a new dependency or changed legal file, generate the maintainer review
template first:

```bash
mise run legal-review template
```

For an independent deterministic audit manifest of discovered components:

```bash
mise run legal-review audit
```

The private generator modes and packaging helpers are not part of the normal
developer interface. Use `mise tasks` for the current public command list.

Inspect the exact upstream revision and its legal files, then add a reviewed
record to `reviewed-components.json`, regenerate, and run CI. New components
are never approved by matching a familiar license template. `legal-check`,
`legal-generate`, and `legal-review` are the public legal interface; packaging
helpers and generator modes are private implementation details.

## Custom, copied, or modified material

Use `provenance.json` for local files, forks, replacements, assets, fonts,
datasets, and other material that package managers cannot describe. Never
pretend custom material is an ordinary MIT dependency to silence the gate.

## Generated files

Do not edit these by hand:

- `COPYRIGHT`
- `legal/generated/**`
- `client/public/legal/**`
- `go/internal/legal/assets/**`

The generator also creates release `SOURCE.txt` material from the same project
metadata and reviewed component set.

## Copyright year

`legal/project.json` is the only place to update Graphite Meter's copyright
year or year range. The generator never derives it from the wall clock.

## Rust reviews

The Rust binaries have their own review files:

- `rust-reviewed-components.json`: one approved review per line for each crate
  a shipped Rust binary compiles. A registry review covers its crate name and
  source across versions while the license expression, modification status
  and legal-file fingerprints stay the same; a git review binds the exact
  version and fork revision. Unused approvals may remain.
- `rust-provenance.json` and `manual/rust/`: legal texts, with their SHA-256
  and review notes, for crates that do not package their own.
- `rust-platform-debian-bookworm.json`: one reviewed record per shipped Rust
  target for what the toolchain and builder link beyond Cargo's packages:
  native inputs, system-library imports, the notice texts that cover them, and
  `noticesSha256`, their fingerprint.
- `rust-notice-sources.json`: pinned upstream URLs and SHA-256 of notice texts
  the builder lacks, such as musl's and LLVM's, bound to the Rust release in
  `rustVersion`.
- `rust-forks.json`: the reviewed forks, described below.

`mise run rust-check` runs `scripts.legal.check_rust_reviews`. The client
ships on every TUI platform and the server on every server platform listed in
`[workspace.metadata.graphite-meter]` of `rust/Cargo.toml`. For each of these
builds, the check lists the compiled crates with `cargo tree` over normal and
build edges and requires an approved review for each. Every shipped target
needs an approved record with review notes and a notice fingerprint in the
platform record the metadata names. The reviews file keeps its layout
(`python3 -m scripts.legal.check_rust_reviews --format` rewrites it), and the
static x86_64 Linux server and client compile at most 143 and 150 crates, a
dependency policy rather than a license requirement.

## Updating pinned forks

The Rust workspace builds noq and h2 from forks that carry Graphite Meter
patches on a stable upstream release. Each pin in `rust/Cargo.toml` has a
record in `rust-forks.json`: the fork branch and full-SHA `rev`, the upstream
tag and its commit `base`, `diffSha256` (the SHA-256 of
`git diff-tree -r --no-renames --full-index base rev`), the changed packages
and files, and each commit's subject, purpose and origin.

To move a fork, carry the patches onto the new upstream release on its
`graphite-meter/<crate>-v<version>` branch and review the upstream base, each
patch's origin, the changed paths and their licenses. Then update
`rust/Cargo.toml`, `rust/Cargo.lock` and the record. `mise run rust-check`
requires a record for every git package in the lock and a lock package for
every record; CI's Rust job also runs
`python3 -m scripts.legal.check_git_sources --verify`, which fetches each
branch and tag and checks ancestry, the change-set digest and scope, and the
commit list.

## Rust notices

`python3 -m scripts.legal.rust` collects a Rust binary's notices around one
real Cargo build of it, in three steps:

1. **Prepare.** `cargo tree` for the target and the host gives a conservative
   set of crates the build may compile, build scripts and procedural macros
   included. Each needs its approved review, or manual provenance; the target
   needs its approved platform record, whose notice texts the builder lacks are
   fetched from `rust-notice-sources.json` into `legal/manual/` and checked by
   SHA-256. The step writes the directory the build reads as
   `GM_RUST_LEGAL_DIR`: `LEGAL.txt` (the `-legal` report), `package.txt`,
   `target.txt`, `rustc-path.txt`, and `inputs.txt` with copies of those inputs
   under `inputs/`. For the server it also stages the browser build in
   `browser-assets/` with its own `legal/` files, and, for a reviewed build,
   writes the image's `IMAGE_NOTICES.txt`.
2. **Build and verify.** One `cargo rustc` build embeds the notices; the
   build script refuses a directory prepared for another package, target or
   compiler, or whose input copies differ. The collector then requires that
   the build compiled only prepared crates with unchanged legal files, linked
   only the record's native inputs, imported only its system libraries,
   embedded exactly `LEGAL.txt`, and writes `inventory.json`.
3. **Source offer.** A reviewed release-profile build writes
   `THIRD_PARTY_SOURCE.tar.gz` with the compiled crates' and browser
   packages' sources, the inventory, the report, the fork records and the
   manual material.

A failed platform check prints the build's candidate record and the listing
its `noticesSha256` hashes; `--review-template` prints pending reviews of the
crates that lack one.

`--development` notices need the dependency reviews but no platform record,
so any host builds them. Their report and their executable carry
`UNREVIEWED DEVELOPMENT BUILD`: such a build is not distributable, and a
reviewed build's executable must not carry the marker.
