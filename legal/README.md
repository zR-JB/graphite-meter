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
lacks, such as musl's and LLVM's for the static musl targets, are committed under `legal/manual`
and named by repository path. macOS records come from a macOS runner, the only environment with
Apple's SDK: CI's `rust-darwin` job runs the release request's `rust-darwin-package` task on the
same runner image and Xcode and prints this build's record when the committed one goes stale.

The development tasks (`rust-server-run`, `rust-server-build`, `rust-client-run`, `rust-client-build`)
run `scripts.legal.rust --development` instead: it keeps every dependency review but reads no platform
record or toolchain facts, so it works on any host, and its notices open with `UNREVIEWED DEVELOPMENT
BUILD`. The workflow policy refuses the flag in any workflow, image build, task or shell script that CI
or a release runs. Release verification refuses the marker in each Rust source offer's notices, in each
Rust TUI archive's `THIRD_PARTY_NOTICES.txt`, which must equal its offer's, and in each image's; it cannot
read the copy compressed into each executable, which the same legal build wrote.

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

## Pinned fork upkeep

Forks carry Graphite Meter patches only on the latest stable upstream release: a tag with the
`baseTag` prefix and a plain `MAJOR.MINOR.PATCH` version, never a branch head or pre-release
tag. Each protected patch branch is `graphite-meter/<crate>-v<version>`.

After Rust lands on the default branch, enable graphite-meter's daily/manual `Fork upkeep`.
It reads `rust-forks.json` and keeps fork default branches as pure upstream mirrors. Ordinary
Git pushes reject divergence, including concurrent changes. When a newer stable release exists,
upkeep rebases the canonical patch onto it as `<patch-branch>-next` and opens an issue; pending
proposals are never replaced, and conflicts or merge-only edits also open an issue. Canonical
branches remain protected. Integrate a reviewed proposal as the new versioned protected branch
and update the inventory mapping.

Upkeep uses two GitHub Apps so the credential that writes forks cannot write this repository:

- Fork upkeep App, installed only on the forks: Contents, Issues and Workflows write
  (Workflows because mirrors and proposals carry upstream workflow changes). Set repository
  variable `FORK_UPKEEP_APP_CLIENT_ID` and secret `FORK_UPKEEP_APP_PRIVATE_KEY`.
- Fork pin App, installed only on graphite-meter: Contents and Pull requests write, no Workflows.
  Set repository variable `FORK_PIN_APP_CLIENT_ID` and secret `FORK_PIN_APP_PRIVATE_KEY`.

Without both variables or a landed Rust inventory nothing is published; dispatch accepts only
the default branch. No fork Actions setup is needed.

Canonical updates get draft Cargo.toml pin PRs on SHA-specific branches. Existing proposals
and reviewer edits are never overwritten. Lockfiles and reviewed provenance stay unchanged,
so existing gates block the drafts. Review origins, diffs, package/file scope, licenses and
budgets; update `rust-forks.json` (each commit's subject, purpose and origin), Cargo.lock
and generated legal outputs; then run
`python3 -m scripts.legal.check_git_sources --verify` and the Rust/fork gates before approval.
Upkeep never reviews sources, merges PRs or changes protections.

`python3 -m scripts.ci.fork_upkeep` previews remote changes without publishing.
