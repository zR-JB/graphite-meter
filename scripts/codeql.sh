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
    # The Rust interop peer lives outside the Go module but builds against it.
    [ "$language" != go ] || build=(--command="go build ./..." --command="go build -o /dev/null ../rust/interop/peer.go"
        --working-dir="$out/src/go")
    if [ "$language" = rust ]; then
        # Resolve OUT_DIR includes and macro expansions from a real build; also extract #[path] modules.
        toolchain=+$(python3 -m scripts.ci.toolchains get rust.channel)
        export CODEQL_EXTRACTOR_RUST_PROC_MACRO_SERVER CODEQL_EXTRACTOR_RUST_BUILD_SCRIPT_COMMAND CODEQL_EXTRACTOR_RUST_EXTRA_INCLUDES
        CODEQL_EXTRACTOR_RUST_PROC_MACRO_SERVER=$(rustc "$toolchain" --print sysroot)/libexec/rust-analyzer-proc-macro-srv
        CODEQL_EXTRACTOR_RUST_BUILD_SCRIPT_COMMAND=$(jq -cn --arg toolchain "$toolchain" --arg target "$out/src/target" \
            '["cargo", $toolchain, "check", "--workspace", "--all-targets", "--locked", "--offline", "--message-format=json", "--target-dir", $target]')
        CODEQL_EXTRACTOR_RUST_EXTRA_INCLUDES=$(jq -cn --arg rust "$out/src/rust" '[$rust]')
        build+=(--extractor-option="rust.cargo_target_dir=$out/src/target")
    fi
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
