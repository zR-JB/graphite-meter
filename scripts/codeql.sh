#!/usr/bin/env bash
# Reproduce the hosted CodeQL scan offline with the pinned CLI and list its results.
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
cat >"$out/build-go.sh" <<'BUILD'
#!/usr/bin/env bash
set -euo pipefail
go build -buildvcs=false ./...
go build -buildvcs=false -o /dev/null ../rust/tests/server_client.go
go build -buildvcs=false -o /dev/null ../rust/tests/h3_client.go
BUILD
for language in go javascript-typescript python actions rust; do
    echo "CodeQL $language" >&2
    pack=${language%-typescript}
    build=(--build-mode=none)
    [ "$language" != go ] || build=(--command="bash $out/build-go.sh" --working-dir="$out/src/go")
    quiet "$cli" database create "$out/db-$language" --language="$language" \
        --source-root="$out/src" "${build[@]}" --overwrite --threads=4
    quiet "$cli" database analyze "$out/db-$language" --threat-model=local --format=sarif-latest \
        --output="$out/$language.sarif" --threads=4 --ram=8192 \
        "codeql/$pack-queries:codeql-suites/$pack-security-extended.qls" \
        "codeql/$pack-queries:codeql-suites/$pack-security-and-quality.qls"
done
python3 scripts/codeql_results.py "$out"
