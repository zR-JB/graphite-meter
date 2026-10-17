# CI and release control plane

Workflow YAML owns events, jobs, permissions and environments; `mise` owns pinned tools and project commands.
Standard-library, type-checked Python in this directory owns trust decisions, GitHub JSON validation and artifact
verification. `publish.sh` holds the Skopeo registry writes and `release.py publish` the GitHub Release;
`test_release_transaction.py` runs both against a stateful fake GitHub, Docker and Skopeo, and `fixtures.py` fakes
the GitHub API, the checked-out commit and the container engine, so trust tests run the real commands.

```sh
mise run workflow-check    # jactionlint, zizmor, workflow_policy.py, tool pins
mise run pipeline-test     # ty type check, control-plane and legal tests
```

## CI

`Gate` is the only required status. A pull request that changes only documentation (`.github/ci-paths.yml`) runs the
checks job alone; every other change, and every push to main, runs every job:

| Job | Runs |
|---|---|
| Checks | workflow policy, tooling tests, Gitleaks, the browser client's checks |
| Rust checks | `rust-check`, `check_git_sources --verify`, the delayed-download gate, `rust-check-targets` |
| Rust checks on Windows | the client, net and legal crates' tests; a server build check |
| Image | the linux/amd64 + linux/arm64 image (ci profile) and its server source offers; both images verified with `scripts/verify-container.sh`, the arm64 one under QEMU |
| Client archives | the Linux and Windows client archives with release settings in the pinned builder; the amd64 client runs out of its archive |
| Release staging | `assets.py stage` and `assets.py check` on the two jobs above, as a release does |
| Browser E2E | Chrome in four shards and Firefox against the image's server; Chromium's HTTP/3 and WebTransport check (`rust/interop/browser.py`) |

`mise run ci` runs the same gates locally, and the policy fails if one has no CI job. Cargo caches are keyed by job,
toolchain and `Cargo.lock`; only CI writes caches. Setup installs the Rust release that `rust/rust-toolchain.toml` pins, with
`--no-self-update`, and refuses it unless rustup installed it from the channel manifest whose SHA-256 `mise.toml` pins
as `rust_manifest_sha256`. `advisories.yml` checks the locked crates and client packages against the live advisory
databases daily, so a new advisory never fails an unrelated pull request.

## Releases

```sh
gh workflow run release-request.yml --ref main -f tag=v1.2.3 -f mode=publish
```

A stable release publishes the image as `X.Y.Z`, moves `X.Y` and `latest`, and creates a GitHub Release with five
assets: the client archives for Linux amd64, Linux arm64 and Windows amd64, one third-party source archive for every
shipped build, and `checksums.txt`. A prerelease takes a `vX.Y.Z-{alpha,beta,rc}.N` tag and `-f sha=<main head>`, or
`-f pr=N -f sha=<PR head>` to build an open same-repository PR; it publishes only the image of that version, such as
`X.Y.Z-rc.N`, which no alias follows. `mode=validate` stops before any write. Approve the `ghcr-release` deployment
when the Release run asks.

1. Untrusted build. `release-request.yml` is one `workflow_dispatch` job on main with `contents: read`, no secrets
   and no caches. Only `release.py prepare` sees the inputs, which GitHub also renders into the run title. The pinned
   builder builds the image from main or, for a PR, from the exact commit BuildKit fetches without a token; a stable
   build then exports the server's source offers and the client archives from the same fresh builder. Before its
   reviewed builds the builder trains a PGO profile per architecture on loopback transfers, amd64 natively and arm64
   under QEMU's user mode (`scripts/pgo.py`). `assets.py stage` copies the release files and merges every build's
   part of the source offer into one archive; the job runs the amd64 client and uploads one artifact.
2. Trusted verification. `release.yml` runs main's tooling on `workflow_run` for main dispatches only and never
   executes the requested source. It binds `request.json` to the run title, the owner, the first attempt and the one
   bounded artifact its one job wrote. It verifies the image, the archives and the source offer as data, and requires
   every main CI job and CodeQL for a release of main or, for a PR prerelease, an open PR containing current main with
   identical `.github`, `.githooks`, `scripts` and mise trees, its newest CI Gate and CodeQL check. A stable tag that
   already has a published Release must carry exactly its assets. Publish mode also requires `ghcr-release` to have
   reviewers and main-only deployments.
3. Approved publication. One `ghcr-release` job holds the only write credentials. It rechecks the handoff digests
   and all trust above and pushes the verified digest to its exact version tag. For a stable release it publishes the
   GitHub Release and points `X.Y` and `latest` at the highest published releases, which also repairs aliases a
   cancelled run left behind.

The default `GITHUB_TOKEN` has no write scope in any workflow. Handoffs are retained 35 days to cover the approval
window; the recheck fails closed. A prerelease image names the repository in `SOURCE.txt` and the built commit in its
`revision` label; that commit's lockfiles pin the third-party source it was built from.

Image builds request `provenance: mode=max` and keep BuildKit's insecure entitlements disabled. The Dockerfile may not
select a custom frontend, pins its base images by digest and installs exact apt package versions only from one
snapshot.debian.org timestamp; `toolchain-sync` keeps its image literals equal to `mise.toml`'s. Verification requires
one runnable `linux/amd64` and `linux/arm64` manifest, each with one linked SLSA provenance statement whose source is
the release commit and whose Dockerfile and target are `container/Dockerfile` and `server`; each image's server and
notices must be the reviewed build its source offer names, and nothing may carry `UNREVIEWED DEVELOPMENT BUILD`.
Skopeo copies every blob inside a network-less container whose only mount is the read-only archive. The untrusted
build writes the provenance, so it shows which source was built but does not authenticate it. Build arguments carry no
secrets because max provenance records them.

### Owner setup

1. Actions, General: workflow permissions *Read repository contents and packages*; artifact retention at least
   35 days; if actions are allow-listed, add `actions/create-github-app-token`.
2. Environment `ghcr-release`: required reviewer = owner; deployment branches = selected branch `main` only, no
   tags. Its secrets and variables are the only release credentials; define none at repository level.
3. `GHCR_TOKEN` (environment secret): a classic personal access token of the owner with only `write:packages`
   (clear the preselected `repo`), with an expiry and a rotation reminder. GHCR accepts no App or fine-grained token.
4. Release App: a GitHub App owned by the owner, webhook off, repository permission *Contents: read and write*
   only, installed on this repository only. Store its client ID as environment variable `RELEASE_APP_CLIENT_ID` and a
   private key as environment secret `RELEASE_APP_PRIVATE_KEY`.
5. GHCR package, Manage Actions access: give this repository *Read*, not *Write* or *Admin*, so no workflow token
   can push images.
6. Rulesets: tags `refs/tags/v*` restrict creation, update and deletion with the Release App as the only bypass
   actor; `main` requires pull requests, the `Gate` and CodeQL checks and blocks force pushes and deletion.
7. Releases: enable release immutability.

## Workflow policy

`jactionlint` (`.github/jactionlint.yaml`) checks workflow syntax, expressions and parallel steps, plus its default
security and policy rules. `zizmor` (`.github/zizmor.yml`) requires full-SHA action pins, non-persisted checkout
credentials and no dangerous triggers other than the reviewed `workflow_run`. `workflow_policy.py` holds the project's
own trust rules, and `test_workflow_policy.py` breaks a copy of the repository once per rule: pinned runner images,
secrets only in the approved publish job, release identity only from the run context, no development build anywhere
CI or a release reaches (`--development` or `scripts.build`), a code owner for every dependency manifest, lockfile,
toolchain pin, Cargo configuration and build script, and a Dependabot entry for every lockfile.

When adding an external action, review it and its composite dependencies, allow it in repository settings, pin the SHA
with a version comment for Dependabot and run the checks above.

## Pre-commit

The hook refuses commits to `main` and whitespace errors, and scans the index with the pinned Gitleaks. `precommit.py`
selects the mise checks for the staged paths, counting both sides of a rename: `api/`, `mise.toml` and `mise.lock`
select the full `check`; workflows, `.githooks/`, scripts, the container files and `legal/` select `workflow-check`
and `pipeline-test`; `client/` and the sign-in assets it generates (`rust/server/src/auth/assets/`) select
`client-ci`. CI runs the Rust checks. The checks run on the exact staged tree in a disposable worktree with the
client's frozen dependencies.