#!/usr/bin/env bash
set -euo pipefail

: "${MODE:?MODE is required}"
: "${GH_TOKEN:?GH_TOKEN is required}"
: "${REPOSITORY:?REPOSITORY is required}"
: "${PR_NUMBER:?PR_NUMBER is required}"
: "${REQUESTED_SHA:?REQUESTED_SHA is required}"
: "${REQUESTED_TAG:?REQUESTED_TAG is required}"

die() {
    echo "PR prerelease refused: $*" >&2
    exit 1
}

semver='^v[0-9]+\.[0-9]+\.[0-9]+-(alpha|beta|rc)\.[0-9]+$'
[[ "$PR_NUMBER" =~ ^[1-9][0-9]*$ ]] || die "PR number is invalid"
[[ "$REQUESTED_SHA" =~ ^[0-9a-f]{40}$ ]] || die "PR head SHA is invalid"
[[ "$REQUESTED_TAG" =~ $semver ]] || die "tag '$REQUESTED_TAG' is invalid"

if [[ "$MODE" == request ]]; then
    : "${EVENT_NAME:?EVENT_NAME is required}"
    : "${REF:?REF is required}"
    : "${WORKFLOW_REF:?WORKFLOW_REF is required}"
    : "${REPOSITORY_OWNER:?REPOSITORY_OWNER is required}"
    : "${ACTOR:?ACTOR is required}"
    : "${TRIGGERING_ACTOR:?TRIGGERING_ACTOR is required}"
    : "${REQUEST_RUN_ID:?REQUEST_RUN_ID is required}"

    [[ "$EVENT_NAME" == workflow_dispatch ]] || die "only workflow_dispatch may request a prerelease"
    [[ "$REF" == refs/heads/main ]] || die "the trusted prerelease workflow must run from main"
    [[ "$WORKFLOW_REF" == "$REPOSITORY/.github/workflows/prerelease.yml@refs/heads/main" ]] || die "workflow is not the trusted main workflow"
    [[ "$ACTOR" == "$REPOSITORY_OWNER" && "$TRIGGERING_ACTOR" == "$REPOSITORY_OWNER" ]] || die "Only the repository owner may publish an off-main prerelease."
    [[ "$REQUEST_RUN_ID" =~ ^[0-9]+$ ]] || die "workflow run ID is invalid"
elif [[ "$MODE" != recheck ]]; then
    die "unknown mode '$MODE'"
fi

pr=$(gh api "repos/$REPOSITORY/pulls/$PR_NUMBER")
state=$(jq -r '.state' <<<"$pr")
base=$(jq -r '.base.ref' <<<"$pr")
head_repo=$(jq -r '.head.repo.full_name // ""' <<<"$pr")
head_sha=$(jq -r '.head.sha' <<<"$pr")
merge_sha=$(jq -r '.merge_commit_sha // ""' <<<"$pr")

[[ "$state" == open ]] || die "PR #$PR_NUMBER is not open"
[[ "$base" == main ]] || die "PR #$PR_NUMBER does not target main"
[[ "$head_repo" == "$REPOSITORY" ]] || die "fork PRs cannot publish prereleases"
[[ "$head_sha" == "$REQUESTED_SHA" ]] || die "PR #$PR_NUMBER head SHA changed"
[[ -n "$merge_sha" ]] || die "PR #$PR_NUMBER has no current merge commit"

head_tree=$(gh api "repos/$REPOSITORY/commits/$head_sha" | jq -r '.commit.tree.sha')
merge_tree=$(gh api "repos/$REPOSITORY/commits/$merge_sha" | jq -r '.commit.tree.sha')
[[ "$head_tree" == "$merge_tree" ]] || die "PR #$PR_NUMBER is not up to date with main; update the branch and let CI pass again"

check_runs=$(gh api --paginate --slurp "repos/$REPOSITORY/commits/$REQUESTED_SHA/check-runs?per_page=100" | jq '[.[].check_runs[]]')

require_check() {
    local name=$1 app=$2 check status conclusion workflow
    check=$(jq -c --arg name "$name" --arg app "$app" '
        [.[] | select(.name == $name and (.app.slug // "") == $app)]
        | sort_by((.completed_at // .started_at // .created_at) // "")
        | if length == 0 then empty else .[-1] end
    ' <<<"$check_runs")
    [[ -n "$check" ]] || die "$name for $REQUESTED_SHA is missing"
    status=$(jq -r '.status' <<<"$check")
    conclusion=$(jq -r '.conclusion // "pending"' <<<"$check")
    workflow=$(jq -r '.check_suite.workflow_name // ""' <<<"$check")
    [[ "$status" == completed ]] || die "$name for $REQUESTED_SHA is $status"
    [[ "$conclusion" == success ]] || die "$name for $REQUESTED_SHA is $conclusion"
    if [[ "$name" == Gate && -n "$workflow" && "$workflow" != CI ]]; then
        die "Gate for $REQUESTED_SHA came from workflow '$workflow'"
    fi
}

require_check Gate github-actions
require_check CodeQL github-advanced-security

version=${REQUESTED_TAG#v}
if [[ "$MODE" == request ]]; then
    label="gm-prerelease-$REQUEST_RUN_ID"
    description=$(jq -cn --arg tag "$REQUESTED_TAG" --arg sha "$REQUESTED_SHA" \
        '{t:$tag,s:$sha}')
    if (( ${#description} > 100 )); then
        die "internal prerelease label metadata exceeds GitHub's 100-character limit"
    fi
    label_body=$(jq -cn --arg name "$label" --arg description "$description" \
        '{name:$name,color:"b60205",description:$description}')
    gh api --method POST "repos/$REPOSITORY/labels" --input - <<<"$label_body" >/dev/null

    comment=$(jq -cn --arg label "$label" --arg tag "$REQUESTED_TAG" \
        '{body:("Trusted prerelease request validated for `" + $tag + "`. An owner must apply the temporary label `" + $label + "` to this PR to start the isolated candidate build. The label will be removed automatically when the request finishes.")}')
    gh api --method POST "repos/$REPOSITORY/issues/$PR_NUMBER/comments" --input - <<<"$comment" >/dev/null

    {
        echo "label=$label"
        echo "request_run_id=$REQUEST_RUN_ID"
        echo "pr=$PR_NUMBER"
        echo "sha=$REQUESTED_SHA"
        echo "tag=$REQUESTED_TAG"
        echo "version=$version"
    } >>"${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"
elif [[ "$MODE" == recheck ]]; then
    {
        echo "pr=$PR_NUMBER"
        echo "sha=$REQUESTED_SHA"
        echo "tag=$REQUESTED_TAG"
        echo "version=$version"
    } >>"${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"
fi
