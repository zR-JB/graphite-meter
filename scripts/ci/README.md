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

### PR prereleases

1. `prerelease-request.yml` runs from trusted `main`. It validates the exact
   same-repository PR head, current-main ancestry, and that the CI orchestration
   defining Gate (`ci.yml`, path plan, setup action, `justfile`, and `scripts/ci/*.py`)
   is byte-identical to current main. It then requires the newest exact CI Gate
   and CodeQL result before creating a one-use request label/artifact.
2. `prerelease-candidate.yml` treats the PR checkout as build payload. Canonical
   helper copies are checked out separately from the PR base into `.trusted-ci`
   to reduce PR-controlled pipeline configuration. The runner is still low trust:
   no candidate-side file becomes a publication authority after PR code executes.
   The job has no secrets/write permission and disables shared Go/Bun caches.
3. `prerelease-publish.yml` is the trusted `workflow_run` consumer. It treats
   request/candidate artifacts as untrusted data and validates them with
   trusted-main tooling before the `ghcr-release` environment approval.
4. Before approval, validation binds the transaction to an exact current-main SHA.
   After approval it requires that exact main tip plus PR/Gate/CodeQL state again.
   Any main advance requires a fresh prerelease request rather than silently
   reusing an approval against a different trust base.
5. `_publish-oci.yml` has no repository checkout, serializes by exact destination
   tag, and performs an API-only exact-main/source/PR freshness check immediately
   before registry authentication. It verifies the archive checksum, accepts an existing
   identical digest as an idempotent retry, and rejects a conflicting digest.

The pre-approval and post-approval checks are intentional, not duplicate policy:
the first avoids asking for approval of already-invalid state; the second proves
that the exact approved main tip plus mutable PR/Gate/CodeQL state still holds
immediately before publication. CI control-plane identity is checked against that
exact main tip before approval; the immutable PR/main SHAs make a duplicate blob
comparison after approval unnecessary.

### Stable releases

Stable publication is never tag-push triggered. `release.yml` is manual
`workflow_dispatch` from protected current `main` with:

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
members, unexpected files, or missing checksums. `_promote-oci.yml` globally
serializes alias movement and rejects SemVer rollback.

The OCI build explicitly requests BuildKit `provenance: mode=max`. Verification
requires an OCI v1 index with exactly one runnable `linux/amd64` and `linux/arm64`
manifest plus exactly one correctly linked BuildKit attestation manifest for each
runnable digest, then forces Skopeo to copy `--all` into a temporary OCI layout
so every referenced manifest/blob must be readable. Build arguments contain no
secrets; max-level provenance would make build-argument values observable.

## Policy and tests

`just workflow-check` runs `workflow_policy.py`. It deliberately does **not**
mirror GitHub's allowed-action package list or maintain a second action SHA
database. GitHub repository settings are the authority for which external action
packages may execute. The local checker only enforces the zero-maintenance
property that every external `uses:` reference is an immutable 40-character
commit SHA, plus the pipeline-specific trust boundaries.

Other checked invariants include:

- explicit `ubuntu-24.04` runner-major labels instead of floating `ubuntu-latest`;
- explicit max-level OCI provenance with no GitHub-secret references in the build action;
- low-trust candidate permissions and base-sourced helper boundaries;
- no repository checkout/code execution in isolated publication workflows;
- exact-tag and stable-alias concurrency guards;
- manual current-main stable release initiation, never tag-push initiation;
- protected-environment approval before the final mutable trust recheck;
- GitHub Release target/asset-digest verification;
- no tracked certificate/private-key material.

`just pipeline-test` compiles all control-plane Python and runs dependency-free
positive and negative regression tests. `_pre-commit` executes these pipeline
checks from a temporary archive of the **staged Git index**, not the working tree,
so an unstaged fix cannot hide a broken staged workflow.

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
