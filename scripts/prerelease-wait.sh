#!/usr/bin/env bash
set -euo pipefail

: "${GH_TOKEN:?GH_TOKEN is required}"
: "${REPOSITORY:?REPOSITORY is required}"
: "${REQUEST_RUN_ID:?REQUEST_RUN_ID is required}"
: "${REQUEST_PR:?REQUEST_PR is required}"
: "${REQUEST_SHA:?REQUEST_SHA is required}"
: "${REQUEST_TAG:?REQUEST_TAG is required}"
: "${REQUEST_VERSION:?REQUEST_VERSION is required}"

die() {
    echo "PR prerelease artifact refused: $*" >&2
    exit 1
}

artifact_name="prerelease-oci-$REQUEST_RUN_ID"
root="$RUNNER_TEMP/prerelease-candidate"
rm -rf "$root"
mkdir -p "$root/download" "$root/extracted" "$root/handoff"

artifact_json=
for attempt in $(seq 1 120); do
    artifacts=$(gh api --paginate --slurp "repos/$REPOSITORY/actions/artifacts?per_page=100" | jq '[.[].artifacts[]]')
    matches=$(jq -c --arg name "$artifact_name" '[.[] | select(.name == $name and .expired == false)]' <<<"$artifacts")
    count=$(jq 'length' <<<"$matches")
    if [[ "$count" -gt 1 ]]; then
        die "more than one non-expired artifact is named $artifact_name"
    fi
    if [[ "$count" -eq 1 ]]; then
        artifact_json=$(jq -c '.[0]' <<<"$matches")
        break
    fi
    echo "waiting for candidate artifact $artifact_name (attempt $attempt/120)"
    sleep 15
done
[[ -n "$artifact_json" ]] || die "candidate artifact did not arrive"

artifact_id=$(jq -r '.id' <<<"$artifact_json")
artifact_digest=$(jq -r '.digest // ""' <<<"$artifact_json")
artifact_run_id=$(jq -r '.workflow_run.id // ""' <<<"$artifact_json")
[[ "$artifact_digest" =~ ^sha256:[0-9a-f]{64}$ ]] || die "GitHub did not provide an artifact SHA-256 digest"
[[ "$artifact_run_id" =~ ^[0-9]+$ ]] || die "artifact has no producing workflow run"

run=$(gh api "repos/$REPOSITORY/actions/runs/$artifact_run_id")
jq -e --arg sha "$REQUEST_SHA" --arg repo "$REPOSITORY" --arg pr "$REQUEST_PR" '
    .event == "pull_request"
    and .name == "PR prerelease candidate"
    and .head_sha == $sha
    and (.head_repository.full_name // "") == $repo
    and .status == "completed"
    and .conclusion == "success"
    and ([.pull_requests[]? | select((.number | tostring) == $pr)] | length) == 1
' <<<"$run" >/dev/null || die "candidate workflow provenance does not match this request"

zip="$root/download/artifact.zip"
gh api "repos/$REPOSITORY/actions/artifacts/$artifact_id/zip" >"$zip"
downloaded_digest="sha256:$(sha256sum "$zip" | awk '{print $1}')"
[[ "$downloaded_digest" == "$artifact_digest" ]] || die "downloaded artifact digest does not match GitHub"

mapfile -t names < <(zipinfo -1 "$zip")
[[ "${#names[@]}" -eq 3 ]] || die "candidate artifact must contain exactly three files"
printf '%s\n' "${names[@]}" | sort -u | cmp -s - <(printf '%s\n' graphite-meter.oci.tar graphite-meter.oci.tar.sha256 candidate.json | sort) || die "candidate artifact contains unexpected or duplicate files"
if zipinfo -l "$zip" | awk 'NR > 3 && $1 ~ /^l/ {bad=1} END {exit bad}'; then :; else die "candidate artifact contains a symlink"; fi

unzip -q "$zip" -d "$root/extracted"
find "$root/extracted" -type l -print -quit | grep -q . && die "candidate artifact extracted a symlink" || true
for file in graphite-meter.oci.tar graphite-meter.oci.tar.sha256 candidate.json; do
    test -f "$root/extracted/$file" || die "candidate artifact is missing $file"
done

manifest="$root/extracted/candidate.json"
jq -e --arg repo "$REPOSITORY" --arg pr "$REQUEST_PR" --arg sha "$REQUEST_SHA" \
    --arg tag "$REQUEST_TAG" --arg version "$REQUEST_VERSION" --arg request "$REQUEST_RUN_ID" '
    .schemaVersion == 1
    and .repository == $repo
    and (.pr | tostring) == $pr
    and .headSha == $sha
    and .tag == $tag
    and .version == $version
    and .requestRunId == $request
    and (.candidateRunId | type) == "string"
    and (.candidateRunAttempt | type) == "number"
' "$manifest" >/dev/null || die "candidate manifest does not match the trusted request"

checksum="$root/extracted/graphite-meter.oci.tar.sha256"
grep -Eq '^[0-9a-f]{64}[[:space:]]+graphite-meter\.oci\.tar$' "$checksum" || die "candidate OCI checksum has an invalid format"
(cd "$root/extracted" && sha256sum -c graphite-meter.oci.tar.sha256) || die "candidate OCI checksum failed"

oci_sha256=$(sha256sum "$root/extracted/graphite-meter.oci.tar" | awk '{print $1}')
cp "$root/extracted/graphite-meter.oci.tar" "$root/handoff/graphite-meter.oci.tar"
jq -n --arg sha "$oci_sha256" --arg artifact "$artifact_digest" --arg sourceRun "$artifact_run_id" \
    '{ociSha256:$sha,githubArtifactDigest:$artifact,candidateRunId:$sourceRun}' >"$root/handoff/trusted-validation.json"

{
    echo "oci_sha256=$oci_sha256"
    echo "artifact_digest=$artifact_digest"
    echo "candidate_run_id=$artifact_run_id"
    echo "handoff_dir=$root/handoff"
} >>"${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"
