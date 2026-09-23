# Experimental Rust implementation

The server executable is under development; it is not yet a drop-in replacement.
Go remains the default. The native Rust TUI is runnable, including latency,
download, upload, and bidirectional stages. A failed transfer server is removed
from subsequent stages while surviving servers continue; affected results stay
marked partial. Latency observations and results remain separate for each server;
press `l` to change the displayed server. Full parity validation is unfinished.
Preliminary footprint measurements are promising; feature-complete performance
and size comparisons have not been established.

Run `GM_IMPLEMENTATION=rust mise run tui` to open the experimental TUI, or pass
`--url https://your-server`. `mise run tui` selects Go by default.
`mise run rust-client-run -- --url https://your-server` remains available.
Press `r` to run, `?` for help, and `q` to quit.
Use left/right arrows for the four setup pages and Tab/Shift-Tab to cycle through
setup and the live view. The carbon palette follows `COLORFGBG` when available
and adapts to truecolor, 256-color, or ANSI terminals. Set `GM_TUI_THEME=light`
or `dark` to override the background choice. `NO_COLOR` disables color.
The TUI can connect to either implementation's server.

`mise run rust-client-package VERSION` creates an experimental Linux amd64 GNU
archive with reviewed dependency notices and matching source. Release automation
can opt into the additional TUI archive. Stable release requests can also opt into
a separate Linux amd64 server image tagged `VERSION-rust`, with a matching source
offer. Go remains the release default; Rust prerelease integration remains gated.
The experimental container uses `container/Dockerfile.rust`. The port remains
blocked from merging until a human decides its design.

Set `GM_IMPLEMENTATION=rust` when running `mise run dev` or `mise run prod` to
select the experimental server. Leaving it unset selects Go.

`mise run rust-server-run` builds the browser UI and runs the experimental server
using `GM_*` configuration or server flags. HTTP/1, HTTPS/WSS, HTTP/2, and
HTTP/3/WebTransport listeners share authentication and measurement state.
Password, OIDC, and hybrid authentication are implemented. OIDC has been checked
against a local signed-token provider and a temporary HTTPS Keycloak realm,
including allowed and denied group membership. Other deployments remain untested.

Authentication forms require URL-encoded POST bodies with unique fields. Unlike
Go's form parser, Rust does not accept passwords or CSRF proofs from URL queries.

The workspace pins Rust 1.98.1. From the repository root:

```sh
mise run rust-check
mise run rust-crypto-check
mise run rust-format
python3 rust/tests/server_interop.py
python3 rust/tests/interop.py
```

Ring remains the default TLS/QUIC crypto provider. `mise run rust-crypto-check`
checks the alternative AWS-LC build for both binaries. Build one binary at a
time for provider comparisons; combining both provider features is rejected.
The first-party Rust crates forbid unsafe code. This does not make the full
dependency graph free of unsafe code or native cryptography: ring contains
C/assembly. Isolated probes of rustls-graviola 0.4.0 and rustls-rustcrypto
0.0.2-alpha compiled, but neither exposed a QUIC cipher suite to Noq. The
RustCrypto provider also explicitly warns against production use. A pure-Rust
QUIC performance comparison therefore remains unmeasured.

The tests require OpenSSL; interoperability checks also require Go.
`server_interop.py` exercises the assembled debug server with unchanged Go
libraries: bootstrap, HTTP/3 transfers, WebTransport pings, downloads, upload
progress, independent connections, and rejection of excess WebTransport sessions.
It currently covers unauthenticated loopback
traffic. `interop.py` separately probes low-level transport behavior.

The full probe currently fails on immediate stream reset with the unchanged Go
peer. `--fix-go-reset-reader` tests a diagnostic correction in a temporary Go
dependency copy; it changes neither product dependencies nor the module cache.
A passing diagnostic does not establish compatibility with the unchanged peer.

The QUIC stack uses an experimental Noq commit and local HTTP/3 patches; see
[vendor/PATCHES.md](vendor/PATCHES.md) for provenance and limitations.
