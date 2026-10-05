# CI / release control plane

Workflow YAML owns events, jobs, permissions and environments. `mise` owns
pinned tools and project commands. Stdlib-only, type-checked Python in this
directory owns trust decisions, GitHub JSON validation and artifact
verification. `publish.sh` holds the Skopeo registry writes and `release.py
publish` the GitHub Release; `test_release_transaction.py` runs both against a
stateful fake GitHub, Docker and Skopeo. `fixtures.py` fakes the GitHub API by exact path and
pagination, the checked-out commit and the container engine, so trust tests run the real
commands.

## Working on the pipeline

```sh
mise run workflow-check    # actionlint, zizmor, workflow_policy.py, tool pins
mise run pipeline-test     # ty type check, control-plane and legal tests
```

`mise run check` is the deterministic developer gate; `mise run ci` runs every
CI job's task locally, and the policy fails if one of its steps has no CI job.
`Gate` is the only required status. Path filters (`.github/ci-paths.yml`)
narrow PR runs only; every push to main runs every job. Each job runs on the
filter `workflow_policy.py` names for it, and a filter never selects a job
without the jobs it needs, which the Gate would pass as skipped.

## Releases

```sh
gh workflow run release-request.yml --ref main -f tag=v1.2.3 -f mode=publish
```

A prerelease adds `-f pr=N -f sha=<PR head>` to a `vX.Y.Z-{alpha,beta,rc}.N`
tag; `mode=validate` stops before any write. A stable release adds the
experimental Rust image and TUI archives with `-f rust=true`; Go's image and
archives stay the default and are always released. Approve the `ghcr-release`
deployment when the Release run asks.

1. **Untrusted build.** `release-request.yml` is a `workflow_dispatch` job on
   main with `contents: read`, no secrets and no caches. Only `release.py
   prepare` sees the inputs, which GitHub also renders into the run title. A
   stable build checks the committed legal outputs, stamps the version and
   builds the native archives, the third-party source archive and the OCI
   image from main; a prerelease builds only the image, which BuildKit fetches
   as the exact remote commit without a token. With `rust`, two more jobs
   build main with the pinned Rust builder, without caches: one the
   linux/amd64 + linux/arm64 image and the server source offers, one the
   Linux and Windows TUI archives with their source offers, which it checks by
   running the amd64 TUI. `rust_release.py` stages exactly the release files
   out of each export. Rust builds have no macOS archive, and prereleases
   have no Rust build: only a GitHub Release carries their source offers.
2. **Trusted verification.** `release.yml` runs main's tooling on
   `workflow_run` for main dispatches only and never executes the requested
   source. It binds `request.json` to the run title, the owner, the first
   attempt and bounded artifacts, each written by the request job that
   `release.py` names for it while that job ran, so no other job of the run
   can supply it; it verifies the images and archives as data, the Rust ones
   as CI's `rust-release` job does, and
   requires either every main CI job and CodeQL for a stable release or, for a
   prerelease, an open PR containing current main with identical `.github`,
   `.githooks`, `scripts` and mise trees, its newest CI Gate and CodeQL check.
   Publish mode also requires `ghcr-release` to have reviewers and main-only
   deployments.
3. **Approved publication.** One `ghcr-release` job holds the only write
   credentials. It rechecks the handoff digests and all trust above, pushes the
   verified digest to its exact version tag, and the Rust image's to
   `VERSION-rust`. For a stable release it publishes the GitHub Release and
   points the `major.minor` and `latest` aliases at the highest published
   releases, which also repairs aliases a cancelled run left behind; after a
   Rust release, `major.minor-rust` and `latest-rust` follow the highest
   published releases that shipped a Rust server source offer.

The default `GITHUB_TOKEN` has no write scope in any workflow. Handoffs are
retained 35 days to cover the approval window; the recheck fails closed.
GitHub's automatic source archives provide the project source; a stable
release adds the third-party source archive, each Rust build's source offer
and a source-availability note naming them. A prerelease publishes only its
image and creates no GitHub Release.

OCI builds request `provenance: mode=max`, pin the privileged binfmt image and
keep BuildKit's insecure entitlements disabled. Neither Dockerfile may select a
custom frontend; both pin their base images by digest and install exact apt
package versions only from one snapshot.debian.org timestamp, and
`toolchain-sync` keeps their image literals equal to `mise.toml`'s. Verification requires one runnable `linux/amd64` and
`linux/arm64` manifest, each with one linked SLSA provenance statement whose
source (the fetched commit, or the local checkout's revision) is the release
commit of this repository, requires each image's layers to ship the server and
its `THIRD_PARTY_NOTICES.txt` with no copy carrying `UNREVIEWED DEVELOPMENT
BUILD`, and copies every blob inside a network-less Skopeo
container whose only mount is the read-only archive. The untrusted build writes
that provenance, so it shows which source was built but does not authenticate
it. Build arguments carry no secrets because max provenance records them.

### Owner setup

1. **Actions → General:** workflow permissions *Read repository contents and
   packages*; artifact retention at least 35 days; if actions are
   allow-listed, add `actions/create-github-app-token`.
2. **Environment `ghcr-release`:** required reviewer = owner; deployment
   branches = selected branch `main` only, no tags. Its secrets and variables
   are the only release credentials; define none at repository level.
3. **`GHCR_TOKEN` (environment secret):** a classic personal access token of
   the owner with only `write:packages` (clear the preselected `repo`), with an
   expiry and a rotation reminder. GHCR accepts no App or fine-grained token.
4. **Release App:** a GitHub App owned by the owner, webhook off, repository
   permission *Contents: read and write* only, installed on this repository
   only. Store its client ID as environment variable `RELEASE_APP_CLIENT_ID`
   and a private key as environment secret `RELEASE_APP_PRIVATE_KEY`.
5. **GHCR package → Manage Actions access:** give this repository *Read*, not
   *Write* or *Admin*, so no workflow token can push images.
6. **Rulesets:** tags `refs/tags/v*` restrict creation, update and deletion with
   the Release App as the only bypass actor; `main` requires pull requests, the
   `Gate` and CodeQL checks and blocks force pushes and deletion.
7. **Releases:** enable release immutability.

## Workflow policy

`actionlint` checks workflow syntax and expressions. `zizmor` (configured in
`.github/zizmor.yml`) requires full-SHA action pins, non-persisted checkout
credentials and no dangerous triggers other than the reviewed `workflow_run`.
`workflow_policy.py` holds the project's own trust rules, and
`test_workflow_policy.py` breaks a copy of the repository once per rule. No
workflow or image build, nor a mise task or `scripts/*.sh` one of them runs,
may build with unreviewed development notices (`--development` or
`scripts.rust_build`).

When adding an external action, review it and its composite dependencies,
allow it in repository settings, pin the SHA with a version comment for
Dependabot and run the checks above.

## Pre-commit

The hook refuses commits to `main` and whitespace errors, and scans the index
with the pinned Gitleaks. `precommit.py` selects the mise checks for the staged
paths, counting both sides of a rename; `api/`, `mise.toml` and `mise.lock`
select the full `check`. The checks run on the exact staged tree in a disposable
worktree with frozen client dependencies. `workflow-check` refuses tracked TLS
key and certificate names and PEM material.

## Python and dependencies

Python uses the exact patch release from `mise.toml` and the standard library
only. `mise run python-check` runs the pinned `ty` with warnings as errors. CI
installs the Bun lockfile frozen; `bun dedupe` output is advisory.

## Rust

The `rust` job runs `mise run rust-check`, verifies the Cargo fork pins with
`check_git_sources --verify` and runs `rust-check-targets`. `rust-image`
builds the image once per architecture and exports its server source offers;
`rust-tui` builds the TUI archives and their source offers with release
settings; `rust-release` stages both with `rust_release.py` and verifies them
as a release does, without running them: the image as above, each source
offer's inventory, notices and files against the checkout, each archive's
layout, executable format and notices, and every `SOURCE.txt` against the
offer it names. Setup
installs the toolchain `rust/rust-toolchain.toml` pins with `--no-self-update`
and refuses it unless rustup installed it from the channel manifest whose
SHA-256 `mise.toml` pins as `rust_manifest_sha256`; a new channel needs the
SHA-256 of its `channel-rust-<version>.toml`. Shipped platforms, their Rust
targets and the platform record live in `[workspace.metadata.graphite-meter]`
of `rust/Cargo.toml`, which tooling reads through `rust_workspace.py`. Cargo
caches are keyed by job, toolchain and `Cargo.lock`, so a new lockfile builds
once from scratch.

The `rust` filter selects the workspace checks and the Windows client tests and
covers every file a Rust source includes; `rust-interop` selects the
interoperability and perf jobs, which also build Go; `rust-image` covers every
input of `container/Dockerfile.rust` for the image and its browser suite;
`rust-release` covers every stage but the browser app's for the TUI archives
and the staging check.
