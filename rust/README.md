# Experimental Rust implementation

The server executable is under development; it is not yet a drop-in replacement.
Go remains the default. The native Rust TUI is runnable, including latency,
download, upload, and bidirectional stages. A failed transfer server is removed
from subsequent stages while surviving servers continue; affected results stay
marked partial. Latency observations and results remain separate for each server;
press `l` to change the displayed server. Full parity validation is unfinished.
Preliminary footprint measurements are promising; feature-complete performance
and size comparisons have not been established.

Run `mise run rust-client-run` to open the experimental TUI, or pass
`-- --url https://your-server`. Press `r` to run, `?` for help, and `q` to quit.
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
against a local signed-token provider; deployment-provider validation is pending.

Authentication forms require URL-encoded POST bodies with unique fields. Unlike
Go's form parser, Rust does not accept passwords or CSRF proofs from URL queries.

The workspace pins Rust 1.98.1. From the repository root:

```sh
mise run rust-check
mise run rust-format
python3 rust/tests/server_interop.py
python3 rust/tests/interop.py
```

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
