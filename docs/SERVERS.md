# Selecting and testing servers

A test runs against one to four selected servers on one stage schedule; the default is the server serving the
interface. Combined throughput is what those paths achieve while sharing the client's connection, not independent
server capacity or the physical link's maximum.

## Operator catalogue

Every server runs the same image and configures itself. Servers never contact each other: clients connect directly
to each selected server and never import a peer's catalogue. No shared keys, database or discovery service.

Set one of `GM_SERVER_CATALOG` (inline JSON) or `GM_SERVER_CATALOG_FILE` (a file, normally mounted read-only) and
restart. With neither, the catalogue holds only `self`. Usually a list of the other servers' origins is enough:

```sh
export GM_SERVER_CATALOG='["https://fra.example.net","https://ams.example.net"]'
```

Each server's name, location and paths come from its own `/preflight`; until discovery succeeds the hostname is
shown. IDs derive from the canonical origin, so reordering keeps them and changing the origin creates a new one. Do
not list `/preflight` or `/probe` paths.

The object form adds named IDs, a default selection and extra measurement hosts:

```json
{
  "defaultSelection": ["self"],
  "servers": [
    {"id": "frankfurt", "url": "https://fra.example.net", "name": "Frankfurt"},
    {"id": "amsterdam", "url": "https://ams.example.net", "name": "Amsterdam",
     "additionalOrigins": ["https://transfer.ams.example.net:8443"]}
  ]
}
```

| Field | Rule |
| --- | --- |
| `servers` | Up to 31 entries besides the synthesized `self` (omit it). Unique IDs and origins. |
| `id` | 1–64 ASCII letters, digits, `.`, `_` or `-`. |
| `url` | HTTP(S) origin, ≤ 2048 bytes, no credentials, path, query or fragment. |
| `name`, `location` | Fallbacks, ≤ 256 UTF-8 bytes, no control characters; discovery supplies current values. |
| `additionalOrigins` | Up to 32 exact origins on other hosts; ports on the entry's own hostname need none. |
| `defaultSelection` | One to four existing IDs; omitted means `self`, empty is invalid. |

The raw configuration and the published response are each limited to 64 KiB. The interface derives its browser
connection policy from these destinations, so discovery cannot authorize an unrelated host; proxies must keep that
policy and allow streaming and `Authorization` headers. Protected servers accept measurement grants only on their
own hostname. For Compose, edit [the example](../container/server-catalog.example.json) and add
[the catalogue overlay](../container/docker-compose.catalog.yml).

Browsers save a choice as ID plus canonical origin. A removed or changed entry needs the user to apply a new
selection; an ID never silently redirects to another server. Listing a server does not make it reachable: a catalogue
may mix public, intranet and LAN servers, unselected unavailable entries do not block a test, and an unavailable
selected entry fails by name until access returns or it is deselected.

HTTP and HTTPS servers may be mixed; Automatic resolves each server's advertised paths separately. From an HTTPS
page the browser blocks non-loopback HTTP paths as mixed content, and protected servers need HTTPS on both the page
and the server. The native client has no mixed-content rule.

### Local-network browser permission

A hosted page reaching a private-address server, or an intranet page reaching loopback, may trigger the browser's
local-network permission. It covers the client's own requests, not server-to-server traffic; allow it only for
servers you trust, then Retry or reopen Settings. Chrome requires a secure page and, from Chrome 147, applies it to
WebSocket and WebTransport too ([release notes](https://support.google.com/chrome/a/answer/10314655?hl=en)). It does
not replace certificates, server authorization, CORS or the page's security policy.

Chromium's connection policy cannot match literal IPv6 hosts, so use a DNS name for a separate IPv6 origin; the page's
own origin and the native client accept literals.

## Browser controls

**Settings → Connection paths → Test servers** is a checklist shown when the catalogue has more than one server.
Select one to four; the last one cannot be cleared. Each entry shows its status (Checking…, Ready, Unavailable,
Sign in) and its **preflight request time**: the HTTP discovery request including connection setup, not a latency
measurement. Hover or focus shows name, location and host. Inline **Retry** and **Sign in** resolve individual
entries; **Use available servers** repairs a stale saved selection.

With several servers, **Latency server** picks the one probed for latency (default: the first selected) or
**Every server**. The choice is saved and fixed during the run; unprobed servers have no latency result. Results and
History share one **Combined** / per-server selector; with Every server the initial latency focus is the server with
the lowest preparation RTT. Switching focus never changes what was measured.

Opening Settings discovers unselected entries (bounded concurrency, 5 s each) and closing it cancels that; failures
back off, sign-in failures wait for Sign in or Retry. Start rechecks selected servers whose evidence is missing or
expired. Server, path, stream and probe settings lock during a run; durations of unstarted stages, early finish and
display settings stay live.

There is one throughput and one latency path preference. Automatic checks each server independently: throughput
tries HTTP/1.1 bulk streams, HTTP/2, HTTP/3, proxy-negotiated HTTP, then WebTransport streams; latency tries
WebTransport, then WebSocket, moving on when a check fails. Datagram throughput is always explicit. A forced path
must work on every server it applies to; the client never downgrades it or drops the server, and **Use Automatic**
recovers. Changing servers carries the chosen protocol or transport, not the old address. Exact origins can be
chosen only with a single server.

The terminal client takes `--url` as the catalogue and repeatable `--server ID`; **s** opens the same checklist and
**u** keeps the available servers ([keys](DEPLOYMENT.md#native-terminal-client)). It probes latency on every server.

## Independent sign-in

**Sign in** opens a popup on that server: its normal password or OIDC login, then an approval page naming the
requesting page's HTTPS origin. Approve only if its eight-character code matches the one in Settings. The requesting
page polls a verifier-bound exchange, so no opener or cross-origin messaging is needed; **Open sign-in page** covers a
blocked popup and **Cancel sign-in** ends the exchange. An existing login is reused.

The resulting grant is measurement-only, lives in page memory, and is bound to its issuer, the requesting origin and
the login's lifetime. Cross-origin measurement requests omit cookies and refuse redirects; third-party cookies are
not needed. Reloading the page requires approving again. Signing out on the server revokes its grants and cancels
their measurements; other servers continue. Grant, approval and socket-ticket limits are in
[discovery](../api/discovery.md#browser-measurement-authorization).

## Results and failure

Before measurement starts, every selected server must be ready. Afterwards a server that fails is dropped for the
rest of the run while the others continue; see [coordinated servers](MEASUREMENTS.md#coordinated-servers) for
intervals, dropouts, headlines and missing data. Each server's own result uses its window under the shared load;
per-server headlines do not add up to the Combined value. Saved records keep failure times and reasons, up to 128
recent intervals and whole-run byte totals ([history](MEASUREMENTS.md#saved-history)).
