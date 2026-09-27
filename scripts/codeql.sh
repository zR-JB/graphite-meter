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
    source_root="$out/src"
    if [ "$language" = rust ]; then
        toolchain=$(python3 -c 'import tomllib; print(tomllib.load(open("rust/rust-toolchain.toml", "rb"))["toolchain"]["channel"])')
        sysroot=$(rustup run "$toolchain" rustc --print sysroot)
        [ -d "$sysroot/lib/rustlib/src/rust/library" ] || { echo "Install rust-src for $toolchain" >&2; exit 1; }
        export CODEQL_EXTRACTOR_RUST_SYSROOT="$sysroot"
        export CODEQL_EXTRACTOR_RUST_SYSROOT_SRC="$sysroot/lib/rustlib/src/rust/library"
        export CODEQL_EXTRACTOR_RUST_PROC_MACRO_SERVER="$sysroot/libexec/rust-analyzer-proc-macro-srv"
        export CODEQL_EXTRACTOR_RUST_CARGO_ALL_TARGETS=true
        export CODEQL_EXTRACTOR_RUST_EXTRA_INCLUDES
        export CODEQL_EXTRACTOR_RUST_BUILD_SCRIPT_COMMAND
        CODEQL_EXTRACTOR_RUST_EXTRA_INCLUDES=$(python3 -c 'import json,sys; print(json.dumps([sys.argv[1]]))' "$out/src/rust")
        CODEQL_EXTRACTOR_RUST_BUILD_SCRIPT_COMMAND=$(python3 -c 'import json,sys; print(json.dumps(["cargo", "+" + sys.argv[1], "check", "--workspace", "--all-targets", "--all-features", "--locked", "--offline", "--message-format=json", "-j2", "--target-dir", sys.argv[2]]))' "$toolchain" "$out/src/target")
        source_root="$out/src/rust"
        build+=(--extractor-option="rust.cargo_target_dir=$out/src/target")
    fi
    [ "$language" != go ] || build=(--command="bash $out/build-go.sh" --working-dir="$out/src/go")
    quiet "$cli" database create "$out/db-$language" --language="$language" \
        --source-root="$source_root" "${build[@]}" --overwrite --threads=4
    quiet "$cli" database analyze "$out/db-$language" --threat-model=local --format=sarif-latest \
        --output="$out/$language.sarif" --threads=4 --ram=8192 \
        "codeql/$pack-queries:codeql-suites/$pack-security-extended.qls" \
        "codeql/$pack-queries:codeql-suites/$pack-security-and-quality.qls"
done
python3 scripts/codeql_results.py "$out"
