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

## Rust platform records

The release builder's `rust-platform-debian-bookworm.json` holds one reviewed record per
distributed Rust target: native files beyond known sysroot rlibs (`nativeInputs`), package texts
that cover them (`notices`, path to notice name), and allowed system-library imports.
Every distributed build checks its actual linker map and imports against these allowlists.
The standard-library aggregate (`share/doc/rust/COPYRIGHT-library.html`) and every text in
`licenses/*` come from the actual sysroot. `noticesSha256` hashes one `path<TAB>sha256` line
per standard-library or additional notice text, sorted by path. Changed, added or missing
notice texts require review. Object/archive bytes and full compiler version strings are not
separate legal approvals; pinned toolchain archives, builder images and native package versions
establish provenance. These checks provide assurance that the shipped notices cover the build;
the underlying licenses govern attribution, notice preservation and source provision, not our
particular fingerprint scheme.

`scripts.legal.rust --target TARGET --review-template` writes `platform-candidate.json` and
`platform-inputs.txt`: observed native/import facts and the canonical notice listing. Review
the contributing sources and their terms, add their covering texts to `notices`, then approve
the resulting record. Previously approved native paths remain covered while present, including
import libraries used only by unoptimized builds. Texts the builder lacks, such as musl's and
LLVM's, come from immutable upstream sources in `rust-notice-sources.json`; downloaded and
cached bytes must match their pinned SHA-256. Its `rustVersion` must match the pinned toolchain
when external runtime notices are used, so a Rust upgrade also requires reviewing those sources.
Cached texts stay out of Git and Docker contexts. Rust releases support Linux and Windows;
Go keeps its macOS targets. PR package checks disable fat LTO; release requests retain full
release settings and omit CI caches.

The local tasks (`rust-server-run`, `rust-server-build`, `rust-client-run`, `rust-client-build`)
call `scripts.legal.rust --development --local`. They retain dependency and browser reviews,
ordinary Cargo host output paths and the selected profile, but require no host platform approval
and omit source archives. Their notices omit unreviewed host runtime texts and open with
`UNREVIEWED DEVELOPMENT BUILD`; the executable carries that marker beside its compressed notices.
The optimized local `prod` task remains a development build for distribution purposes.
Workflow policy refuses the development flag in workflow, image and release command chains.
Release verification refuses the marker in each Rust source offer's notices, in each Rust TUI archive's
executable and `THIRD_PARTY_NOTICES.txt`, which must equal its offer's, and in each image's server and
notices.

The collector prepares notices from the locked normal/build dependency graph for the target and
host, then compiles the final binary once. This conservative inventory may include uncompiled
dependencies; after compilation every actual component must be covered and its source and legal
facts must agree. The exported inventory and dependency-source archive describe actual compiled
inputs, including build scripts and procedural macros. Final checks also verify native/import
coverage and the embedded notice bytes. The reviewed-distribution guarantee belongs to this
collector and final packaging verification; manually supplying private `GM_RUST_LEGAL_DIR` to
Cargo is not a substitute. Registry version updates may reuse a same-name/source review only
while license expressions, modification status and complete legal-file fingerprints agree.
Git forks retain exact revision reviews. Unused approvals may remain; the Linux crate-count limit
is a separate project dependency policy.

About uses browser URLs separately from Cargo's package-source identities, which remain in the
inventories for review. Crates link to their published version; Git dependencies link to their
pinned revision. Forks also link to the reviewed upstream revision and comparison. Patched npm
packages link to the local patch at the build's release tag or Git revision, or HEAD for an
unstamped development build. A Rust engine stamped with its Git revision uses that revision
for its notices and source links too. Go and Rust use the same link labels and generator.

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

## Updating pinned forks

Carry Graphite Meter patches onto a reviewed stable upstream release. Review the upstream
base, patch origins, changed package/file scope and licenses; update `rust/Cargo.toml`,
`Cargo.lock` and `legal/rust-forks.json`, then run `legal-check`,
`python3 -m scripts.legal.check_git_sources --verify` and the Rust checks. The verifier
requires exact commit pins, ancestry and a change set matching the reviewed fingerprint.
