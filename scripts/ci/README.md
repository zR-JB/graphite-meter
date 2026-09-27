# CI / release control plane

Workflow YAML owns events, jobs, permissions and environments. `mise` owns
pinned tools and project commands. Stdlib-only, type-checked Python in this
directory owns trust decisions, GitHub JSON validation and artifact
verification. `publish.sh` holds the Skopeo registry writes and `release.py
publish` the GitHub Release; `test_release_transaction.py` runs both against a
stateful fake GitHub, Docker and Skopeo. `fixtures.py` fakes `gh` by exact API path and pagination, the
checked-out commit and the container engine, so trust tests run the real
commands.

## Working on the pipeline

```sh
mise run workflow-check    # actionlint, zizmor, workflow_policy.py, tool pins
mise run pipeline-test     # ty type check, control-plane and legal tests
```

`mise run check` is the deterministic developer gate; `mise run ci` runs every
CI job's task locally, and the policy fails if one of its steps has no CI job.
`Gate` is the only required status. Path filters (`.github/ci-paths.yml`)
narrow PR runs only; every push to main runs every job.

| Job | mise task |
| --- | --- |
| `tooling` | `workflow-check`, `pipeline-test` |
| `core` | `legal-check`, `core-check` |
| `go` | `server-race` |
| `e2e` | `e2e` ([real-server fleet](../../docs/DEVELOPMENT.md#tests)) |
| `smoke` | `container-smoke` |
| `release` | `release-check` |
| `security` | `security`, `client-audit` |
| `secret-scan` | `secret-scan-ci` |

## Releases

```sh
gh workflow run release-request.yml --ref main -f tag=v1.2.3 -f mode=publish
```

A prerelease adds `-f pr=N -f sha=<PR head>` to a `vX.Y.Z-{alpha,beta,rc}.N`
tag; `mode=validate` stops before any write. Approve the `ghcr-release`
deployment when the Release run asks.

1. **Untrusted build.** `release-request.yml` is a `workflow_dispatch` job on
   main with `contents: read`, no secrets and no caches. Only `release.py
   prepare` sees the inputs, which GitHub also renders into the run title. A
   stable build checks the committed legal outputs, stamps the version and
   builds the native archives, the third-party source archive and the OCI
   image from main; a prerelease builds only the image, which BuildKit fetches
   as the exact remote commit without a token.
2. **Trusted verification.** `release.yml` runs main's tooling on
   `workflow_run` for main dispatches only and never executes the requested
   source. It binds `request.json` to the run title, the owner, the first
   attempt and bounded artifacts, verifies the image and archives as data, and
   requires either every main CI job and CodeQL for a stable release or, for a
   prerelease, an open PR containing current main with identical `.github`,
   `.githooks`, `scripts` and mise trees, its newest CI Gate and CodeQL check.
   Publish mode also requires `ghcr-release` to have reviewers and main-only
   deployments.
3. **Approved publication.** One `ghcr-release` job holds the only write
   credentials. It rechecks the handoff digests and all trust above, pushes the
   verified digest to its exact version tag, and for a stable release
   publishes the GitHub Release and points the `major.minor` and `latest`
   aliases at the highest published releases, which also repairs aliases a
   cancelled run left behind.

The default `GITHUB_TOKEN` has no write scope in any workflow. Handoffs are
retained 35 days to cover the approval window; the recheck fails closed.
GitHub's automatic source archives provide the project source; a stable
release adds the third-party source archive and a source-availability note.

OCI builds request `provenance: mode=max`, pin the privileged binfmt image and
keep BuildKit's insecure entitlements disabled. The Dockerfile may not select a
custom frontend. Verification requires one runnable `linux/amd64` and
`linux/arm64` manifest, each with one linked SLSA provenance statement whose
source (the fetched commit, or the local checkout's revision) is the release
commit of this repository, and copies every blob inside a network-less Skopeo
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
`test_workflow_policy.py` breaks a copy of the repository once per rule.

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
