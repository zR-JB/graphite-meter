#!/usr/bin/env bash
# Reproduce the hosted CodeQL scan offline with the pinned CLI; fail on any result.
set -euo pipefail
cli=${CODEQL:-$(command -v codeql || echo "$HOME/.local/share/codeql-bundle/codeql/codeql")}
version=$("$cli" version --format=terse)
if [ "$version" != "$GM_CODEQL_VERSION" ]; then
    echo "$cli is CodeQL $version, not $GM_CODEQL_VERSION" >&2
    exit 1
fi
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
quiet() { "$@" >"$out/log" 2>&1 || { cat "$out/log" >&2; return 1; }; }
# Like the hosted scan, analyze tracked files only, not local build output.
mkdir "$out/src"
git ls-files -z | tar --null -T - -cf - | tar -xf - -C "$out/src"
for language in go javascript-typescript python actions rust; do
    echo "CodeQL $language" >&2
    pack=${language%-typescript}
    build=(--build-mode=none)
    [ "$language" != go ] || build=(--command="go build ./..." --working-dir="$out/src/go")
    quiet "$cli" database create "$out/db-$language" --language="$language" \
        --source-root="$out/src" "${build[@]}" --overwrite --threads=0
    quiet "$cli" database analyze "$out/db-$language" --threat-model=local --format=sarif-latest \
        --output="$out/$language.sarif" --threads=0 \
        "codeql/$pack-queries:codeql-suites/$pack-security-extended.qls" \
        "codeql/$pack-queries:codeql-suites/$pack-security-and-quality.qls"
done
jq -r '.runs[0].results[]? | .ruleId + "  " + (.locations[0].physicalLocation
    | .artifactLocation.uri + ":" + (.region.startLine | tostring))' "$out"/*.sarif >"$out/results"
cat "$out/results"
cut -d' ' -f1 "$out/results" | sort | uniq -c
echo "$(wc -l <"$out/results") results"
test ! -s "$out/results"
