# Experimental Rust implementation

The server executable is under development; it is not yet a drop-in replacement.
Go remains the default. The native Rust TUI is runnable, including latency,
download, upload, and bidirectional stages. Multi-server recovery, adaptive
stream control, release packaging, and Rust dependency notices are unfinished.
Preliminary footprint measurements are promising; feature-complete performance
and size comparisons have not been established.

Run `mise run rust-client-run` to open the experimental TUI, or pass
`-- --url https://your-server`. Press `r` to run, `?` for help, and `q` to quit.
The TUI can connect to either implementation's server.

Set `GM_IMPLEMENTATION=rust` when running `mise run dev` or `mise run prod` to
select the experimental server. Leaving it unset selects Go.

`mise run rust-server-run` builds the browser UI and runs the experimental server
using `GM_*` configuration or server flags. HTTP/1, HTTPS/WSS, HTTP/2, and
HTTP/3/WebTransport listeners share authentication and measurement state. OIDC
and hybrid authentication are not implemented and fail startup.

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
progress, and independent sessions. It currently covers unauthenticated loopback
traffic. `interop.py` separately probes low-level transport behavior.

The full probe currently fails on immediate stream reset with the unchanged Go
peer. `--fix-go-reset-reader` tests a diagnostic correction in a temporary Go
dependency copy; it changes neither product dependencies nor the module cache.
A passing diagnostic does not establish compatibility with the unchanged peer.

The QUIC stack uses an experimental Noq commit and local HTTP/3 patches; see
[vendor/PATCHES.md](vendor/PATCHES.md) for provenance and limitations.
