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
`--legal` and the browser About endpoint. As Go's TUI report does, `--legal`
opens with the project's copyright, its source (the release tag's tree for a
release version) and the LICENSE, and the browser's About links that source.
The packaged TUI keeps its reviewed notice compressed inside the executable and
expands it only for `--legal`; the archive also carries it as `THIRD_PARTY_NOTICES.txt`.

Run `mise run rust-client-run -- --url https://your-server` to open the experimental TUI,
which follows Go's views and keys. ↑/↓ move between setup rows, ←/→ change a value,
Enter opens or edits a row, Space switches a stage on or off, and `r` starts the test
from any row; `v` checks the paths again, `s` chooses servers, `a` makes every path
automatic and `u` keeps the servers that are ready. During a run `d` shows Details,
`l` changes the displayed latency server and Esc asks to stop it; afterwards Enter
runs again and Esc returns to setup. `?` lists every key, and `q` quits.
Pass `--report` for a single run without the TUI; redirected output also uses
report mode. Completion exits 0, a failed or incomplete run exits 1, and signals
exit 130 (interrupt, or Ctrl-Break on Windows) or 143 (terminate, or a closed
Windows console). The graphite palette follows the background the terminal reports
to Go's OSC 11 query (not on Windows), in the colour profile Go's colorprofile picks
from `TERM`, `COLORTERM`, `NO_COLOR`, `CLICOLOR` and `CLICOLOR_FORCE`.
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
notices record its toolchain, which also cross-compiles arm64. The builder installs
its exact cross-compiler versions from snapshot.debian.org at a fixed timestamp, so
a Debian point release changes neither them nor their dependencies. Like Go's image,
the Rust image takes its CA roots and their copyright from the pinned Go builder, and
its `THIRD_PARTY_NOTICES.txt` lists them after the binary's notices. The port remains
blocked from merging until a human decides its design.

`GM_IMPLEMENTATION=rust` switches `mise run dev`, `prod` and `tui` from Go to the
experimental server or TUI. `mise run rust-server-run` builds the browser UI and runs
the experimental server using `GM_*` configuration or server flags after `--`;
`rust-server-build` builds a release binary with the production UI. They, `rust-client-run` and
`rust-client-build` build on any host with [development notices](../legal/README.md#rust-platform-records);
the builder image produces the reviewed release binaries. HTTP/1, HTTPS/WSS, HTTP/2, and
HTTP/3/WebTransport listeners share authentication and measurement state.
Password, OIDC, and hybrid authentication are implemented. OIDC has been checked
against a local signed-token provider and a temporary HTTPS Keycloak realm,
including allowed and denied group membership. Other deployments remain untested.
Log lines carry UTC timestamps, where Go's use the host's time zone.

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
As in Go, every exchange on every listener has 15 seconds in all, request and
reply, until it is admitted as a measurement and its operation's lifetime takes
over. A write of an HTTP/2 or HTTP/3 reply that the peer's flow control holds
for 30 seconds ends the reply, as Go's idle writer ends a download; Rust also
applies this to progress streams, which Go bounds only by their lifetime.
Once a quarter of either connection capacity or the budget is used, unvalidated
QUIC handshakes require Retry; as in Go, so does one from a source whose IPv4
address or IPv6 /64, /56 or /48 already holds a QUIC connection. A connection
whose floor does not fit is refused while established connections continue.
A QUIC handshake has 10 seconds in all, as in quic-go, which also ends one after
5 seconds without a packet; Noq cannot tell that, so a silent client holds its
handshake for the full 10 seconds.

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
Transmit windows adapt between 2 MiB and 16 MiB. Neither window grows once three
quarters of the budget is used, so pressure slows new transfers instead of
closing running ones; one log line reports when growth is held back, and one
when usage falls below five eighths again.
A client's HTTP/2 and HTTP/3 connections together hold raised receive windows
within its share: one 48 MiB HTTP/3 window for each QUIC connection it may hold,
at most eight as in Go, so that each of them can be funded as Go funds every
connection. As in admission, a client is its IPv4 address or IPv6 /64, whose /56
and /48 may hold twice and four times that, or under authentication its login or
grant. An OIDC login's principal may hold twice; all password logins and their
grants share the local operator's principal, so each of them is bounded alone.
The hold-back at three quarters of the budget bounds all clients together, and
their windows together hold at most half of it, so they alone never trigger the
hold-back. Each connection's window counts against the admitted client that
first raised it until the connection closes; past its share, or past that half,
an upload reads at the current window, as under pressure. Endpoint
reservations cover the configured UDP socket buffers, receive batches, the first
packet of each handshake waiting to be accepted and shard forwarding queues until
the socket and its senders drop. They grow with the host's cores, not its load,
so the Retry and hold-back thresholds and the clients' half count only the
budget past them, and an idle server is never under pressure. Additional
incoming packets are capped at 64 KiB per handshake and 4 MiB in all; several
endpoints each take an equal part of the 4 MiB, while every handshake keeps its
64 KiB on the one endpoint its packets reach. The shared 256 KiB download block
is charged once.

The server runs a thread with a current-thread runtime for each worker of its
runtime (`TOKIO_WORKER_THREADS`) and serves each accepted TCP connection on the
next of them in turn, so a connection and its streams stay on one thread. On
Linux, HTTP/3 runs an endpoint on half of these threads, at least two and at
most sixteen, as many as the buffer budget covers; a log line reports fewer.
The cap keeps their reservations a small part of the default budget on hosts
with many cores, which one connection never spreads over, and the endpoints
split quic-go's 7 MiB UDP socket buffers, each keeping 2 MiB at least. On four
workers, two endpoints cost less CPU per byte than one or four, for one fast
client and for eight paced ones. Each endpoint has its own UDP socket on the
shared port, which `SO_REUSEPORT` spreads by 4-tuple, so a QUIC connection
stays on one thread too. Connection IDs begin with their endpoint's index, and
an endpoint forwards short-header packets that name another to it, so a client
whose address changes keeps its connection; handshakes are never forwarded.
Admission, Retry, connection limits and receive-credit shares stay server-wide.
A current-thread server runtime serves every connection itself, and it and
other targets keep one endpoint.

This accounting does not establish a resident-memory bound. Payload backing
allocations, header decoding and metadata, TLS state, and allocator overhead still
need worst-case accounting and sustained-load measurements before the
experimental merge. The many-client performance matrix remains unfinished.

Configured origins require ASCII hosts; use punycode for international names.
Empty host labels, host punctuation other than hyphens and underscores, IPv4
shorthand, leading-zero IPv4 octets, and trailing-dot IPv4 addresses are
rejected. Domain trailing dots remain supported. Punycode labels are passed to
DNS as ASCII, without IDNA decoding. The TUI reads a received catalogue as Go
does: an origin may end in one slash, and international hosts in catalogues and
preflight targets become the punycode Go dials. It converts Latin, IPA, Greek,
Cyrillic, Armenian, Hebrew, Arabic, Georgian, kana, CJK and Hangul letters, and
refuses a label that IDNA would map to other letters, that needs combining
marks, or that mixes writing directions, where Go also accepts those it can
map. Unlike Go, an entry that stays invalid is left out alone instead of
refusing the whole catalogue; selecting it names its fault. `-url` still takes
ASCII hosts. OIDC issuer paths must be ASCII; percent-encode other
characters. HTTP and WebSocket clients read `HTTP_PROXY`, `HTTPS_PROXY` and
`NO_PROXY` as Go does, never `ALL_PROXY`: loopback bypasses proxies, NO_PROXY
compares ports as written, a leading dot or `*.` matches subdomains only, an
international name matches in punycode, converted as catalogue hosts are, an
IPv4-mapped address matches as IPv4, and under CGI a set `HTTP_PROXY` fails
every cleartext request, loopback and NO_PROXY hosts included. A value the client cannot use
fails each request it would carry, naming the variable. Cleartext HTTP uses absolute-form
requests; HTTPS uses CONNECT. TLS to an `https://` proxy skips verification under `-insecure`
as Go's does, but offers no ALPN: a proxy that would choose HTTP/2 speaks HTTP/1.1 to the
client, where Go would write its CONNECT into HTTP/2 and fail, or send cleartext requests
over HTTP/2. As in Go, `socks5://` and `socks5h://` proxies
both pass host names to the proxy and log in with the URL's user and password.

The TUI trusts the roots Go 1.27 would: on Linux the first readable file of
Go's list and every file in its directories, where `SSL_CERT_FILE` replaces only
the file and `SSL_CERT_DIR` only the directories, and a missing one is skipped;
on macOS and Windows the platform verifier, unless either variable is set. The
store loads once, when the first verified TLS connection needs it; cleartext
paths never read it, and a store without roots fails each TLS connection as
not trusted instead of the whole client.

Each path check and run opens connections of its own, as Go's client takes new
transports, and TCP connections probe an idle peer after 30 seconds as Go's
dialer does; a run within 30 seconds of its check keeps that check's
connections. Unlike Go, an HTTP/2 connection that reads nothing for 30 seconds
is pinged and closed when the ping goes unanswered for 20 seconds, and one whose
request gets no response headers within 10 seconds takes no further requests.
A request without a body that fails on a reused connection before its response
is sent once more over a new connection, also a POST, where Go replays only
idempotent requests.

OIDC verifies RS/PS 256–512, ES256/384 and EdDSA with ring. HS*, none and ES512
are rejected; RSA keys must be 2048–8192 bits. As with go-oidc, ID tokens require
the configured issuer, an audience that includes the client ID, expiry and the
nonce; issued-at is optional, azp is not read, and a present at_hash must match.
Unlike go-oidc, tokens over 16 KiB, cty, crit or enc headers and a typ other than
JWT or JOSE are rejected, and signed user information must name the issuer and
the client. User information members match in any letter case, as Go's decoder
matches them; ID token and token response members must be in lower case, where
Go accepts any case. Unknown signing keys trigger one coordinated JWKS refresh.
Discovery also refuses an authorization endpoint off a canonical HTTPS origin,
such as one on port 0, which sign-in pages would name in their form-action;
Go accepts it and renders those pages. It refuses one with a fragment too, after
which Go appends the sign-in query; other endpoints drop theirs, as Go's client does.

`GM_AUTH_PUBLIC_URL` is used in canonical form: `HTTPS://Meter.Example` serves as
`https://meter.example`, where Go keeps the host's spelling and so refuses every
sign-in whose browser sends it in lower case.

Authentication forms require URL-encoded POST bodies with unique fields; another
body fails as malformed, where Go reads no fields and reports a stale form. Unlike
Go's form parser, Rust does not accept passwords or CSRF proofs from URL queries.
A CLI approval page opened by another login is refused at once, where Go shows
the page and then refuses its approval.

The WebSocket handshake checks a request in the order Go's library does, but
keeps two headers HTTP requires where that library omits them: a HEAD upgrade is
refused with `Allow: GET`, and an HTTP/1.0 one with `Upgrade: websocket`.

The workspace pins Rust 1.98.1. From the repository root:

```sh
mise run rust-check
mise run rust-check-targets  # needs gcc-mingw-w64-x86-64-win32 and mingw-w64-x86-64-dev
mise run rust-format
python3 rust/tests/server_interop.py
python3 rust/tests/client_interop.py
python3 rust/tests/interop.py
python3 rust/tests/browser_transports.py --server rust/target/debug/graphite-meter-server
```

Ring is the TLS/QUIC crypto provider. Unlike Go's server, which prefers AES-128-GCM for clients
whose first suite is AES, the server deliberately follows the client's TLS 1.3 suite order, so
OpenSSL-based clients such as curl, which list AES-256-GCM first, get AES-256-GCM, at about 3%
more CPU per byte. `rust-check` enforces dependency policy with
`cargo deny --locked check` (cargo-deny pinned in `mise.toml`); a daily workflow rechecks advisories.
It also limits the crates each static Linux binary compiles, build-time crates and the
binary’s own included, to 141 for the server and 170 for the TUI.
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
checks. CI's Rust browser E2E job runs the Chromium checks (`--browsers chromium
--chromium PATH`) against the image's server.

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
revisions and each commit's purpose. `rust-check` validates locked sources offline,
requires an approved Rust legal review of every crate a shipped binary compiles for any
shipped target, also those only release builds compile, and rejects reviews of crates no
shipped binary compiles (`python3 -m scripts.legal.check_rust_reviews --prune` drops them)
or in another layout than the legal tools write (`--format` rewrites them);
`python3 -m scripts.legal.check_git_sources --verify`, which CI's Rust job runs, checks fork
branches, upstream tags and diffs. [Fork upkeep](../legal/README.md#pinned-fork-upkeep) covers updates.
The workspace's `http3` crate, shared by the server and the client, keeps a
cancelled stream's association header, with plain RESET fallback when peers do
not support reliable reset. The forks remain experimental; current-codepoint
tests do not establish Safari compatibility.
