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
- Additional engine coverage: **Linux WebKit 26.6**, Playwright build `2359`, in the
  matching Ubuntu 24.04 image
  `mcr.microsoft.com/playwright@sha256:eff16c30e6f3f4af0a03fa4b706120d5e9b0891c344a27d64559aff5900a4a27`.
  This is Linux WebKit, despite its Safari-like user agent; it is not Safari/macOS/iOS.
  Its container trusts only the temporary public CA certificate for these fixtures,
  with no HTTPS-error bypass. `WebTransport` is undefined in this build.
- Docker Engine **29.7.2**; Nginx Proxy Manager **2.15.1**, OpenResty **1.29.2.5**,
  image `jc21/nginx-proxy-manager@sha256:52b2c59994f3d36acfcf70a1626f29734df0ed8c71bacc0269f78b6f939858bb`.
- A temporary CA issued a two-day certificate for `localhost`, `meter.gm183.test`,
  `peer.gm183.test`, `127.0.0.1`, and `::1`. Only temporary Chrome/Firefox profiles
  trusted this CA for the initial matrix. HTTPS certificate verification, mixed-content checks, CSP, CORS,
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
| DNS AAAA fixture | Trusted DNS-over-HTTPS at `https://localhost:18553/dns-query`; actual AAAA response for `peer.gm183.test` is `2001:db8:183::1`, assigned to the local interface. A queries return no address. |

The public/private addresses exercise real browser address-space enforcement but are
assigned to the local host. They do not traverse a public ISP, LAN access point, VPN,
or mobile network. The permission fixture's hostname mappings are distinct from the
separate DNS-server/AAAA measurement below.

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
| Actual DNS AAAA resolution | Resolve a hostname to IPv6, retain normal TLS/CSP checks and complete measurement | **Pass, Firefox.** Its DNS-over-HTTPS resolver queried type 28 for `peer.gm183.test`; the HTTPS run completed with authoritative upload. `/probe` reports `clientIp: 2001:db8:183::1`, `clientIpVersion: 6`. No browser host-resolver mapping was used for this case. |
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

The DNS fixture uses Firefox `network.trr.mode=3`, URI
`https://localhost:18553/dns-query`, bootstrap address `127.0.0.1`,
`network.trr.confirmationNS=skip`, `network.trr.wait-for-portal=false`, and IPv6 enabled.
Its AAAA records have TTL 0 so queries remain visible; it is an authoritative local fixture,
not a recursive resolver, DNS propagation, or public routed-IPv6 test. HTTPS still uses the
trusted hostname certificate. Query counts and receiving-server address evidence are retained.

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
4. The additional Linux WebKit pass on `9ed8e54` found that grouped WebSocket and
   HTTP/1 choices pick the first matching clear-loopback origin even when a matching
   TLS origin exists. WebKit rejects those clear requests from the HTTPS interface.
   Automatic selection and explicit HTTPS/WSS selection complete on the same fixture;
   grouped selection needs to prefer matching secure paths from an HTTPS page.
   The production correction is confirmed below.

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

### Additional engine confirmation

Linux WebKit completed the clear HTTP measurement on `f6f32a3`. On `9ed8e54`, it
completed trusted direct HTTPS with Automatic selection, explicit HTTPS/WSS selection,
and NPM HTTPS proxy measurement. All three stages completed and uploads were
server-authoritative. The NPM browser hop was `h2`, while the measured backend was
`http/1.1`. The grouped-choice defect above was reproduced on this same revision;
the later focused retest below checks the correction independently.

A fresh-profile Linux WebKit retest used production revision
**`aeab964a0c103fa24bffe566cc456570e77f2778`**, binary SHA-256
`603b587089d860c5d2dbbbef618e5e7b02a46561c69e8a4ca2dd51c5fbc37f73`.
All four runs completed latency, download and upload, saved complete history, and
retained receiver-authoritative upload with no page errors:

| Focused retest | Observed |
| --- | --- |
| HTTPS page, grouped WebSocket latency | **Pass.** Every ping socket uses `wss://localhost:18411/ws/ping`; upload uses HTTPS. |
| HTTPS page, grouped HTTP/1 throughput | **Pass.** Upload uses `https://localhost:18411`; explicit WSS latency completes. |
| Clear HTTP page, grouped HTTP/1 and WebSocket | **Pass.** Upload stays on `http://localhost:18410`; ping sockets use `ws://localhost:18410/ws/ping`. |
| NPM HTTPS, Automatic throughput and grouped WebSocket | **Pass.** Browser probe hop is `h2`; measured backend is `http/1.1`; complete streaming upload and saved result. |

Clear-loopback exceptions differ by browser. Chrome and Firefox accepted the exercised
clear native paths from HTTPS; Linux WebKit rejected the grouped clear HTTP/WebSocket
requests. Operators should expose HTTPS/WSS paths for HTTPS interfaces rather than
rely on a clear-loopback exception across browsers.

### Actual upgrade and native IPv6 follow-up

The same-origin upgrade fixture replaced the production binary built from the released
`v0.8.0` source (`f6f32a3f51b229c4b45fae67886e41eb6f6e58cc`) with the production
`aeab964a0c103fa24bffe566cc456570e77f2778` binary. It kept
`https://localhost:18361`, the server name, configuration and Chrome profile unchanged.
After reloading, discovery changed `engineVersion` from `f6f32a3` to `aeab964` and
reported a different generation. Both measurements completed with authoritative upload.
History retained the first result and saved the second with their respective engine
revisions; both selections kept the same `self` ID, URL and name. No page errors occurred.
This is an actual old-release-source to current-source process replacement, using locally
built production binaries, not a downloaded release-package or container migration test.
The evidence retains both binary checksums, discovery payloads and saved records.

A native client built with Go 1.27.1 from current main
`925a55b0678116e81489108bb9233a0c56552a45` also completed against the `aeab964`
server at `http://[2001:db8:183::1]:18310`. This non-loopback IPv6 address was assigned
to the local interface. The probe reports that IPv6 socket source; the client used H1
fetch throughput and explicitly selected clear WebSocket latency. It completed download,
upload and latency, displayed `server-clock` upload and exited successfully. The result
preserves one unresolved idle-latency sample and one unresolved download-latency sample.
It does not establish routed IPv6 reachability or performance. The alias and fixture were
removed after the run.

**Untested:** genuine Safari/macOS/iOS, physical mobile/touch hardware, actual public/LAN/VPN
deployments, recursive/public DNS behavior, routed/global IPv6,
interactive permission-prompt clicks or managed browser policies, production public-CA
WebTransport trust, multi-backend load-balancer affinity, and downloaded release-package
or container upgrade mechanics. Authentication/sign-in controls are covered separately by issue #184.
### Proposed disposition of untested cases

These are explicit coverage gaps, not passes. The recommendation is to keep them as
nonblocking follow-up coverage for v0.8.1 once the reproduced defects pass their focused
retests and the release gates pass. The release owner makes the final disposition.

| Untested coverage | Basis for the recommendation and remaining limit |
| --- | --- |
| Genuine Safari/macOS/iOS | Linux WebKit now exercises the engine with real measurements and exposed a defect requiring correction; it does not establish Apple platform behavior. Keep Safari/iOS smoke testing as a named follow-up. |
| Physical mobile/touch hardware | Emulated touch, narrow layout, keyboard focus and reduced motion pass. Hardware input, mobile networking, energy use and rendering performance remain unmeasured. |
| Actual public/LAN/VPN and routed/global IPv6 | Local interfaces exercise browser address-space rules, multi-server measurement, egress denial and real DNS AAAA resolution. They do not establish routing, VPN interception, MTU or network performance. |
| Recursive/public DNS; native routed IPv6 | Native loopback and non-loopback local-alias IPv6 measurements and browser AAAA measurement pass separately. Recursive DNS, propagation and native routed IPv6 remain follow-ups. |
| Interactive permission prompts; managed policies | Browser permission states were denied, granted, revoked and regranted through CDP with real blocked/allowed requests. Native prompt interaction and enterprise overrides are separate coverage. |
| Production public-CA WebTransport trust | Forced H3 carried bytes only with a narrow temporary-certificate accommodation. UDP failure and WebTransport-unavailable fallback pass; public-CA H3 trust remains unverified. |
| Multiple backend affinity | The documented single-backend NPM deployment carried all upload session/progress/checkpoint routes. No multi-backend deployment is claimed validated. |
| Downloaded package/container upgrade mechanics | Actual replacement of the old-release-source binary with the current binary passes with stable identity, refreshed generation/version and preserved history. Package installation and container migration are separate coverage. |

Authentication/sign-in behavior and its release disposition belong to the separate
issue #184 validation report.
