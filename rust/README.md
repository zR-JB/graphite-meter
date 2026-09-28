# Experimental Rust implementation

The server executable is under development; it is not yet a drop-in replacement.
Go remains the default. The native Rust TUI is runnable, including latency,
download, upload, and bidirectional stages. With multiple servers, a failed
transfer server leaves later stages while surviving servers continue. A sole
server is prepared again for the next stage; prior results retain their failure
and partial evidence. Latency observations and results remain separate for each server;
the run's latency is the first selected server's, and `l` changes the displayed server.
Full parity validation is unfinished.
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
expands it only for `--legal`; the archive also carries it as `THIRD_PARTY_NOTICES.txt`.

Run `mise run rust-client-run -- --url https://your-server` to open the experimental TUI.
Start test is focused initially. Tab/Shift-Tab changes focus, arrows adjust
settings, and Space toggles stages. Advanced exposes stream and timing settings.
Press `d` for Details, `l` to change the displayed latency server, `?` for help,
and `q` to quit. Esc asks to stop an active test; Enter runs again after it ends.
Pass `--report` for a single run without the TUI; redirected output also uses
report mode. Completion exits 0, a failed or incomplete run exits 1, and signals
exit 130 (interrupt, or Ctrl-Break on Windows) or 143 (terminate, or a closed
Windows console). The graphite palette of Go's TUI follows `COLORFGBG` when available,
otherwise the background the terminal reports to Go's OSC 11 query (not on Windows),
and adapts to truecolor, 256-color, or ANSI terminals. Set `GM_TUI_THEME=light`
or `dark` to override the background choice. `NO_COLOR` disables color.
The TUI can connect to either implementation's server.

`mise run rust-client-package VERSION` builds the experimental Linux and Windows
TUI archives in the pinned builder image, with reviewed dependency notices and
matching source; the Linux TUIs and the server image's binary are static musl executables, like Go's. Release requests can
opt into Rust TUI archives for every platform the Go TUI ships, named like Go's
with a `_rust` marker, and into a linux/amd64 + linux/arm64 server image tagged
`VERSION-rust`; stable releases move `X.Y-rust` and `latest-rust` with it. These
tags share `ghcr.io/zr-jb/graphite-meter` with Go's, so semver-range updaters
such as Flux, Argo CD Image Updater or WUD read `X.Y.Z-rust` as a prerelease of
Go's `X.Y.Z` and may skip it or move a Rust pin to Go; follow `latest-rust`, or
match tags on `-rust$` and compare the version before that suffix. Every
binary has a matching source offer. Go remains the release default; Rust
prerelease integration remains gated.
The experimental container uses `container/Dockerfile.rust`, which
`mise run rust-container-build` builds as `graphite-meter:latest-rust`. That
task and `rust-client-package` need an amd64 build host: the reviewed platform
notices record its toolchain, which also cross-compiles arm64. The port remains
blocked from merging until a human decides its design.

`mise run rust-server-run` builds the browser UI and runs the experimental server
using `GM_*` configuration or server flags. HTTP/1, HTTPS/WSS, HTTP/2, and
HTTP/3/WebTransport listeners share authentication and measurement state.
Password, OIDC, and hybrid authentication are implemented. OIDC has been checked
against a local signed-token provider and a temporary HTTPS Keycloak realm,
including allowed and denied group membership. Other deployments remain untested.

The server shares one buffer budget across QUIC and HTTP/2 listeners,
configured by `GM_MAX_BUFFER_BYTES` or `--max-buffer-bytes` (default 8 GiB).
It must cover `GM_MAX_CONNECTIONS` times the larger connection floor of the
enabled HTTP/2 and HTTP/3 listeners below, the QUIC endpoint buffers when HTTP/3
is enabled and the shared download block; HTTP/1 connections hold no floor.
Configuration checks what it knows; loading or reloading the certificate chain
and binding the QUIC socket check the actual terms, and a reload that does not
fit keeps the previous chain. The default budget covers Go's default of 4096
connections with every listener enabled, also with per-client limits raised to
the totals.
QUIC charges bytes when they are buffered instead of reserving connection
windows up front. From accept until Noq drops the connection, a QUIC connection
holds a floor for the TLS handshake, five server flights of the loaded
certificate chain and a copy of it, and for the HTTP/3 layer's fixed state
(2.6 KiB), plus the floor Noq charges itself for the streams a client may open,
481 KiB at the default limits and 976 KiB with per-client limits at the totals:
about 0.5 MiB with a two-certificate chain and default limits. The layer
retains no payload and charges each request's and session's own state as it
arrives; a request the budget cannot cover is refused with
`H3_REQUEST_REJECTED`, while running transfers finish from their floors.
An HTTP/2 connection holds a 1.5 MiB floor from accept until its task ends:
512 KiB of TLS and codec buffers and a 1 MiB allowance for decoded headers,
buffered DATA frames and queued response metadata, charged as they fill. A
request's headers stay charged until its stream future and queued trailers are
gone; past the allowance, requests are refused and DATA resets its stream. The
receive window stays 64 KiB until an admitted upload reads, then grows to
16 MiB while the budget is under three quarters used, charged until the peer can
no longer fill it. Under pressure uploads continue at their current window.
An HTTP/2 or QUIC connection that raised its window closes once no admitted
download, upload, progress stream or WebTransport session has run on it for 15
seconds. Unadmitted requests never extend that period; those in flight get five
seconds, even if the peer withholds flow control, while admitted work that raced
the GOAWAY runs to its own limits.
Once a quarter of either connection capacity or the budget is used, unvalidated
QUIC handshakes require Retry. A connection whose floor does not fit is refused
while established connections continue.

Noq's floor holds the state of every stream the peer may open, 64 KiB for local
streams and 64 KiB for each of its five buffer pools. Beyond it, Noq charges its
receive reassembly, send buffers, packet and control metadata, datagrams,
handshake data and queued incoming packets to the same budget as they fill,
within unchanged per-connection caps. A refused data charge slows that
connection and a local stream open waits; refused control metadata beyond the
floor, or a floor that does not fit, closes only that connection with
`INTERNAL_ERROR`. Returned byte
slices keep their charge until their last owner drops, including after
connection destruction. Until an admitted upload reads
on the connection, its receive window is 64 KiB, so a silent or unauthenticated
peer can make Noq hold at most 192 KiB of reassembly. The first grant reserves
the rest of Go's 48 MiB connection window until Noq drops the connection, and
Noq charges the connection's buffers to it first. The window never shrinks, so
the peer never holds more credit than is reserved; streams get Go's 32 MiB.
Transmit windows adapt between 2 MiB and 48 MiB. Neither window grows once three
quarters of the budget is used, so pressure slows new transfers instead of
closing running ones; one log line reports when growth is held back, and one
when usage falls below five eighths again. Endpoint
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
requests; HTTPS uses CONNECT. As in Go, `socks5://` and `socks5h://` proxies
both pass host names to the proxy and log in with the URL's user and password.

OIDC verifies RS/PS 256–512, ES256/384 and EdDSA with ring. HS*, none and ES512
are rejected; RSA keys must be 2048–8192 bits. As with go-oidc, ID tokens require
the configured issuer, an audience that includes the client ID, expiry and the
nonce; issued-at is optional, azp is not read, and a present at_hash must match.
Unlike go-oidc, tokens over 16 KiB, cty, crit or enc headers and a typ other than
JWT or JOSE are rejected, and signed user information must name the issuer and
the client. Unknown signing keys trigger one coordinated JWKS refresh.

Authentication forms require URL-encoded POST bodies with unique fields. Unlike
Go's form parser, Rust does not accept passwords or CSRF proofs from URL queries.
A CLI approval page opened by another login is refused at once, where Go shows
the page and then refuses its approval.

The workspace pins Rust 1.98.1. From the repository root:

```sh
mise run rust-check
mise run rust-format
python3 rust/tests/server_interop.py
python3 rust/tests/client_interop.py
python3 rust/tests/interop.py
python3 rust/tests/browser_transports.py --server rust/target/debug/graphite-meter-server
```

Ring is the TLS/QUIC crypto provider. `rust-check` enforces dependency policy with
`cargo deny --locked check` (cargo-deny pinned in `mise.toml`); a daily workflow rechecks advisories.
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

`browser_transports.py` drives Chromium 153 and Playwright's Firefox 153 build
through HTTP/3 downloads and uploads and WebTransport datagrams, streams and the
close codes of sessions the server ends at their lifetime and when it stops;
WebKitGTK, which has neither, is the negative control. It binds fixed loopback
ports, so run it in a private network namespace. The Go server passes the same
checks.

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

The workspace uses exact revisions of the [Noq](https://github.com/zR-JB/noq)
and [h2](https://github.com/zR-JB/h2) forks.
[Fork provenance](../legal/rust-forks.json) records upstream bases, reviewed
revisions and each commit's purpose. `rust-check` validates locked sources offline and
rejects Rust legal reviews of crates no shipped binary compiles
(`python3 -m scripts.legal.check_rust_reviews --prune` drops them) or in another layout
than the legal tools write (`--format` rewrites them);
`scripts/legal/check_git_sources.py --verify` checks fork branches, upstream tags
and diffs. [Fork upkeep](../legal/README.md#pinned-fork-upkeep) covers updates.
The workspace's `http3` crate, shared by the server and the client, keeps a
cancelled stream's association header, with plain RESET fallback when peers do
not support reliable reset. The forks remain experimental; current-codepoint
tests do not establish Safari compatibility.
