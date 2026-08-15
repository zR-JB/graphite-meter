# Graphite Meter — monorepo task runner.
# Requires: Bun `.bun-version`, Go `go/go.mod`, and Just `.just-version`.
# (https://github.com/casey/just)
#
# Naming: `<component>-<verb>`, where verb is one of:
#   build  — produce an artifact (client/dist, or a go binary) and stop there.
#   run    — execute something that's already runnable (a binary, a dev server).
#   test   — run that component's test suite.
# `dev` and `prod` are the two config profiles; a recipe suffixed `-dev`/`-prod`
# only differs in which profile it targets. The two top-level recipes without a
# component prefix, `dev` and `prod`, are the "do everything" entrypoints: each
# builds the client in its own profile, embeds it into the Go server, and runs
# the server — same shape both ways, only the profile differs. Anything with a
# leading underscore is a private helper step, not meant to be run directly.

# Default shell for Linux/macOS.
[unix]
set shell := ["sh", "-c"]

# PowerShell shell used on Windows (PowerShell 7+).
[windows]
set shell := ["pwsh", "-NoProfile", "-Command"]

# CRITICAL: Automatically exports all just variables as environment variables
# to the underlying shell. This eliminates inline "KEY=VALUE command" breaking on Windows.
set export

# --- Production build knobs (override via env or `just prod label=… engine=…`) ---
# These feed the client's Vite `define` (see client/vite.config.ts) and the
# server's version ldflag. `client-build-prod` / `prod` / `server-build-prod`
# all build a real-only, dev-tooling-free bundle by default; flip the knobs to
# produce a configurable multi-engine build instead.
engine := env("GM_CLIENT_ENGINE", "real")
allow_dummy := env("GM_CLIENT_ALLOW_DUMMY", "0")
dev_tools := env("GM_CLIENT_DEV_TOOLS", "0")

# Detect Git commit hash cross-platform, suppressing errors cleanly depending on OS.
# `label` is the status-bar label and the label half of the client version
# <semver>+<label> (Endpoint info drawer, dist/version.json, preflight query).
# `version` feeds BOTH the client build's VERSION env (see client-build-prod
# below) and the Go ldflag, identically, so a `just` build always agrees with
# itself. Three fallback tiers, low to high precedence:
#   1. no VERSION, no `just` at all (raw `go build`/`bun run build`) — the
#      compiled-in sentinels: go/internal/config.EngineVersion's "0.0.0-dev"
#      and client/package.json's "0.0.0". Never bumped by hand.
#   2. `just` run with no VERSION set — falls back to the git short hash
#      (this line), a real build identity, distinct from tier 1's sentinel.
#   3. `VERSION=x.y.z just ...` (or CI/release.yml, which always sets it from
#      the git tag) — the authoritative release version.
label := env("GM_CLIENT_BUILD_LABEL", if os() == "windows" { `cmd.exe /c "git rev-parse --short HEAD 2>nul || echo prod"` } else { `git rev-parse --short HEAD 2>/dev/null || echo prod` })
version := env("VERSION", label)
# Legal metadata distinguishes a release tag from an untagged development
# build. The normal build label may be a commit hash, but that is not a source
# release version and must not appear as legal source-version metadata.
legal_version := env("VERSION", "development")
tools_dir := ".tools"
gitleaks_version := trim(shell("cat .gitleaks-version"))
gitleaks_image := "ghcr.io/gitleaks/gitleaks@sha256:c00b6bd0aeb3071cbcb79009cb16a60dd9e0a7c60e2be9ab65d25e6bc8abbb7f"
staticcheck_version := "2025.1.1"
govulncheck_version := "v1.6.0"

# Set OS-specific path for the Go build cache to remain fully cross-platform
GOCACHE := env("GOCACHE", if os() == "windows" { env("TEMP") / "graphite-meter-go-build" } else { "/tmp/graphite-meter-go-build" })
CGO_ENABLED := "0"

# List the available developer commands.
[group('setup')]
default:
    @just --list

# Prepare dependencies, pinned tools, and the repository hook.
[group('setup')]
setup:
    #!/usr/bin/env sh
    set -eu
    cd client && bun install --frozen-lockfile
    cd ../go && go mod download
    cd ..
    just _install-tools
    git config core.hooksPath .githooks
    just doctor

# Report and validate local, CI, and Docker toolchain drift.
[group('setup')]
doctor:
    #!/usr/bin/env sh
    set -eu
    fail=0
    expected_go=$(sed -n 's/^toolchain[[:space:]]\+//p' go/go.mod | head -1)
    actual_go=$(go env GOVERSION 2>/dev/null || echo unknown)
    actual_go=${actual_go%%[-	 ]*}
    expected_bun=$(tr -d '[:space:]' < .bun-version)
    actual_bun=$(bun --version 2>/dev/null || echo missing)
    actual_bun_revision=$(bun --revision 2>/dev/null || echo missing)
    expected_just=$(tr -d '[:space:]' < .just-version)
    actual_just=$(just --version 2>/dev/null | awk '{print $2}')
    docker_go=$(sed -n 's/^FROM docker.io\/library\/golang:\([^ ]*\) AS server$/\1/p' container/Dockerfile | head -1)
    docker_bun=$(sed -n 's/^ARG BUN_VERSION=//p' container/Dockerfile | head -1)
    echo "expected Go: ${expected_go:-missing}"
    echo "actual Go:   ${actual_go:-missing}"
    echo "expected Bun: ${expected_bun:-missing}"
    echo "actual Bun:   ${actual_bun:-missing} (${actual_bun_revision:-missing})"
    echo "expected Just: ${expected_just:-missing}"
    echo "actual Just:   ${actual_just:-missing}"
    echo "Docker Go builder: ${docker_go:-missing}"
    echo "Docker Bun fallback: ${docker_bun:-missing}"
    [ "$actual_go" = "$expected_go" ] || { echo "doctor: Go toolchain mismatch" >&2; fail=1; }
    if [ "$expected_bun" = canary ]; then
        echo "$actual_bun_revision" | grep -q canary || { echo "doctor: Bun is not on the canary channel; run bun upgrade --canary" >&2; fail=1; }
    else
        [ "$actual_bun" = "$expected_bun" ] || { echo "doctor: Bun version mismatch" >&2; fail=1; }
    fi
    [ "$actual_just" = "$expected_just" ] || { echo "doctor: Just version mismatch" >&2; fail=1; }
    [ "$docker_go" = "${expected_go#go}" ] || { echo "doctor: Docker Go builder disagrees with go.mod" >&2; fail=1; }
    [ "$docker_bun" = "$expected_bun" ] || { echo "doctor: Docker Bun fallback disagrees with .bun-version" >&2; fail=1; }
    hardcoded=$(grep -RInE --include='*.yml' --include='*.yaml' '^[[:space:]]*(go-version|bun-version):' .github/workflows 2>/dev/null || true)
    if [ -n "$hardcoded" ]; then
        echo "doctor: hard-coded CI toolchain declarations found outside setup-project:" >&2
        echo "$hardcoded" >&2
        fail=1
    fi
    gitleaks_path="{{ tools_dir }}/gitleaks-{{ gitleaks_version }}/gitleaks"
    if [ -x "$gitleaks_path" ]; then
        echo "Gitleaks: $($gitleaks_path version 2>/dev/null || echo installed)"
    else
        echo "Gitleaks: missing (run just setup; required by pre-commit)"
    fi
    exit "$fail"

[private]
_install-tools staticcheck="true" govulncheck="true" gitleaks="true":
    #!/usr/bin/env sh
    set -eu
    if [ "{{ staticcheck }}" = true ]; then
        mkdir -p "{{ tools_dir }}/staticcheck-{{ staticcheck_version }}"
        test -x "{{ tools_dir }}/staticcheck-{{ staticcheck_version }}/staticcheck" || GOBIN="$PWD/{{ tools_dir }}/staticcheck-{{ staticcheck_version }}" go install honnef.co/go/tools/cmd/staticcheck@{{ staticcheck_version }}
        "{{ tools_dir }}/staticcheck-{{ staticcheck_version }}/staticcheck" -version
    fi
    if [ "{{ govulncheck }}" = true ]; then
        mkdir -p "{{ tools_dir }}/govulncheck-{{ govulncheck_version }}"
        test -x "{{ tools_dir }}/govulncheck-{{ govulncheck_version }}/govulncheck" || GOBIN="$PWD/{{ tools_dir }}/govulncheck-{{ govulncheck_version }}" go install golang.org/x/vuln/cmd/govulncheck@{{ govulncheck_version }}
        "{{ tools_dir }}/govulncheck-{{ govulncheck_version }}/govulncheck" -version
    fi
    if [ "{{ gitleaks }}" = true ]; then
        mkdir -p "{{ tools_dir }}/gitleaks-{{ gitleaks_version }}"
        test -x "{{ tools_dir }}/gitleaks-{{ gitleaks_version }}/gitleaks" || GOBIN="$PWD/{{ tools_dir }}/gitleaks-{{ gitleaks_version }}" go install github.com/zricethezav/gitleaks/v8@{{ gitleaks_version }}
        "{{ tools_dir }}/gitleaks-{{ gitleaks_version }}/gitleaks" version
    fi

[private]
_pre-commit:
    #!/usr/bin/env sh
    set -eu
    staged=$(git diff --cached --name-only --diff-filter=ACMR)
    [ -z "$staged" ] && exit 0
    branch=$(git symbolic-ref --short HEAD 2>/dev/null || echo HEAD)
    if [ "$branch" = main ]; then
        echo "pre-commit: refusing to commit directly to main; create a branch first" >&2
        exit 1
    fi
    if git diff --cached -U0 --diff-filter=ACMR | grep -qE '^\+(<{7}|={7}|>{7})'; then
        echo "pre-commit: unresolved merge conflict markers in staged changes" >&2
        exit 1
    fi
    tls_files=$(printf '%s\n' "$staged" | grep -Ei '(^|/)(\.dev-certs|certs?|certificates?|letsencrypt)(/|$)|\.(pem|key|crt|cer|der|csr|p12|pfx|pkcs8|jks|keystore)$' || true)
    if [ -n "$tls_files" ]; then
        echo "pre-commit: refusing staged TLS certificate or key material:" >&2
        echo "$tls_files" >&2
        exit 1
    fi
    if git diff --cached -U0 --diff-filter=ACMR | grep -qE '^\+-----BEGIN (CERTIFICATE|([^ -]+ )*PRIVATE KEY)-----'; then
        echo "pre-commit: refusing staged PEM certificate or private-key material" >&2
        exit 1
    fi
    if ! git diff --cached --name-only --diff-filter=ACMR -z |
        xargs -0 -r -n1 sh -c '
            file=$1
            bytes=$(git cat-file -s ":$file" 2>/dev/null || echo 0)
            if [ "$bytes" -gt 1048576 ]; then
                echo "pre-commit: staged file exceeds 1 MiB: $file" >&2
                exit 1
            fi
        ' sh
    then
        exit 1
    fi
    gitleaks="{{ tools_dir }}/gitleaks-{{ gitleaks_version }}/gitleaks"
    if [ ! -x "$gitleaks" ]; then
        echo "pre-commit: pinned Gitleaks is missing; run just setup" >&2
        exit 1
    fi
    "$gitleaks" protect --staged --redact -v
    git diff --cached --check

    if printf '%s\n' "$staged" | grep -qE '^(\.github/|\.githooks/|scripts/ci/|justfile$)'; then
        just _pipeline-check-staged
    fi

    if printf '%s\n' "$staged" | grep -qE '^(go/|client/src/auth/)'; then
        just check-generated
    fi
    if printf '%s\n' "$staged" | grep -q '^go/'; then
        just server-check
        just server-unit
    fi
    if printf '%s\n' "$staged" | grep -q '^client/'; then
        just client-ci
    fi
    if printf '%s\n' "$staged" | grep -qE '^(go/|client/|legal/|container/|scripts/(package-tui\.sh|tui-targets\.txt)$|LICENSE$|COPYRIGHT$)'; then
        just legal-check
    fi

# Validate the staged index, not the working tree, before committing CI/control-plane changes.
[private]
_pipeline-check-staged:
    #!/usr/bin/env sh
    set -eu
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    tree=$(git write-tree)
    git archive "$tree" | tar -x -C "$tmp"
    (cd "$tmp" && python3 -m compileall -q scripts/ci && python3 scripts/ci/workflow_policy.py && python3 scripts/ci/test_pipeline.py)

# Validate GitHub Actions structure, immutable external refs, trust boundaries, and publication ordering.
[group('check')]
workflow-check:
    python3 scripts/ci/workflow_policy.py

# Run dependency-free release/prerelease control-plane regressions.
[group('check')]
pipeline-test:
    python3 -m compileall -q scripts/ci
    python3 scripts/ci/test_pipeline.py

# Run the same pinned Gitleaks container used by GitHub Actions.
[group('check')]
secret-scan-ci:
    #!/usr/bin/env sh
    set -eu
    engine=${CONTAINER_ENGINE:-docker}
    command -v "$engine" >/dev/null 2>&1 || { echo "secret-scan-ci requires Docker or Podman" >&2; exit 2; }
    expected=$(printf '%s' '{{ gitleaks_version }}' | sed 's/^v//')
    actual=$("$engine" run --rm "{{ gitleaks_image }}" version | tr -d '\r')
    [ "$actual" = "$expected" ] || { echo "Gitleaks image version $actual != expected $expected" >&2; exit 1; }
    "$engine" run --rm -v "$PWD:/repo:ro" "{{ gitleaks_image }}" \
      detect --source=/repo --no-banner --redact --exit-code 1

# Run the fast deterministic developer gate after setup.
[group('check')]
check: doctor workflow-check pipeline-test check-generated client-ci server-check server-unit staticcheck legal-check

# Run client formatting, type checks, generated checks, and unit tests.
[group('check')]
client-ci: client-check-generated
    cd client && bun run format:check
    cd client && bun run check
    cd client && bun test

# --- Client (Svelte/Vite, bun) ---

# Build the client with the development profile.
[group('build')]
client-build-dev:
    cd client && bun run build

# Build the client with the production profile.
[group('build')]
client-build-prod:
    bun -e "process.env.GM_CLIENT_ENGINE='{{ engine }}'; process.env.GM_CLIENT_ALLOW_DUMMY='{{ allow_dummy }}'; process.env.GM_CLIENT_DEV_TOOLS='{{ dev_tools }}'; process.env.GM_CLIENT_BUILD_LABEL='{{ label }}'; process.env.VERSION='{{ version }}'; import { spawnSync } from 'child_process'; spawnSync('bun', ['run', 'build'], { stdio: 'inherit', shell: true, cwd: 'client', env: process.env });"

# Discover the production browser closure in a temporary Vite output tree and
# run the single offline legal generator. The scan build always consumes the
# checked-in about.json from the previous generation, avoiding a circular
# dependency between the bundle and its legal data.
_legal-run mode="check":
    #!/usr/bin/env sh
    set -eu
    scan=$(mktemp)
    out=$(mktemp -d)
    trap 'rm -f "$scan"; rm -rf "$out"' EXIT
    cd client
    GM_LEGAL_SCAN_OUT="$scan" GM_LEGAL_SCAN_DIR="$out" bun run build:bundle -- --outDir "$out"
    cd ..
    cd go
    VERSION='{{ legal_version }}' GM_LEGAL_SCAN_MODULES="$scan" go run ./internal/legal/cmd/legalgen -mode '{{ mode }}' -repo ..

# Regenerate reviewed legal outputs after an approved change.
[group('legal')]
legal-generate:
    just _legal-run generate

# Validate runtime legal closure, reviews, provenance, and generated drift.
[group('legal')]
legal-check:
    just _legal-run check

# Print exceptional legal audit or review-template output.
[group('legal')]
legal-review mode="audit":
    just _legal-run review-{{ mode }}

[private]
_legal-source-bundle:
    just _legal-run source-bundle

# Type-check the client, including Bun test files.
[group('check')]
client-check:
    cd client && bun run check

# Run the client's Bun unit tests.
[group('check')]
client-test:
    cd client && bun test

# Deliberately refresh Bun's native per-file timing metadata.
[group('manual')]
client-test-timings:
    cd client && bun test --timings=test-timings.json --update-timings

# Run the standalone Vite development server.
[group('dev')]
client-watch:
    cd client && bun run dev

# Preview the real no-JavaScript login page on loopback.
[group('dev')]
auth-preview mode="hybrid" oidc_ready="true":
    cd go && go run ./internal/auth/cmd/authpreview --mode {{ quote(mode) }} --oidc-ready={{ quote(oidc_ready) }}

# Output lives inside the client tree so Vite's dev-server fs.allow is happy.
# Regenerate client API types from the JSON schemas.
[group('build')]
client-gen-types:
    cd client && bunx json-schema-to-typescript ../api/preflight.schema.json -o src/lib/api/preflight.ts
    cd client && bunx json-schema-to-typescript ../api/probe.schema.json -o src/lib/api/probe.ts

# The same drift gate check-generated applies to the embedded auth assets, for
# the schema-derived client types. It hangs off client-ci rather than
# check-generated because the generator needs bun and client/node_modules, and
# ci.yml calls check-generated from the Go job, which sets up neither.
# Check that generated client API types match the schemas.
[group('check')]
client-check-generated: client-gen-types
    #!/usr/bin/env sh
    set -e
    if ! git diff --quiet -- client/src/lib/api/; then
        echo "client/src/lib/api is stale; run 'just client-gen-types' and commit the result"
        exit 1
    fi

# --- Embedding (Go module + client, shared by both profiles) ---

# Copy the just-built client/dist into the Go module so //go:embed picks it up.
# Handled cross-platform via Bun's filesystem API to avoid rm/cp syntax differences.
# Profile-agnostic: run this after whichever of client-build-dev/client-build-prod ran.
_embed-client:
    bun -e "import fs from 'fs'; fs.rmSync('go/internal/static/dist', { recursive: true, force: true }); fs.cpSync('client/dist', 'go/internal/static/dist', { recursive: true }); fs.writeFileSync('go/internal/static/dist/.gitkeep', '')"

# --- Go server ---
# build-* recipes produce a standalone binary and stop (nothing runs it) — that's
# what a persisted artifact is for, so both strip debug info (-s -w -trimpath).
# The dev/prod entrypoints below use `go run` instead: there's no artifact to
# strip when the binary is executed once and discarded.

# Build the dev-profile server binary: dev client embedded, no version stamp
# Build the development Go server binary with embedded client assets.
[group('build')]
server-build-dev: client-build-dev _embed-client
    cd go && go build -ldflags="-s -w" -trimpath -o graphite-meter ./cmd/graphite-meter

# This is the shippable artifact (what a manual/non-Docker deploy would run).
# Build the prod-profile server binary: real-only client embedded, version-stamped.
# Build the production Go server binary with embedded client assets.
[group('build')]
server-build-prod: client-build-prod _embed-client
    cd go && go build \
      -ldflags="-s -w -X github.com/zR-JB/graphite-meter/go/internal/config.EngineVersion={{ version }}" \
      -trimpath -o graphite-meter ./cmd/graphite-meter

# Check Go formatting and vet diagnostics. The second vet carries the stress
# tag so the saturation harness cannot rot outside the gate.
# Run Go formatting and vet diagnostics.
[group('check')]
server-check:
    cd go && unformatted=$(gofmt -l .); if [ -n "$unformatted" ]; then echo "$unformatted"; gofmt -d .; exit 1; fi
    cd go && go vet ./...
    cd go && go vet -tags stress ./internal/server/

# Run the fast non-race Go unit suite used by the normal developer gate.
# Run the fast non-race Go unit suite.
[group('check')]
server-unit:
    cd go && go test ./...

# Regenerate the embedded auth assets and fail if they drift from source. The
# pre-commit hook and ci.yml both call this recipe.
# Regenerate embedded auth assets and fail if they drift.
[group('check')]
check-generated:
    #!/usr/bin/env sh
    set -e
    (cd go/internal/auth && go run ./cmd/authassets)
    if ! git diff --quiet -- go/internal/auth/assets_generated.go; then
        echo "assets_generated.go is stale; run 'go generate ./internal/auth' and commit the result"
        exit 1
    fi

# Deterministic static analysis. `just setup` prepares this exact binary.
# Run deterministic static analysis with the pinned binary.
[group('check')]
staticcheck:
    #!/usr/bin/env sh
    set -e
    staticcheck_dir="$PWD/{{ tools_dir }}/staticcheck-{{ staticcheck_version }}"
    test -x "$staticcheck_dir/staticcheck" || { echo "staticcheck is not prepared; run just setup" >&2; exit 1; }
    cd go
    "$staticcheck_dir/staticcheck" ./...

# Network-sensitive vulnerability scanning against the live advisory database.
# Run the networked vulnerability scan against live advisories.
[group('check')]
security:
    #!/usr/bin/env sh
    set -e
    govulncheck_dir="$PWD/{{ tools_dir }}/govulncheck-{{ govulncheck_version }}"
    test -x "$govulncheck_dir/govulncheck" || { echo "govulncheck is not prepared; run just setup" >&2; exit 1; }
    cd go
    "$govulncheck_dir/govulncheck" ./...

# Race-detector tests plus the coverage floor. ci.yml calls this recipe, so the
# floor is enforced identically locally and in CI. Raise the floor as coverage
# climbs; never lower it.
# Run shuffled race tests and enforce the coverage floor.
[group('check')]
server-test:
    #!/usr/bin/env sh
    set -e
    cd go
    CGO_ENABLED=1 go test -race -shuffle=on -coverprofile=cover.out ./...
    total=$(go tool cover -func=cover.out | awk '/^total:/ {print $3}' | tr -d '%')
    echo "total statement coverage: ${total}%"
    awk -v t="$total" 'BEGIN { exit (t + 0 >= 75.0) ? 0 : 1 }' \
        || { echo "coverage ${total}% is below the 75% floor"; exit 1; }

# The stubbed browser suite for one selected engine. The bundle is rebuilt
# because Playwright's webServer previews dist.
# Run the selected stubbed Playwright browser project.
[group('check')]
client-browser project="chromium":
    cd client && bun run build:browser && bunx playwright test -c playwright.browser.config.ts --project={{ project }}

# End to end: boots the server and moves bytes over every real transport from
# a real browser, through the production lanes. The H3 cells are Chromium-only:
# QUIC ignores ignoreHTTPSErrors, while Firefox needs a system trust anchor.
# H1-clear fetch cells run on both. Needs Go and openssl; the certificate is
# generated per run, so nothing is a prerequisite.
# Run real transport E2E tests through real browsers and a real server.
[group('check')]
client-e2e:
    #!/usr/bin/env sh
    set -e
    certs=$(mktemp -d)
    trap 'rm -rf "$certs"' EXIT
    openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
      -keyout "$certs/key.pem" -out "$certs/cert.pem" -subj "/CN=127.0.0.1" \
      -addext "subjectAltName=IP:127.0.0.1,DNS:localhost" 2>/dev/null
    GM_E2E_TLS_CERT="$certs/cert.pem"
    GM_E2E_TLS_KEY="$certs/key.pem"
    GM_E2E_SPKI=$(openssl x509 -in "$certs/cert.pem" -pubkey -noout \
      | openssl pkey -pubin -outform der \
      | openssl dgst -sha256 -binary | openssl enc -base64)
    export GM_E2E_TLS_CERT GM_E2E_TLS_KEY GM_E2E_SPKI
    cd client && bun run test:e2e

# Measurement only; not part of ci. Unix only, since the CPU column reads
# getrusage. Loads the server over kernel TCP and again over userspace QUIC, and
# once more with the CPU constrained.
# Server saturation envelope (issue #44): observer RTT percentiles under growing loader concurrency.
# Measure the server saturation envelope; this is not a CI gate.
[group('manual')]
stress:
    cd go && go test -tags stress -run TestSaturationEnvelope -v -timeout 30m -count=1 ./internal/server/

# Excluded from CI. Every benchmark decodes or encodes a PONG; the progress
# feed's NDJSON is not measured here or anywhere.
# Ping-bus encoding evidence, Go and TypeScript: why the bus keeps a text codec.
# Benchmark the Go and TypeScript wire codec implementations.
[group('manual')]
bench-wire:
    cd go && go test ./internal/wire/ -run '^$' -bench 'Decode|Encode' -benchmem -benchtime=2s
    cd client && bun run src/lib/runner/real/wire.bench.ts

# Measurement only; not part of ci, and it takes hours. `filter` is a playwright
# -g filter and `project` a --project passthrough; omit either and that axis is
# not narrowed, so a bare filter still runs against every browser project. Cell
# ids look like `h1-clear/down/lanes=2`, so one cell on one engine is:
#   just bench-throughput 'h1-clear/down/lanes=2' chromium
# Needs ../.dev-certs (see docs/DEVELOPMENT.md) on every run, since the config
# starts all four listeners whatever origins were asked for, and GM_BENCH_SPKI
# set, or the chromium project does not exist at all.
# Browser throughput matrix against a real server, chromium and firefox.
# Run the long browser throughput benchmark matrix.
[group('manual')]
bench-throughput filter="" project="":
    cd client && bunx playwright test -c playwright.bench.config.ts {{ if filter != "" { "-g '" + filter + "'" } else { "" } }} {{ if project != "" { "--project=" + project } else { "" } }}

# Build every supported native TUI target from one warmed Go setup.
# Cross-build every supported native TUI target from one Go setup.
[group('check')]
tui-cross-build:
    #!/usr/bin/env sh
    set -eu
    while IFS= read -r target; do
        [ -n "$target" ] || continue
        goos=${target%/*}
        goarch=${target#*/}
        echo "building ${goos}/${goarch}"
        (cd go && CGO_ENABLED=0 GOOS="$goos" GOARCH="$goarch" go build -trimpath -o /dev/null ./cmd/graphite-meter-client)
    done < scripts/tui-targets.txt

# Verify distributable package and source-bundle invariants without publishing.
# Verify representative release packages and corresponding source.
[group('release')]
release-check version="development":
    #!/usr/bin/env sh
    set -eu
    VERSION="{{ version }}" just legal-check
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    (cd go && go test ./internal/legal/...)
    RELEASE_DIST="$tmp/dist" VERSION="{{ version }}" just release-artifacts "{{ version }}"
    (cd "$tmp/dist" && sha256sum -c checksums.txt)
    while IFS= read -r target; do
        [ -n "$target" ] || continue
        goos=${target%/*}
        goarch=${target#*/}
        archive_base="graphite-meter-client_{{ version }}_${goos}_${goarch}"
        case "$goos" in
            windows) archive="$tmp/dist/$archive_base.zip"; unzip -l "$archive" | grep -F "$archive_base/THIRD_PARTY_NOTICES.txt" ;;
            *) archive="$tmp/dist/$archive_base.tar.gz"; tar -tzf "$archive" | grep -Fx "$archive_base/THIRD_PARTY_NOTICES.txt" ;;
        esac
        test -s "$archive"
    done < scripts/tui-targets.txt
    test -s "$tmp/dist/graphite-meter_{{ version }}_corresponding-source.tar.gz"

# Build exact production client and Go server artifacts for release validation.
[group('release')]
release-build version="development":
    VERSION="{{ version }}" GM_CLIENT_BUILD_LABEL=prod just server-build-prod

# Build all stable release artifacts into go/dist without publishing them.
# Build all versioned release artifacts and their checksums.
[group('release')]
release-artifacts version="development":
    #!/usr/bin/env sh
    set -eu
    dist=${RELEASE_DIST:-go/dist}
    case "$dist" in /*) ;; *) dist="$PWD/$dist" ;; esac
    mkdir -p "$dist"
    find "$dist" -maxdepth 1 -type f -delete
    VERSION="{{ version }}" LEGAL_SOURCE_OUT="$dist/graphite-meter_{{ version }}_corresponding-source.tar.gz" just _legal-source-bundle
    while IFS= read -r target; do
        [ -n "$target" ] || continue
        goos=${target%/*}
        goarch=${target#*/}
        scripts/package-tui.sh "{{ version }}" "$goos" "$goarch" "$dist"
    done < scripts/tui-targets.txt
    (cd "$dist" && find . -maxdepth 1 -type f ! -name checksums.txt -printf '%f\n' | sort | xargs sha256sum > checksums.txt)

# Build and verify the local container with the same reusable CI verifier.
# Build and verify the local container with the reusable smoke verifier.
[group('release')]
container-smoke:
    #!/usr/bin/env sh
    set -eu
    engine=${CONTAINER_ENGINE:-docker}
    command -v "$engine" >/dev/null 2>&1 || { echo "container-smoke requires Docker or Podman; install Docker or set CONTAINER_ENGINE=podman" >&2; exit 2; }
    bun_version=$(tr -d '[:space:]' < .bun-version)
    "$engine" build -f container/Dockerfile -t graphite-meter:smoke \
      --build-arg BUN_VERSION="$bun_version" \
      --build-arg VERSION=0.0.0-ci \
      --build-arg GM_CLIENT_BUILD_LABEL=local \
      --build-arg GM_CLIENT_VALIDATE=0 \
      --label org.opencontainers.image.licenses=AGPL-3.0-or-later .
    scripts/verify-container.sh graphite-meter:smoke

# Full local CI-equivalent validation, including security and Docker.
# Run the complete local CI-equivalent validation.
[group('check')]
ci:
    #!/usr/bin/env sh
    set -eu
    just check
    just server-test
    just security
    just client-browser chromium
    just client-browser firefox
    just client-e2e
    just tui-cross-build
    just release-check
    just container-smoke

# --- Go native TUI client (graphite-meter-client) ---
# No dev/prod split: it doesn't embed the Svelte client, so there's nothing to profile.

# Build only the native Go Bubble Tea client. Does not build or stage the Svelte app.
# Stamped with the same `version` as server-build-prod (see the fallback tiers above).
# Build the standalone native TUI client.
[group('build')]
goclient-build:
    cd go && go build \
      -ldflags="-s -w -X github.com/zR-JB/graphite-meter/go/internal/goclient.Version={{ version }}" \
      -trimpath -o graphite-meter-client ./cmd/graphite-meter-client

# Run the native Go client against a running Graphite Meter server.
# Run the standalone native TUI client against a server.
[group('dev')]
goclient-run:
    cd go && go run ./cmd/graphite-meter-client

# --- Main entrypoints ---
# Both build the client (in their own profile) and embed it, then `go run` the
# server (in the same profile) — same shape, only the profile differs.

# Dev: build + embed the dev-profile client, then `go run` the server on :7246
# Build the development client and run the embedded server.
[group('dev')]
dev: client-build-dev _embed-client
    cd go && go run ./cmd/graphite-meter

# Override the GM_CLIENT_* knobs inline exactly like client-build-prod, e.g.
# `just prod allow_dummy=1 dev_tools=1 label=0.2.0`. For a persisted binary
# instead of a live run, use `server-build-prod`.
# Prod: build + embed the prod-profile client, then `go run` the version-stamped server on :7246.
# Build the production client and run the embedded server.
[group('dev')]
prod: client-build-prod _embed-client
    cd go && go run \
      -ldflags="-X github.com/zR-JB/graphite-meter/go/internal/config.EngineVersion={{ version }}" \
      ./cmd/graphite-meter

# --- Container (Docker/Podman) ---

# Build the production image (single static binary)
# Build the production container image.
[group('build')]
container-build:
    #!/usr/bin/env sh
    set -eu
    engine=${CONTAINER_ENGINE:-docker}
    command -v "$engine" >/dev/null 2>&1 || { echo "container-build requires Docker or Podman; install Docker or set CONTAINER_ENGINE=podman" >&2; exit 2; }
    bun_version=$(tr -d '[:space:]' < .bun-version)
    "$engine" build -f container/Dockerfile -t graphite-meter:latest \
      --build-arg BUN_VERSION="$bun_version" \
      --build-arg VERSION="{{ version }}" \
      --build-arg GM_CLIENT_BUILD_LABEL="{{ label }}" .
