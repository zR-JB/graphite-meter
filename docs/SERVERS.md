# Selecting and testing servers

Graphite Meter 0.8 runs one coordinated test against one to four selected servers.
The normal default remains the server serving the interface. Combined throughput
measures the achievable rate across those selected paths while they share the
client's connection. It does not measure independent server capacities or prove
the physical link's maximum rate.

## Operator catalogue

Every instance uses the same image and deployment model. Peers need no shared
keys, database, service discovery, peer connections, or special measurement-only
image. Their embedded interfaces may remain unused. Servers do not contact each
other for discovery or measurement: each browser or terminal client connects
directly to the selected servers. Clients never recursively import peer catalogues.

Set exactly one of `GM_SERVER_CATALOG` (inline JSON) or `GM_SERVER_CATALOG_FILE`
(a readable JSON file, normally mounted read-only). Restart after changing it.
With neither setting, the published catalogue contains only `self`.

For a typical deployment, list only the origins of additional servers:

```yaml
services:
  graphite-meter:
    environment:
      GM_SERVER_CATALOG: |-
        [
          "https://fra.example.net",
          "https://ams.example.net"
        ]
```

Each server configures its own name, location and listeners. Clients obtain those
values and available transports from its `/preflight`; `/probe` verifies a path
and reports connection evidence. Do not include either path in the list. No peer
requests occur during server startup. Until discovery succeeds, the hostname is
shown. IDs are derived from the canonical origin and remain stable when the list
is reordered. Changing the origin creates a different identity.

The same JSON array works in a catalogue file, or as one shell variable:

```sh
export GM_SERVER_CATALOG='["https://fra.example.net","https://ams.example.net"]'
```

The advanced object form is available when you need named IDs, a different default
selection, or measurement hosts different from the discovery hostname:

```json
{
  "defaultSelection": ["self"],
  "servers": [
    {"id": "frankfurt", "url": "https://fra.example.net", "name": "Frankfurt"},
    {"id": "amsterdam", "url": "https://ams.example.net", "name": "Amsterdam", "additionalOrigins": ["https://transfer.ams.example.net:8443"]}
  ]
}
```

Catalogue names and locations are fallbacks; successful discovery supplies the
server's current display metadata without changing its ID or origin.

The configuration omits `self`; Graphite Meter prepends it with its own name,
location, discovery origin and configured transport origins. There may be up to
31 other entries. IDs use 1–64 ASCII letters, digits, dots, underscores or hyphens.
Names and locations are at most 256 UTF-8 bytes, without control characters.
URLs are HTTP(S) origins, without credentials, paths, queries or fragments; origin
strings are at most 2048 bytes. IDs and discovery origins must be unique. The raw
configuration is limited to 64 KiB; its normalized form reserves 16 KiB for the
synthesized local entry. Published responses are limited to 64 KiB.

`defaultSelection` contains one to four existing IDs. Omitting it selects `self`;
an empty list is invalid. Catalogue order stays stable in both clients. Browser
choices override operator defaults and are saved as ID plus canonical origin.
Removing or changing an entry requires the user to review and apply a selection;
an ID cannot silently redirect a saved choice to a replacement server.

Each entry's `/preflight` supplies its own capabilities. Ports on the configured
hostname are allowed. Separately named transport hosts require exact origins in
`additionalOrigins` (up to 32 per entry). The host derives its browser connection
policy from these configured destinations. Discovery cannot authorize an unrelated
host or expand a grant's permissions. Reverse proxies must preserve that policy
and allow streaming traffic and authorization headers.

Public HTTP and HTTPS entries may coexist. For example, an interface at
`http://meter.lan:7246` can measure its local clear server and
`https://fra.example.net` together. Automatic resolves the advertised paths of
each server separately; it does not force the interface's protocol onto peers.
An ordinary HTTP interface uses WebSocket latency when WebTransport is unavailable.
Graphite blocks non-loopback HTTP measurement paths from an HTTPS interface;
connection settings mark those mixed-content paths unavailable.
Protected remote authorization requires HTTPS for both the requesting interface
and the protected server. The native client can use public HTTP independently of
the browser's mixed-content rules.

For Compose, copy and edit [the catalogue example](../container/server-catalog.example.json),
then use [the catalogue overlay](../container/docker-compose.catalog.yml) alongside
your normal [TLS or proxy deployment](DEPLOYMENT.md). Publishing a catalogue does
not expose an otherwise private peer to the internet; clients must be able to
reach every selected discovery and transport origin.

### Local-network browser permission

The browser may ask to access devices on the local network when a hosted interface
contacts a private-address server, or when an intranet interface contacts loopback.
This is permission for direct client requests, not communication between servers.
Allow it only for an interface and measurement servers you trust. After changing
the site's permission, use Retry or reopen Settings.

Chrome requires a secure requesting context for this permission. Its local-network
and loopback permissions also cover WebSocket and WebTransport connections as of
Chrome 147. See the [Chrome release notes](https://support.google.com/chrome/a/answer/10314655?hl=en)
for current behavior; other browsers and managed-device policies may differ.
Permission does not replace a trusted certificate, the server's authorization,
CORS, or Graphite's content-security and mixed-content checks. Use an HTTPS interface
and reachable HTTPS measurement origins for public-to-private deployments.

For browser connections to a separate IPv6 origin, use a DNS hostname that resolves
to that address. Chromium cannot match literal IPv6 hosts in the site's narrowly
scoped connection policy; Graphite reports that limitation before attempting the
connection. Same-origin IPv6 paths remain usable, as do literal IPv6 destinations
in the native client. A different port is a different origin.

## Browser and terminal controls

**Settings → Connection paths → Servers** offers a compact colored band. Toggle
one to four servers directly; the final selected server cannot be removed.
Overlapping pills show the server name and location, so providers in the same city
remain separate choices. The band wraps into additional rows, with full names,
locations and hosts available on hover, keyboard focus or touch. The band appears
when more than one server is configured. A quiet gauge indicator shows
the selected or measured server count. Single-server runs retain the ordinary
instrument and result view.

Changes apply immediately. Opening Settings discovers unselected entries serially,
with five seconds per request and a shared ten-second budget. Closing Settings
cancels that work. Fresh discoveries are reused; failures and timeouts back off so
reopening can reach later entries. Background readiness refreshes concern selected
servers only. Inline Retry and Sign in actions resolve individual unavailable
entries. Ready selections start directly.

Each successfully discovered entry shows its latest **HTTP preflight request time**
in milliseconds, including connection setup and the complete response body. This
is separate from measured ping latency and appears regardless of the latency
selection. It opens no extra ping workers. Unreachable or unauthorized entries
have no time; a missing value is never rendered as zero. These catalogue timings
do not enter run statistics or decide latency focus.

The browser defaults to measuring **latency against one selected server**, while
all selected servers perform the speed test. Choose that primary server in Settings
before starting. Initially it is the first selected entry in catalogue order; a
saved primary choice is reused while it remains selected. **All** in the Latency band enables
separate latency measurements for all selected servers. The choice covers both idle
and loaded latency, is saved on this device, and is fixed during preparation and
measurement. An unprobed server has no latency result, rather than a zero RTT.
If the primary fails, the client does not silently change latency endpoints.

There is one throughput preference and one latency transport preference. Automatic
resolves each independently, preferring fetch throughput and WebTransport latency
where available. Reliable transport mechanisms may differ; experimental datagram
throughput is always explicit. A forced throughput choice must work on every selected
server; a forced latency choice must work on every server being probed. The client
neither silently downgrades it nor removes an incompatible server. Use Automatic is
an explicit recovery action. Detailed origin selection remains available for a
single selected server.

The latency pills change the gauge, profile, chart and descriptors together without
changing measurement targets. With All, initial display focus uses the
lowest successful preflight RTT estimate, with catalogue order breaking ties.
Each pill exposes the server name, location and host on hover or keyboard focus.
Result cards and saved history share compact All / named server bands to switch
between aggregate throughput and individual measurements. Quiet latency source captions expose
the full identity and location in tooltips. Latency belongs to one server; it is not
pooled. Selecting a result with latency evidence also focuses its latency profile.

Server, connection, stream and probe settings are locked for the active run.
Supported live changes remain available for active/future durations, unstarted
stages and early finish. Display units, visual scales, theme and result inspection
remain interactive. Preparation locks run configuration until launch or cancellation.

The TUI uses `--url` as the originating catalogue URL and repeatable `--server ID`
arguments as the selected set. Without `--server`, operator defaults apply. Press
**s** in setup, navigate with arrows, toggle with Space, apply with Enter or cancel
with Escape. **a** applies Automatic paths. Selection is hidden when the catalogue contains
only one server. For a multi-server run, **l** rotates latency focus and **d**
opens scrollable server details. Explicit origin overrides require a singleton
selection. Protected peers each use their own explicit browser approval.

## Independent sign-in

Choosing Sign in opens that server's existing password/OIDC login followed by an
approval page naming the requesting interface's exact HTTPS origin. If a popup is
blocked, use the visible Open sign-in page link. Compare the eight-character code
shown in Settings with the approval page and approve only when both match.
Approval uses the server's ordinary
first-party session and CSRF protection. The requesting page polls a verifier-bound
exchange, so opener access and cross-origin message delivery are unnecessary.
Cancel sign-in stops the pending exchange and closes its owned popup. Retrying
creates a fresh approval; deselecting that server also cancels a pending approval.

The resulting measurement-only bearer grant stays in memory, belongs to its issuer
and exact requesting origin, and expires with the parent login session. Cross-origin
measurement fetches omit cookies and reject redirects. Third-party cookie access is
not required. Reloading the requesting interface requires approving protected peers
again. Signing out on a peer revokes its grants and cancels their active measurements;
other participants can continue.

A login session can authorize up to eight browser clients. At that limit, approval
offers an explicit login renewal instead of silently replacing an existing grant.
Renewal revokes that login's previous grants and ends their active connections.
After renewing at the remote server, choose Sign in again in the requesting interface.

WebSocket and WebTransport connections use short-lived, single-use tickets bound to
the destination, route, requesting origin and grant. Reusable bearer grants never
appear in socket URLs. Separate grants cannot access each other's uploads, and their
resources remain subject to the existing parent identity/session limits.

## Results and failure

All participants share preparation, warmup and one measured stage schedule. Before
any measurement, unresolved participants prevent the start. After measurement has
begun, a terminal throughput failure removes that server for the remaining run after
bounded transport recovery. Healthy connections continue. A latency-only interruption
does not remove healthy throughput.

Each throughput interval has fixed membership. A dropout starts a fresh survivor
interval and resets stability confirmation. The headline uses the latest interval
only, with at least 800 ms of client evidence and, for upload, 800 ms in each receiver
window. An insufficient final interval produces an unavailable headline; earlier
measurements remain available in the affected server's results. If all servers fail, the result is incomplete.

Live results and history share compact server controls and failure notices. Each
server's result uses its own canonical measurement window under the shared load;
these individual headline values must not be summed to reconstruct the aggregate.
The aggregate is calculated from common receiver windows by the coordinator.

Saved evidence retains relative failure times, reasons and at most 128 recent
intervals even though the ordinary UI no longer presents a timing drawer. An omitted
count records older intervals while unique byte totals retain the whole measured
run. History schema 4 stores this evidence and reads schema 3 as original singleton
history.

See [measurement definitions](MEASUREMENTS.md#coordinated-server-windows) for units,
clock boundaries, aggregate stability, unique byte totals and missing-data rules.
