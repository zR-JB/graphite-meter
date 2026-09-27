# Experimental Rust implementation

The server executable is under development; it is not yet a drop-in replacement.
Go remains the default. The native Rust TUI is runnable, including latency,
download, upload, and bidirectional stages. With multiple servers, a failed
transfer server leaves later stages while surviving servers continue. A sole
server is prepared again for the next stage; prior results retain their failure
and partial evidence. Latency observations and results remain separate for each server;
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

Run `mise run rust-client-run -- --url https://your-server` to open the experimental TUI.
Start test is focused initially. Tab/Shift-Tab changes focus, arrows adjust
settings, and Space toggles stages. Advanced exposes stream and timing settings.
Press `d` for Details, `l` to change the displayed latency server, `?` for help,
and `q` to quit. Esc asks to stop an active test; Enter runs again after it ends.
Pass `--report` for a single run without the TUI; redirected output also uses
report mode. Completion exits 0, a failed or incomplete run exits 1, and signals
exit 130 (interrupt) or 143 (terminate). The carbon palette follows `COLORFGBG` when available
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

`mise run rust-server-run` builds the browser UI and runs the experimental server
using `GM_*` configuration or server flags. HTTP/1, HTTPS/WSS, HTTP/2, and
HTTP/3/WebTransport listeners share authentication and measurement state.
Password, OIDC, and hybrid authentication are implemented. OIDC has been checked
against a local signed-token provider and a temporary HTTPS Keycloak realm,
including allowed and denied group membership. Other deployments remain untested.

The server shares one buffer budget across QUIC and HTTP/2 listeners,
configured by `GM_MAX_BUFFER_BYTES` or `--max-buffer-bytes` (default 8 GiB).
QUIC charges bytes when they are buffered instead of reserving connection
windows up front. From accept until its task ends, a QUIC connection holds a
floor of 80 KiB per stream the peer may open, covering one maximal HTTP/3 frame
and copy block outside Noq: 67 streams and 5.2 MiB with the default limits.
HTTP/2 still reserves 36 MiB before each TLS handshake; that reservation
outlives its connection, local stream futures, and buffers. Once a quarter of
either connection capacity or the budget is used, unvalidated QUIC handshakes
require Retry. A connection whose floor or reservation does not fit is refused
while established connections continue.

Noq charges its receive reassembly, send buffers, packet and control metadata,
datagrams, and queued incoming packets to the same budget as they fill, within
unchanged per-connection caps. A refused charge closes only that connection with
`INTERNAL_ERROR`. Returned byte slices keep their charge until their last owner
drops, including after connection destruction. Until the connection has an
admitted operation or session, its receive window is 64 KiB, so a silent or
unauthenticated peer can make Noq hold at most 192 KiB of reassembly. Admitted
work raises the window to 16 MiB, or less when a third of the free budget is
smaller, and it returns to 64 KiB after the last admitted operation ends.
Credit already granted stays usable until it is consumed. Transmit windows grow
from 2 MiB to 32 MiB only into budget that is free at that moment. Endpoint
reservations cover the configured UDP socket buffers, receive batches and
pending incoming packets until the socket and its senders drop. Additional
incoming packets are capped at 64 KiB per handshake and 4 MiB per endpoint. The
shared 256 KiB download block is charged once.

This accounting does not establish a resident-memory bound. Payload backing
allocations, header decoding and metadata, TLS state, and allocator overhead still
need worst-case accounting and sustained-load measurements before the
experimental merge. The many-client performance matrix remains unfinished.

Origins require ASCII hosts; use punycode for international names. Empty host
labels, host punctuation other than hyphens and underscores, IPv4 shorthand,
leading-zero IPv4 octets, and trailing-dot IPv4 addresses are rejected. Domain
trailing dots remain supported. Punycode labels are passed to DNS as ASCII,
without IDNA decoding. OIDC issuer paths must be ASCII; percent-encode other
characters. HTTP and WebSocket clients use the Go proxy environment rules with
an ALL_PROXY fallback: loopback bypasses proxies, NO_PROXY supports ports,
and a leading dot matches subdomains only. Cleartext HTTP uses absolute-form
requests; HTTPS uses CONNECT. SOCKS proxies are rejected.

OIDC verifies RS/PS 256–512, ES256/384 and EdDSA with ring. HS*, none and ES512
are rejected; RSA keys must be 2048–8192 bits. ID tokens require the configured
issuer, one audience, expiry, issued-at and nonce. Present azp and at_hash claims
must match. Unknown signing keys trigger one coordinated JWKS refresh.

Authentication forms require URL-encoded POST bodies with unique fields. Unlike
Go's form parser, Rust does not accept passwords or CSRF proofs from URL queries.

The workspace pins Rust 1.98.1. From the repository root:

```sh
mise run rust-check
mise run rust-format
python3 rust/tests/server_interop.py
python3 rust/tests/client_interop.py
python3 rust/tests/interop.py
```

Ring is the TLS/QUIC crypto provider. `rust-check` enforces dependency policy with
`cargo deny --locked check` (cargo-deny 0.20.2); a daily workflow rechecks advisories.
It also limits the Linux production graph to 135 server crates and
164 client crates, including each binary’s root crate.
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
measurement engine through all four stages with WebTransport streams and datagram latency,
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

The server uses exact revisions of the [Noq](https://github.com/zR-JB/noq),
[HTTP/3](https://github.com/zR-JB/h3) and [h2](https://github.com/zR-JB/h2) forks.
[Fork provenance](../legal/rust-forks.json) records upstream bases, reviewed
revisions and each commit's purpose. `rust-check` validates locked sources offline;
`scripts/legal/check_git_sources.py --verify` checks fork branches, upstream tags
and diffs. [Fork upkeep](../legal/README.md#pinned-fork-upkeep) covers updates.
The shared `webtransport` crate owns association-preserving cancellation, with
plain RESET fallback when peers do not support reliable reset. The forks remain
experimental; current-codepoint tests do not establish Safari compatibility.
