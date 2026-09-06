# Selecting and testing servers

Graphite Meter 0.8 runs one coordinated test against one to four selected servers.
The normal default remains the server serving the interface. Combined throughput
measures the achievable rate across those selected paths while they share the
client's connection. It does not measure independent server capacities or prove
the physical link's maximum rate.

## Operator catalogue

Every instance uses the same image and deployment model. Peers need no shared
keys, database, service discovery, peer connections, or special measurement-only
image. Their embedded interfaces may remain unused. The server never contacts
catalogue peers during startup, and clients never recursively import peer catalogues.

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

Public HTTP and HTTPS entries may coexist. An HTTPS browser page cannot use a
mixed-content HTTP measurement path; the chooser marks that path unavailable.
Protected remote authorization requires HTTPS for both the requesting interface
and the protected server. The native client can use public HTTP independently of
the browser's mixed-content rules.

For Compose, copy and edit [the catalogue example](../container/server-catalog.example.json),
then use [the catalogue overlay](../container/docker-compose.catalog.yml) alongside
your normal [TLS or proxy deployment](DEPLOYMENT.md). Publishing a catalogue does
not expose an otherwise private peer to the internet; clients must be able to
reach every selected discovery and transport origin.

## Browser and terminal controls

**Settings → Connection paths → Servers → Change** opens the server chooser.
It appears only when more than one server is configured. A quiet indicator in the
gauge identifies a multi-server selection or run; single-server tests retain the
ordinary instrument and result view. Checkboxes
edit a draft; Apply commits it and Escape cancels it. `self` is labelled “This
server” and may be deselected. Opening the chooser checks stale entries with at
most four concurrent discoveries within a shared twelve-second budget. Background
readiness refreshes concern selected servers only. Retry and Sign in apply to
individual unresolved entries. Ready selections start directly; selection changes
are locked during preparation and measurement.

There is one throughput preference and one latency preference. Automatic resolves
each independently on each selected server, preferring fetch throughput and
WebTransport latency where available. Reliable transport mechanisms may differ;
experimental datagram throughput is always explicit. A forced choice must work on
every selected server. The client neither silently downgrades it nor removes an
incompatible server. Use Automatic is an explicit recovery action. Detailed origin
selection remains available for a single selected server.

The latency selector changes the gauge, profile, chart and descriptors together.
Its initial focus is the lowest successful preflight RTT estimate, with catalogue
order breaking ties and supplying a fallback. Focus remains stable during the run
until the user changes it; it never changes which servers are measured.

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
blocked, use the visible Open sign-in page link. Approval uses the server's ordinary
first-party session and CSRF protection. The requesting page polls a verifier-bound
exchange, so opener access and cross-origin message delivery are unnecessary.

The resulting measurement-only bearer grant stays in memory, belongs to its issuer
and exact requesting origin, and expires with the parent login session. Cross-origin
measurement fetches omit cookies and reject redirects. Third-party cookie access is
not required. Reloading the requesting interface requires approving protected peers
again. Signing out on a peer revokes its grants and cancels their active measurements;
other participants can continue.

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
measurements remain labelled in Details. If all servers fail, the result is incomplete.

Result and history details use the same server contributions, relative failure times,
reasons and interval records. Contributions were measured while sharing the connection.
At most 128 recent intervals are retained; an explicit omitted count reports older
intervals while unique byte totals retain the whole measured run. History schema 4
stores this evidence and reads schema 3 as original singleton history.

See [measurement definitions](MEASUREMENTS.md#coordinated-server-windows) for units,
clock boundaries, aggregate stability, unique byte totals and missing-data rules.
