# CI / release control plane

This directory is Graphite Meter's dependency-free GitHub Actions policy and
provenance layer. The intended split is deliberately small:

- workflow YAML owns events, jobs, dependencies, permissions, environments, and
  short publication transactions;
- `just` owns project build/test commands;
- typed, stdlib-only Python in `scripts/ci/` owns trust decisions, GitHub API
  shape validation, provenance checks, and artifact verification.

There are no standalone CI shell scripts in this directory. The small Bash
blocks that remain in no-checkout publication workflows are transaction plumbing
for `gh`/Skopeo and intentionally stay in YAML: moving them into embedded Python
would not make them type-checkable, while checking out repository scripts into a
write-capable job would weaken the publication boundary.

## JSON and typing boundary

`github_api.py` is the single GitHub JSON decoding boundary. `json.loads()` is
narrowed into recursive JSON types and security-sensitive callers validate
objects, arrays, strings, integers, and optional values before policy code uses
them. The control plane intentionally has no Pydantic or other PyPI runtime
dependency.

All Python functions are required by the regression suite to annotate every
parameter and return value, and `type: ignore` is rejected. `pipeline-test` also
compiles every `scripts/ci/*.py` module before running tests.

## Trust boundaries

### Normal CI

`ci.yml` runs pull-request and main-push checks with read-only repository access.
The path plan comes from `.github/ci-paths.yml`; project operations use named
`just` recipes. `Gate` is the single ordinary branch-protection status.

The browser/server transport E2E suite does not keep a Vite development server
alive beside the test runner. Vite builds the small transport harness once into
`client/.e2e-dist`, and `Bun.serve()` serves those immutable assets on an
OS-assigned loopback port. Bun.WebView drives the pinned Linux Chromium while
the fixture owns the real Graphite Meter backend process. The Go server integration tests use an
internal socket-provider seam so tests hand already-bound TCP/UDP sockets into
the production listener assembly path; they never probe a free port, release it,
and race to bind the same number later.

### PR prereleases

1. `prerelease-request.yml` is a **low-authority producer**. Manual
   `workflow_dispatch` can be pointed at a selected ref, so the candidate job is
   gated to the repository default branch and has only `contents: read`, no
   secrets/write permission, no shared dependency caches, and no publication path.
   It checks out only trusted request/build tooling plus the trusted `.bun-version`.
   The raw dispatch SHA reaches only `prerelease.py request-prepare`; after exact
   40-character validation, the trusted OCI action gives that sanitized SHA to
   BuildKit as a public remote Git context. PR files never exist in the runner
   workspace, and the Docker build receives no `github.token`.
2. `prerelease-publish.yml` is the trusted default-branch `workflow_run` consumer.
   It accepts a producer run only when GitHub's API proves that the exact run came
   from `prerelease-request.yml`, was attempt 1, was dispatched from `main` at the
   exact current-main SHA, completed successfully, and was initiated by the
   repository owner. A selected stale/feature ref can therefore build only
   low-authority data; it can never authorize publication.
3. The publisher never checks out PR source. On a fresh runner it downloads the
   candidate artifact as untrusted data, checks the exact file set/checksum, binds
   its metadata to the open same-repository PR, requires that PR to contain exact
   current main, requires the complete CI/release control plane to be byte-identical
   to that main SHA, then requires the newest exact PR CI Gate and PR-bound CodeQL
   check before environment approval.
4. After approval, PR head/current-main/Gate/CodeQL are rechecked. Any main advance
   or PR/CI/CodeQL change requires a fresh prerelease request. `_publish-oci.yml`
   performs one final API-only source/CI/CodeQL freshness check immediately before
   registry authentication while still executing no repository code.
5. The old label-triggered candidate hop is intentionally absent. GitHub suppresses
   most workflow runs caused by events created with the repository `GITHUB_TOKEN`,
   including `pull_request:labeled`, so publication does not depend on a
   workflow-created label triggering another workflow.

The pre-approval and post-approval checks are intentional, not duplicate policy:
first avoid asking a human to approve already-invalid state; then prove that the
mutable state still holds immediately before the first irreversible write. Trusted
publication handoffs are retained for 35 days so the artifact lifetime covers the
platform's approval/wait lifetime; repository/organization Actions retention must
therefore allow at least 35 days. Extending artifact storage does not extend trust,
because the post-approval and last-mile checks still fail closed on stale state.

### Stable releases

Stable publication is never tag-push triggered and the write-capable release
workflow is **not** directly manually dispatched. `release-request.yml` is a
zero-write manual request workflow. Because GitHub allows a manual dispatch to
select a branch/tag, that request is treated as untrusted input and only uploads a
small request artifact.

`release.yml` is a trusted default-branch `workflow_run` consumer of `Request
stable release`. Its guard accepts the request only when GitHub proves the request
run was attempt 1, owner-initiated, dispatched from `main`, completed successfully,
and has the exact same SHA as the current default-branch consumer. The immutable
release source thereafter is `${{ github.sha }}` directly; no guard job output is
reused as a second source identity.

The request has two modes:

- `validate`: full build/verification, zero publication;
- `publish`: same build, protected-environment approval, then a fresh current-main
  + Gate + CodeQL recheck before write-capable jobs.

Before any write-capable job, the release guard also preflights a pre-existing
`vMAJOR.MINOR.PATCH` ref and refuses if it targets a different commit. Publication
order is exact version OCI image → verified draft GitHub Release → monotonic
`major.minor`/`latest` OCI aliases. `_publish-release.yml` uses GitHub's CLI/API
directly, verifies the release target SHA and GitHub-recorded asset SHA-256
digests, and does not run third-party release code with `contents: write`. Native
release archives are rejected for traversal paths, links/devices, duplicate
members, unexpected files, or missing checksums. Stable releases do not duplicate
Graphite Meter's own tagged repository tree inside a custom source asset: GitHub's
automatic `Source code (zip)` / `Source code (tar.gz)` links provide the project
source for the release tag, while the verified
`graphite-meter_VERSION_third-party-source.tar.gz` asset carries the external
Go/npm/manual source material and legal/provenance inventories. The publisher
prepends an explicit source-availability notice to generated release notes and
fails closed on retries if that notice is missing or stale. `_promote-oci.yml`
globally serializes alias movement and rejects SemVer rollback.

The OCI build explicitly requests BuildKit `provenance: mode=max`. The privileged
`binfmt` image launched by `setup-qemu-action` is pinned by digest rather than
accepting that action's floating `latest` default. `setup-buildx-action` also gets
an explicit benign daemon flag, which replaces its default
`security.insecure`/`network.host` entitlements; this build needs neither.
Verification requires an OCI v1 index with exactly one runnable `linux/amd64` and `linux/arm64`
manifest plus exactly one correctly linked BuildKit attestation manifest for each
runnable digest, then forces the pinned immutable Skopeo container to copy `--all` into its
own ephemeral filesystem so every referenced manifest/blob must be readable. The
archive is the verifier container's only host bind mount, it is read-only, and
local verification runs with container networking disabled. This avoids both
root-owned verifier output on the runner and unnecessary verifier network access.
Build arguments contain no secrets; max-level provenance would make build-argument
values observable.

Stable native artifacts are verified immediately after the exact release payload
is built, before the more expensive multi-platform OCI build. The verifier checks
the built server's embedded version through `graphite-meter --version`; it does
not boot a fixed-port HTTP server merely to read `/preflight`. The stable workflow
also does not rebuild a second representative `release-check` payload after the
exact artifacts already exist.

## Policy and tests

`just workflow-check` runs `workflow_policy.py`. It deliberately does **not**
mirror GitHub's allowed-action package list or maintain a second action SHA
database. GitHub repository settings are the authority for which external action
packages may execute. The local checker only enforces the zero-maintenance
property that every external `uses:` reference is an immutable 40-character
commit SHA, plus the pipeline-specific trust boundaries.

The ordinary release-package CI job also executes the exact digest-pinned immutable
Skopeo container's `--version` contract through the same typed parser used by OCI
verification, so a CLI output/digest-version mismatch is caught on a workflow PR
instead of at first publication.

Other checked invariants include:

- explicit `ubuntu-24.04` runner-major labels instead of floating `ubuntu-latest`;
- explicit max-level OCI provenance with no GitHub-secret references in the build action;
- digest-pinned privileged QEMU/binfmt input and no BuildKit insecure daemon entitlements;
- low-authority prerelease producer permissions, default-branch job guard, and no PR checkout on the runner;
- no repository checkout/code execution in isolated publication workflows;
- exact-tag and stable-alias concurrency guards;
- zero-write stable manual request plus trusted default-branch consumer, never tag-push/direct write-capable dispatch;
- protected-environment approval before the final mutable trust recheck;
- GitHub Release target/asset-digest verification;
- local OCI verification with no network and no writable host output mount;
- static, dynamically served browser E2E harness instead of a fixed-port dev server;
- exact staged-tree pre-commit checks, including staged deletions/renames;
- no tracked certificate/private-key material.

`just pipeline-test` compiles all control-plane Python and runs dependency-free
positive and negative regression tests. The Git hook is only a two-line launcher
for `scripts/ci/precommit.py`; Just no longer contains a second hook implementation.
The typed hook records the exact staged Git tree, materializes that tree in a
disposable detached worktree, and runs selected component checks there. Deleted
paths and the old side of renames participate in check planning, so removing or
renaming a workflow cannot skip pipeline validation. Gitleaks still scans the
staged index directly. This prevents an unstaged local fix from making a broken
staged commit appear healthy while keeping ignored dependency/tool directories
out of Git history.

## External action maintenance

GitHub Settings and workflow YAML each have one non-overlapping responsibility:

1. GitHub **Actions permissions** allow only the external action packages the
   repository intends to trust.
2. GitHub **Require actions to be pinned to a full-length commit SHA** enforces
   immutable refs on the platform.
3. Workflow `uses:` lines keep the exact 40-character SHA plus a same-line version
   comment so Dependabot can update them normally.
4. `workflow_policy.py` repeats only the SHA *shape* check locally for fast
   pre-commit/CI feedback; there is no package/SHA allowlist file to maintain.

When adding a new external action package, review it **and any composite-action
dependencies it invokes**, add every required package pattern in repository
Settings, use an exact 40-character SHA in YAML, then run `just workflow-check`
and `just pipeline-test`. The current `extractions/setup-just` pin invokes the
separately allowlisted `extractions/setup-crate` package from its pinned composite
action definition.
