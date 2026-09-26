# CI / release control plane

Workflow YAML owns events, jobs, permissions, environments and the short
publication transactions. `mise` owns pinned tools and project commands.
Stdlib-only, type-checked Python in this directory owns trust decisions, GitHub
JSON validation and artifact verification.

The Bash left in no-checkout publication workflows is `gh`/Skopeo plumbing.
It stays in YAML because checking out repository scripts into a write-capable
job would weaken the publication boundary; `test_release_transaction.py` and
`test_trust.py` execute that shell and jq against fakes.

## Working on the pipeline

```sh
mise run toolchain-check   # pinned literals match mise.toml and mise.lock
mise run workflow-check    # workflow_policy.py
mise run pipeline-test     # ty type check, control-plane and legal tests
```

`mise run check` is the complete deterministic gate and `mise run ci` the full
local gate. CI splits `check` across jobs so nothing runs twice: `plan` runs
the workflow and pipeline checks, `core` runs `core-check` after
`legal-generate`, and `go` runs every Go test with `-race`. The policy fails if a
step of `mise run ci` has no CI job. `Gate` is the only required status.

The E2E suite serves a prebuilt Vite harness with `Bun.serve()` on an
OS-assigned loopback port while the fixture owns the real server. Go
integration tests pass pre-bound sockets into the production listener assembly
instead of probing for free ports.

## Releases

Stable releases and PR prereleases share one pipeline. The tag decides the
kind: `vMAJOR.MINOR.PATCH` builds current main, `vMAJOR.MINOR.PATCH-{alpha,
beta,rc}.N` builds the given head of a same-repository PR. Nothing is ever
triggered by a tag push, and `validate` mode never reaches a write.

1. **Untrusted build.** `release-request.yml` is a `workflow_dispatch` job on
   the default branch with `contents: read`, no secrets and no caches. Only
   `release.py prepare` sees the dispatch inputs. A stable build compiles and
   verifies the native archives, the third-party source archive and the OCI
   image from main; a prerelease builds only the image, which BuildKit fetches
   as the exact remote commit without a token. It uploads the request, the
   archives and their checksums.
2. **Trusted verification.** `release.yml` runs default-branch tooling on
   `workflow_run` and never executes the requested source. It accepts only a
   successful first attempt of the request workflow, dispatched by the owner
   from exact current main, with bounded artifacts. It checks the exact file
   set and checksums, verifies the image and archives as data, and requires
   either the main CI Gate and CodeQL for a stable release or, for a
   prerelease, an open PR containing current main with identical `.github`,
   `.githooks`, `scripts` and mise trees, its newest CI Gate and CodeQL check.
3. **Approval.** The `ghcr-release` environment gates every write. Afterwards
   the same trust is rechecked, and `_publish-oci.yml` repeats an API-only
   freshness check immediately before registry login.
4. **Publication.** The verified image digest is pushed to its exact version
   tag; stable releases then publish the verified GitHub Release and move the
   `major.minor` and `latest` aliases without regressing them.

Checking before and after approval is deliberate: the first avoids approving
invalid state, the second proves it still holds before the first irreversible
write. Handoffs are retained 35 days to cover the approval window; retention
does not extend trust because the later checks fail closed. GitHub's automatic
source archives provide the project source; a stable release adds the
third-party source archive and a source-availability note.

OCI builds request `provenance: mode=max`, pin the privileged binfmt image and
keep BuildKit's insecure entitlements disabled. Verification requires exactly
one runnable `linux/amd64` and `linux/arm64` manifest, each with one linked
provenance attestation, and copies every blob inside a network-less Skopeo
container whose only mount is the read-only archive. Build arguments carry no
secrets because max provenance records them.

## Workflow policy

GitHub repository settings decide which action packages may run and require
full-length SHAs. `workflow_policy.py` repeats only fast local invariants:

- external actions pinned to 40-character SHAs; no `ubuntu-latest`;
- no secrets, `pull_request_target`, `write-all` or interpolated `${{ }}` in
  run scripts; checkouts never persist credentials or select a caller ref;
- reviewed triggers per workflow, top-level permissions, a write-scope
  allowlist and an action allowlist for every release workflow;
- ordered invariants for approval, recheck, publication and freshness checks;
  dispatch inputs reach only the request validator and the build uses no cache;
- mise-provisioned tools, uncached trusted Python bootstraps and the pinned
  Chromium launch check;
- CI coverage of the local gate, path filters and no tracked key material.

`test_workflow_policy.py` mutates a copy of the repository once per rule.

When adding an external action, review it and its composite dependencies,
allow it in repository settings, pin the SHA with a version comment for
Dependabot and run the checks above.

## Pre-commit

The hook bootstraps the staged Python pin and runs the staged `precommit.py`.
It refuses commits to `main`, staged TLS paths or PEM material and files over
1 MiB, runs Gitleaks against the index, then materializes the exact staged tree
in a disposable worktree with frozen client dependencies and runs the component
gates selected by the changed, deleted or renamed paths. `api/`, `mise.toml` and
`mise.lock` changes select the full `check`.

## Python and dependencies

Python uses the exact patch release from `mise.toml` and the standard library
only. `mise run python-check` runs the pinned `ty` with warnings as errors. CI
installs the Bun lockfile frozen; `bun dedupe` output is advisory.
