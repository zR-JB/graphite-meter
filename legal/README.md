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

Each `rust-platform-*.json` file holds one reviewed record per Rust target of a build
environment: the linked native files beyond the target's sysroot rlibs (`nativeInputs`), the
package texts that cover them (`notices`, path to notice name), and the system libraries the
executable may import. The rlibs (`lib/rustlib/<target>/lib/*.rlib`) and the Rust
standard-library texts (`share/doc/rust/COPYRIGHT-library.html` and `licenses/*`) come from the
sysroot. `inputsSha256` is the SHA-256 of one `path<TAB>sha256` line per file of all four sets,
sorted by path. `scripts.legal.rust` reads the linker map of every Rust build and refuses a
target whose compiler, linked native files, imported libraries or inputs differ from its record;
the error prints this build's unreviewed record and the listing it hashes, and
`--review-template` writes them to `platform-candidate.json` and `platform-inputs.txt`. The
record keeps the reviewed native inputs this build did not link while they exist, such as import
libraries only unoptimized builds take, so re-approving it drops no fingerprinted input. Review
both, add each package text that covers the native files to `notices`, rerun with that record
for its digest, then approve it and commit it to the environment's file. Texts the environment
lacks, such as musl's and LLVM's for the static musl targets, are fetched from immutable
upstream revisions in `rust-notice-sources.json`. Downloaded and cached bytes must match
the pinned SHA-256 before entering the notices; cached files stay out of Git and Docker
contexts. Other missing package notices remain under `legal/manual`
and named by repository path. Each actual distributed build checks its linked inputs and prints
its facts if the review is stale. Rust releases support Linux and Windows; Go keeps its macOS targets.
PR checks use the pinned Debian release builder for Linux/Windows, with fat LTO disabled to reduce
compile time. Release requests use full release settings and no CI caches. The Fedora host record
covers local builds only; distributed artifacts always use their own builder's record.

The local tasks (`rust-server-run`, `rust-server-build`, `rust-client-run`, `rust-client-build`)
run `scripts.legal.rust --host`: they keep Cargo's ordinary host output paths and flags, read its
linker map, and select the host's `rust-platform-*.json` record by target and native compiler.
The same compiler, native-input, import and notice fingerprints used for release builds must match.
Fedora 44 x86-64 has a host record. A different host or changed
toolchain needs review; run the pipeline with `--host --review-template` to collect its candidate
and input listing, then review it as above. These tasks fail if the platform review is missing or stale.

Explicit `scripts.legal.rust --development` builds remain available on unreviewed hosts: they keep
every dependency review but omit the platform, and their notices open with `UNREVIEWED DEVELOPMENT
BUILD`, which their executables carry in plain text beside the compressed notices. The workflow
policy refuses the flag in any workflow, image build, task or shell script that CI or a release runs.
Release verification refuses the marker in each Rust source offer's notices, in each Rust TUI archive's
executable and `THIRD_PARTY_NOTICES.txt`, which must equal its offer's, and in each image's server and
notices.

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
