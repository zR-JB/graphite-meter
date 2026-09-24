# Experimental Rust implementation

The server executable is under development; it is not yet a drop-in replacement.
Go remains the default. The native Rust TUI is runnable, including latency,
download, upload, and bidirectional stages. A failed transfer server is removed
from subsequent stages while surviving servers continue; affected results stay
marked partial. Latency observations and results remain separate for each server;
press `l` to change the displayed server. Full parity validation is unfinished.
An adaptive HTTP/3 send window reduced Rust peak memory versus a fixed-window
Rust build. A separate Go/Rust HTTP/3 batch still showed higher Rust CPU and
peak memory. A matched WebTransport stream-download run showed lower Rust server
memory and CPU but lower throughput than Go. Short WebTransport stream-upload
and datagram runs found similar or higher Rust receiver throughput with lower
server CPU and memory. Simulated 100 ms RTT runs
exposed fixed QUIC receive-window limits in the Rust server and TUI; larger
bounded windows improved those runs. These short local and delayed-path samples
do not establish real-WAN, packet-loss, many-user, or sustained-memory superiority.
The release server embeds one reviewed third-party notice payload for both
`--legal` and the browser About endpoint.
The packaged TUI keeps its reviewed notice compressed inside the executable and
expands it only for `--legal`; the archive also carries the readable `LEGAL.txt`.

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
python3 rust/tests/client_interop.py
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
libraries: bootstrap, HTTP/3 transfers, WebTransport pings, stream and datagram
downloads/uploads, receiver-counted upload progress, independent connections,
and rejection of excess WebTransport sessions. A password-protected loopback
replay checks cookie-authenticated HTTP/3, one-use WebTransport tickets, and
logout revocation. The same harness simulates the browser approval forms and
runs all four stages through the actual Go native measurement engine over
WebTransport. It does not exercise the Bubble Tea interface or an external
authenticated deployment. `interop.py` separately probes low-level transport
behavior against both unchanged quic-go and a disposable build that offers
only the current reliable-reset transport parameter. The latter checks QUIC
negotiation, WebTransport transfers, and immediate reset without modifying the
shipped Go implementation.

`client_interop.py` starts an unchanged Go product server and runs the Rust
measurement engine through all four stages with WebTransport streams, datagrams,
HTTPS HTTP/1.1 fetch streams, and HTTP/2 fetch streams. The loopback test
trusts only its disposable CA through `SSL_CERT_FILE`, and verifies
TLS identity on HTTP and QUIC connections. It does not exercise an external
identity provider or real deployment.

The full probe includes immediate WebTransport stream reset against the unchanged
Go client using quic-go v0.63.0. It requires the session association prefix to
survive reset without a temporary dependency overlay. Earlier quic-go v0.62.0
lost the final prefix byte when a read returned that byte with a reset error.
The current-only Go probe exercises the draft-09+ transport parameter; it is
not evidence of Safari browser parity.

The QUIC stack uses an experimental Noq commit and local HTTP/3 patches; see
[vendor/PATCHES.md](vendor/PATCHES.md) for provenance and limitations.
