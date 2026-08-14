#!/usr/bin/env bash
set -euo pipefail

# Resolve a release only from a tag commit or the checked-out trusted main
# commit. This script intentionally does not accept PR, merge-ref, or SHA
# inputs from workflow_dispatch.

: "${GH_TOKEN:?GH_TOKEN is required}"
: "${REPOSITORY:?REPOSITORY is required}"
: "${EVENT_NAME:?EVENT_NAME is required}"
: "${EVENT_SHA:?EVENT_SHA is required}"
: "${REF:?REF is required}"
: "${REF_NAME:?REF_NAME is required}"
: "${WORKFLOW_REF:?WORKFLOW_REF is required}"

semver='^v[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta|rc)\.[0-9]+)?$'
tag="$REF_NAME"
dry_run=false

if [[ "$EVENT_NAME" == workflow_dispatch ]]; then
    if [[ "$REF" != refs/heads/main || "$WORKFLOW_REF" != "$REPOSITORY/.github/workflows/release.yml@refs/heads/main" ]]; then
        echo "Release refused: workflow_dispatch must run from the trusted release workflow on main." >&2
        exit 1
    fi
    tag="${REQUESTED_VERSION:-}"
    dry_run=true
    if [[ ! "$tag" =~ $semver ]]; then
        echo "Release refused: dry-run version '$tag' is not supported SemVer; must be vMAJOR.MINOR.PATCH or vMAJOR.MINOR.PATCH-{alpha,beta,rc}.N" >&2
        exit 1
    fi
    sha=$(git rev-parse HEAD)
else
    if [[ ! "$tag" =~ $semver ]]; then
        echo "Release refused: unsupported release tag '$tag'." >&2
        exit 1
    fi
    sha="$EVENT_SHA"
    if [[ "$(git rev-parse HEAD)" != "$sha" ]]; then
        echo "Release refused: checked-out tag commit does not match event SHA $sha." >&2
        exit 1
    fi
fi

git fetch --no-tags origin main
if ! git merge-base --is-ancestor "$sha" FETCH_HEAD; then
    echo "Release refused: $tag source $sha is not reachable from origin/main." >&2
    exit 1
fi

check_runs=$(gh api "repos/$REPOSITORY/commits/$sha/check-runs?per_page=100")
workflow_runs=$(gh api "repos/$REPOSITORY/actions/workflows/ci.yml/runs?head_sha=$sha&event=push&per_page=100")
ci_successes=$(jq --arg sha "$sha" '[.workflow_runs[] | select(.head_sha == $sha and .head_branch == "main" and .status == "completed" and .conclusion == "success")] | length' <<<"$workflow_runs")
if [[ "$ci_successes" -eq 0 ]]; then
    ci_state=$(jq -r --arg sha "$sha" '[.workflow_runs[] | select(.head_sha == $sha and .head_branch == "main")] | if length == 0 then "missing" else (.[0].status + "/" + (.[0].conclusion // "pending")) end' <<<"$workflow_runs")
    echo "Release refused: normal CI for $sha is $ci_state." >&2
    exit 1
fi

require_check() {
    local label=$1 app=$2
    local matches count status conclusion observed_app
    matches=$(jq --arg label "$label" --arg app "$app" '[.check_runs[] | select(.name == $label and (.app.slug // "") == $app)]' <<<"$check_runs")
    count=$(jq 'length' <<<"$matches")
    if [[ "$count" -eq 0 ]]; then
        echo "Release refused: $label for $sha is missing." >&2
        exit 1
    fi
    if [[ "$count" -ne 1 ]]; then
        echo "Release refused: $label for $sha is ambiguous ($count matching checks)." >&2
        exit 1
    fi
    read -r status conclusion observed_app <<<"$(jq -r '.[0] | [.status, (.conclusion // "pending"), (.app.slug // "unknown")] | @tsv' <<<"$matches")"
    if [[ "$status" != completed ]]; then
        echo "Release refused: $label for $sha is $status." >&2
        exit 1
    fi
    if [[ "$conclusion" != success ]]; then
        echo "Release refused: $label for $sha is $conclusion." >&2
        exit 1
    fi
    if [[ "$observed_app" != "$app" ]]; then
        echo "Release refused: $label for $sha came from unexpected app $observed_app." >&2
        exit 1
    fi
}

require_check Gate github-actions

# GitHub returns this endpoint as a top-level JSON array. Fetch all CodeQL
# analyses for main so releasing an older-but-still-main-reachable commit
# cannot fail merely because its analysis fell off the first result page.
analysis_pages=$(
    gh api --paginate --slurp \
        "repos/$REPOSITORY/code-scanning/analyses?ref=refs/heads/main&tool_name=CodeQL&per_page=100"
)

codeql_analyses=$(
    jq --arg sha "$sha" \
        '[.[][] | select(.commit_sha == $sha and (.tool.name // "") == "CodeQL")]' \
        <<<"$analysis_pages"
)

codeql_count=$(jq 'length' <<<"$codeql_analyses")

if [[ "$codeql_count" -eq 0 ]]; then
    echo "Release refused: CodeQL analysis for $sha is missing." >&2
    exit 1
fi

codeql_errors=$(jq '[.[] | select((.error // "") != "") ]' <<<"$codeql_analyses")
if [[ $(jq 'length' <<<"$codeql_errors") -gt 0 ]]; then
    echo "Release refused: CodeQL analysis for $sha has errors." >&2
    jq -r '.[] | "  \(.category // .analysis_key // \"unknown\"): \(.error)"' <<<"$codeql_errors" >&2
    exit 1
fi

version=${tag#v}
IFS=. read -r major minor _ <<<"${version%%-*}"
stable=true
[[ "$tag" == *-* ]] && stable=false
{
    echo "sha=$sha"
    echo "tag=$tag"
    echo "version=$version"
    echo "series=$major.$minor"
    echo "stable=$stable"
    echo "dry_run=$dry_run"
} >>"${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"
