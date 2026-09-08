# v0.8.1 deployment validation

This is the recorded local deployment pass for [issue #183](https://github.com/zR-JB/graphite-meter/issues/183),
performed on 2026-09-08 against **`f6f32a3f51b229c4b45fae67886e41eb6f6e58cc`**.
It exercises the production embedded application and real server processes. It is not a
claim of Safari, physical mobile, public internet, or VPN coverage, and does not by itself
close the issue. The reproduced defects and remaining coverage are listed below.

The checked-in [results](validation/0.8.1/deployments.json) contain stage outcomes,
receiver-clock evidence, observed routes and protocols, permission results, failure labels,
and sanitized configuration. The [generated NPM configuration](validation/0.8.1/npm.conf)
and mobile screenshots of [keyboard focus](validation/0.8.1/mobile-chromium.png) and
[long labels](validation/0.8.1/mobile-long-labels.png) accompany them.
Loopback transfer rates are only evidence that bytes were carried, not performance claims.

## Environment and topology

- Linux x86-64, WSL2 kernel `6.18.33.2-microsoft-standard-WSL2`.
- Production client and server built with repository-pinned Bun 1.4.2 and Go 1.27.1,
  using `mise run server-build-prod` in a clean worktree at the revision above.
- Google Chrome **152.0.7977.82**, headless; Firefox **155.0**, Playwright's matching
  Linux build `1543`; Playwright 1.63.0. An initial incompatible Firefox 153 build
  was discarded after an automation protocol assertion, before collecting Firefox results.
- Docker Engine **29.7.2**; Nginx Proxy Manager **2.15.1**, OpenResty **1.29.2.5**,
  image `jc21/nginx-proxy-manager@sha256:52b2c59994f3d36acfcf70a1626f29734df0ed8c71bacc0269f78b6f939858bb`.
- A temporary CA issued a two-day certificate for `localhost`, `meter.gm183.test`,
  `peer.gm183.test`, `127.0.0.1`, and `::1`. Only temporary Chrome/Firefox profiles
  trusted this CA. HTTPS certificate verification, mixed-content checks, CSP, CORS,
  and local-network permission remained enabled. No managed browser policy was configured.
  Browser automation supplies its usual process/automation flags; it does not constitute
  an interactive user clicking a native permission prompt.

| Fixture | Listeners and advertised paths |
| --- | --- |
| Direct/self | Clear HTTP `:18310`, H1 TLS `:18311`, H2 `:18312`, H3 TCP+UDP `:18313`; all native endpoints advertised. Catalogue includes the TLS-only peer. |
| TLS-only peer | Clear administrative listener `:18320`; H1 TLS `:18321`; only `http1-tls` advertised. |
| NPM | `https://meter.gm183.test:18443` → Docker bridge `172.17.0.1:18330`; backend advertises no native endpoints and `GM_PUBLIC_ORIGINS=self`. |
| Public/private permission fixtures | Hostname mappings to removable local interface addresses: `meter.gm183.test` → `203.0.113.183`, `peer.gm183.test` → `192.168.183.1`; HTTPS `:18341` and `:18351`, plus loopback peer `:18321`. Chrome reported **Public**, **Local**, and **Loopback** address spaces. |
| Failure/restart fixtures | Two independent processes on HTTPS `:18361` and `:18371`; SIGKILL interrupts live transfers without graceful draining. |
| Egress-blocked fixtures | Clear HTTP `:18380` and `:18390`; dedicated synthetic UID 60183; nftables rejects new outbound connections from that UID, while established replies and the browser's own connections remain allowed. |

The public/private addresses exercise real browser address-space enforcement but are
assigned to the local host. They do not traverse a public ISP, LAN access point, VPN,
or mobile network. Hostname mappings are not a DNS-server/AAAA validation.

## Measurement and proxy results

Each ordinary run enables latency, download and upload, disables adaptive early finish,
uses one stream per direction, and uses 250 ms warmup, 1 s idle latency and 1.5 s transfers.
History saving is enabled. Selection and transport preferences are written before page load
using the application's existing storage format. Start is pressed after initial discovery
settles; the immediate-Start exception is recorded under defects.

| Scenario | Expected | Observed |
| --- | --- | --- |
| Clear HTTP interface; Automatic throughput; clear self + TLS-only peer | Resolve origins independently; both carry download and receiver-authoritative upload | **Pass, Chrome and Firefox.** Two participants; each upload interval uses `clock: receiver`; all three stages complete. |
| HTTPS direct interface | Load trusted HTTPS UI and complete Automatic measurement | **Pass, Chrome and Firefox.** Loopback Automatic can use the clear native listener; this case alone does not prove TLS transfer. |
| Forced direct H1 TLS and H2 | Carry bytes on the requested protocol | **Pass, Chrome.** H1 TLS and H2 recorded separately, with complete upload and `serverAuthoritative: true`. |
| NPM HTTPS proxy | H2 browser hop, H1 upstream; upgrades and complete streaming/checkpoint route family | **Pass, Chrome and Firefox.** Browser Resource Timing reports `h2`; `/probe` and saved server protocol report `http/1.1`. Download, upload and latency complete. |
| Same-origin IPv6 literal `http://[::1]:18310` | CSP `'self'` permits measurement | **Pass, Chrome and Firefox.** All stages complete; authoritative upload. |
| Native IPv6 literal | Native client carries download/upload and latency | **Pass.** Same-revision native executable completed via `http://[::1]:18310`, Automatic H1 fetch and H3 WebTransport latency; final upload marked `server-clock`. |
| Reachable H3/WebTransport | Forced streams and latency use H3 and carry real bytes | **Pass, Chrome, with transport-test certificate accommodation described below.** Resource Timing `h3`; history records WebTransport for both roles and authoritative upload. |
| UDP `:18313` rejected | Automatic falls back; forced H3/WT must not silently switch protocol | **Automatic passes** via H1 fetch + WebSocket after discovery settles. **Forced paths carry no measurement traffic.** Their hidden Start refusal is the reproduced feedback defect below. Firewall counted rejected packets. |
| WebTransport unavailable | Automatic still completes with supported transports | **Pass, Firefox with `network.webtransport.enabled=false`.** This is an explicitly disabled-feature case, not a claim that default Firefox 155 lacks WebTransport. |
| Server-to-server egress blocked | Catalogue and both measurements need only direct client connections | **Pass, Chrome.** A same-UID outbound curl failed with exit 7; firewall counted 3 rejected packets/220 bytes. Both selected servers completed, with authoritative upload. Rules and fixture processes were removed in `finally` cleanup. |

Chrome accepted the local CA for normal HTTPS but rejected it on the QUIC path with
`QUIC_TLS_CERTIFICATE_UNKNOWN`. Only the dedicated H3 transport cases used
`--ignore-certificate-errors-spki-list=<temporary certificate SPKI>` and
`--origin-to-force-quic-on=localhost:18313`. They establish protocol/UDP behavior,
not production public-CA trust or ordinary browser permission behavior. The permission
matrix did not use either flag.

NPM was configured through its normal API, corresponding to a Proxy Host with WebSocket
support enabled, HTTP/2 enabled, caching disabled, the uploaded temporary certificate,
and the full `location /` in its **Advanced** field. Its generator omitted the default
location; `nginx -t` passed with one location. No Custom Locations entry was added.
The generated configuration preserves `$http_host` including `:18443`, sets forwarding
headers, disables response/request buffering, permits unlimited upload body size, and
sets both proxy timeouts to 300 seconds. `/upload/session`, `/upload/progress`, upload
data, and checkpoint traffic reached the same backend. Default NPM uses `$host`,
which removes a nonstandard port; placing forwarding headers only at server level
does not override NPM's generated location headers.

## Browser permissions, failures and presentation

| Scenario | Expected | Observed |
| --- | --- | --- |
| Public → loopback and public → private | Deny/revoke blocks; grant/regrant permits retry | **Pass, Chrome.** CDP set each site's permission to denied, granted, denied, granted. Failed requests report `LocalNetworkAccessPermissionDenied`; granted requests return HTTP 200 from the selected server. |
| Public interface measures self, private and loopback together after grant | Resolve all three independently and complete authoritative upload | **Pass, Chrome.** Three participants, all stages complete, and per-participant upload intervals use receiver clocks. This is a real application run on the representative local address-space topology. |
| Private → loopback | Record actual browser policy rather than assume a prompt | **Observed allowed, Chrome.** All four permission settings returned HTTP 200. Do not claim this Chrome version requires a grant for that path. |
| Cross-origin IPv6 literal, including another port | Reject with DNS-hostname remedy | **Pass, Chrome.** Selecting `http://[::1]:18310` from the HTTPS public-address interface shows “Use a DNS hostname for browser connections to this IPv6 server.” No IPv6-literal host source is inserted into CSP. |
| HTTPS interface → clear public peer | Refuse incompatible path with actionable HTTPS guidance | **Refused, feedback defect reproduced.** At this revision the selected peer shows only “Server could not be reached” and Retry. |
| Peer stopped before preparation | Bounded failure; no fabricated transfer | **Observed error within 6.5 s of fixture setup.** Zero transfer bytes; Run Again is offered; no new history record. |
| Peer stopped during download | Preserve observations and survivor membership | **Pass.** Run completes with a partial saved outcome, “1 of 2 servers”, peer upload failure, and positive receiver-authoritative survivor upload. |
| All servers stopped during download | Preserve collected observations, mark incomplete and persist the incomplete result when history is enabled | **Persistence defect reproduced.** UI ends Complete with “0 of 2 servers” and two upload failures; it retains observed download/ping and omits upload results. A fresh profile still has no saved record after ten seconds because the deferred history repository module cannot load from the dead primary server. |
| Cancel during download | Stop boundedly and exclude history | **Pass.** Phase becomes `aborted`, shows Test aborted/Run Again; no new saved record. |
| Primary/all-server latency toggle before run | Reuse healthy discovery evidence | **Pass.** Toggle primary then all; combined preflight/probe response count stays 6 → 6. The subsequent two-server run completes. |
| Failed discovery, retry, restart | Retry refreshes metadata; reload discovers new generation/identity | **Pass.** An absent peer shows Retry; starting it and retrying reveals `restarted-peer`. Replacing its process and reloading reveals `upgraded-peer` and a different generation. This tests same-binary restart/identity change, not an old-version upgrade. |
| Mobile viewport, touch, reduced motion, keyboard focus | Controls fit and remain operable | **Pass, emulated Chrome.** 390×844, touch points 1, reduced-motion media query true; measured document width 390 with no horizontal overflow. Settings/selection remain operable; keyboard focus has a visible solid outline. Computed selector animation and transition durations are `0.001 ms` with reduced motion. Screenshots retain focus and long-label truncation. No physical-device or animation-frame performance claim. |

The desktop runs include an extended operator server name; live and saved selected-server
identities are retained in the evidence. Preflight request times stay in selection controls,
with the explicit tooltip that connection/setup time is not steady-state ping. The latency
measurements and saved receiver-clock intervals are separate values.

CSP/CORS were observed without modification: public-page `connect-src` contains `'self'`,
the configured catalogue hostnames with supported schemes/ports, and the native HTTPS/WSS
origin. It does not admit arbitrary hostnames or cross-origin IPv6 literals. Unauthenticated
measurement endpoints retain their existing `Access-Control-Allow-Origin: *` behavior;
this pass does not claim a restrictive authenticated CORS policy. The exact headers are
retained in the results file.

## Reproduced defects and remaining coverage

1. Pressing Start before initial catalogue readiness, or pressing Start on a forced path
   that failed validation, leaves the gauge at Idle/Ready with no measurement. The refusal
   is hidden instead of actionable. Reproduced against real servers; addressed separately
   in [PR #186](https://github.com/zR-JB/graphite-meter/pull/186).
2. HTTPS-to-clear-public discovery returns generic unreachable feedback instead of the
   required HTTPS remedy. The browser correctly blocks the request. A focused origin
   restriction correction accompanies this validation work.
3. On a fresh page, stopping all servers during download prevents the first history save:
   importing the deferred repository module fails with `ERR_CONNECTION_REFUSED`. The
   same scenario with that existing module explicitly preloaded saves an `incomplete`
   record. This diagnostic separates an asset-loading dependency from result validation;
   no validator was weakened. A persistence-loading correction accompanies this work.

### Integrated fix confirmation

The reproduced defects were retested on production revision
**`9ed8e5424680198e1a4964a56367a27ad59ca107`**, using binary SHA-256
`f40216745cf18eafe4a88a9700f7ff3ceacd9a50b04459fb415a99dbb8deb1e5`.
This was a normal production build without diagnostic instrumentation. Chrome 152 and
the same temporary CA/topology were used; the initial matrix revision remains preserved
above rather than relabelling earlier observations as tests of the new revision.

| Retest | Observed on the integrated revision |
| --- | --- |
| Start during catalogue loading | **Pass.** A 1.5 s delay on the real `/servers` request makes the condition reproducible. The gauge shows “Test cannot start” and “Servers are still loading. Try again in a moment.” |
| HTTPS interface selects a clear public peer | **Pass.** Shows “Use an HTTPS origin for this server when the interface is HTTPS.” No clear-peer preflight request is issued. |
| Forced WebTransport/H3 with UDP rejected | **Pass.** Both show “Connection check failed” and “Throughput path is unavailable”; zero measurement requests. No silent fallback. The temporary firewall counted 4 rejected packets/5112 bytes and was removed. |
| Both servers killed during download, fresh profile | **Pass.** One saved record with `outcome: incomplete`, no surviving participants; UI labels the result “Incomplete”. History saving works without fetching a deferred repository chunk after server loss. |
| One server killed during download | **Pass.** One saved record with `outcome: partial`, surviving participant `self`; UI labels the result “Partial”. |

The [results file](validation/0.8.1/deployments.json) retains the terminal text and persisted
outcomes for these retests. Broader browser/proxy measurements were not relabelled or
repeated merely because the documentation changed.

**Untested:** genuine Safari/macOS/iOS, physical mobile/touch hardware, actual public/LAN/VPN
deployments, DNS served with AAAA records, routed/global IPv6, native non-loopback IPv6,
interactive permission-prompt clicks or managed browser policies, production public-CA
WebTransport trust, multi-backend load-balancer affinity, and an upgrade from an older
released server binary. Authentication/sign-in controls are covered separately by issue #184.
No general release-readiness conclusion is implied for these untested cases.
