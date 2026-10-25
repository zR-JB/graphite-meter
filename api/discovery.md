# Discovery and control response boundary

This document is the control-plane entry point for client implementers. See also the [upload contract](upload.md),
the [latency and WebTransport wire protocol](wire.md) and [measurement definitions](../docs/MEASUREMENTS.md);
operators start with [advertised measurement paths](../docs/DEPLOYMENT.md#advertised-measurement-paths).

## Validation limits

Both clients cap discovery, probe, lease and native approval JSON responses at 64 KiB of decoded bytes, including streamed and decompressed bodies. The cap bounds size only; request cancellation
and timeouts bound how long the read may take.

| Field | Limit |
| --- | --- |
| Throughput / latency endpoints | ≤ 32 each, counted as sent (an input bound, before any target is skipped); `transport` (and a throughput target's `protocol`) required and nonempty on every target, never inferred from its absence. A value outside the enumerations in [preflight.schema.json](preflight.schema.json) skips that target, so newer servers stay usable ([preflight.forward.golden.json](preflight.forward.golden.json)). |
| Endpoint `baseUrl` | `.` (the origin that served discovery) or an HTTP(S) origin ≤ 2048 bytes, no credentials, path, query or fragment. |
| Server name, location, engine version, generation | ≤ 256 UTF-8 bytes; generation nonempty. |
| Probe evidence | Published IP-version, source and negotiated-protocol values; IP text 1 to 64 bytes; optional `load` with integers `active` ≥ 0 and `max` ≥ 1. |
| Token and upload ID | Nonempty strings ≤ 8192 bytes. |
| Lease limits | `lease.maxMeasurements` ≥ 1 and `lease.maxSessions` ≥ 0, both required. |
| Lease answer | `token`, the `measurements` and `sessions` it holds, and at most 4 `uploads`, each an upload ID. |

Beyond the [envelope](#the-envelope), the engine version is metadata, not a compatibility test; unknown additive fields
are ignored. Invalid discovery fails before its targets are published and invalid probe evidence before it reaches the
caller, so a rejected body never turns into a successful probe.

Duplicate object member names are ambiguous: the terminal client rejects them at any depth,
including inside unknown fields, in these responses and in [upload progress records](upload.md#progress-records). The
browser client's `JSON.parse` keeps the last duplicate.

## The envelope

Every preflight and probe carries an envelope that no API version changes: `engineVersion`, the server's release,
and `apiVersion`, an integer the server raises with each change older clients cannot read.
[apiversions.txt](apiversions.txt) pins each API version's first release. A client reads the envelope first, whatever
the rest of the document holds, and validates the rest only for an API version it speaks.

- A preflight without `apiVersion` but with a string `engineVersion` is a server's from before the envelope: without
  `lease` it is API version 1 (0.13.4 and earlier), with `lease` API version 2 (0.13.5). A probe without
  `apiVersion` is read as before.
- A server of an API version the client does not speak fails its check with words that say which side to update. The
  browser shows them in the server's row and on the dial, and reads that server again only on Retry or a reload; the
  terminal client reports them.
  - Older: "S2 runs Graphite Meter 0.13.4 (API version 1); this page needs API version 2, from 0.13.5. Update S2."
  - Newer: "S2 runs Graphite Meter 0.15.0, newer than this page (0.13.6); reload the page or update the server this
    page comes from." The terminal client says "this client" and "update this client".
  - A probe names its origin in place of the server, so a transfer port that another release serves is named too.
- Where `engineVersion` is missing, empty or unsafe, the words say "an unknown Graphite Meter release". A preflight
  of an API version the client speaks still needs a safe one, which may be empty; a probe may omit it, but one it
  sends must be safe.

## Refusals

A server that refuses a measurement request for a reason below says why: the answer carries
`Graphite-Meter-Refusal: <code>`, which CORS exposes, and a one-line text body. A request that needs a sign-in gets
`403` with `Graphite-Meter-Auth: required`, which a `revoked` refusal carries too. [refusals.txt](refusals.txt) pins
each code's text, status and the failure reason a client records when the refusal ends a server's stage; an upload's
progress feed carries the same code in its `error` record ([upload contract](upload.md)).

| Code | Answer | When |
| --- | --- | --- |
| `client-limit` | `429`, `Retry-After: 1` | The client's address or sign-in already holds its share: `GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT` measurements, `GM_MAX_SESSIONS_PER_CLIENT` WebTransport sessions, or its upload IDs, of which each lease holds its own share. |
| `server-capacity` | `503`, `Retry-After: 1` | The server holds `GM_MAX_ACTIVE_MEASUREMENTS` measurements, `GM_MAX_ACTIVE_SESSIONS` sessions, or all the upload ids it keeps. |
| `wrong-protocol` | `409` with `Alt-Svc` | `/download` or `/upload` reached the HTTP/3 port over TCP, which serves no transfer. The client fails the path at once instead of asking again; `Alt-Svc` brings the browser back to HTTP/3 for later requests. Not `421`, which makes Firefox forget the origin's HTTP/3. |
| `invalid`, `owner-mismatch`, `idle`, `revoked` | `400`, `403`, `408`, `403` | An upload id or lane; see the [upload contract](upload.md). |

Browsers hide the status of a refused WebSocket upgrade or WebTransport CONNECT, so a page sees those refusals only as
a dial that failed.

## The lease

A client takes a lease at a server's discovery origin before it measures there. The lease holds the client's
measurements from then on, so a server without room for them says so before anything is measured, and its work is
never refused for the client's share while the lease holds room for it.

| Request | Answers |
| --- | --- |
| `POST /lease` `{"measurements": m, "sessions": s, "uploads": u}` | `200 {"token", "measurements", "sessions", "uploads"}`: a new lease holding `m` measurements, of them `s` WebTransport transfer sessions, within the preflight's `lease` limits, which cut a larger ask, and a client then runs no test on that server; `u` (at most 4, default 0) upload IDs minted with it. `429 client-limit` or `503 server-capacity` when the client's share or the server cannot hold it, which leaves a held lease as it was; `400` for an invalid body (`m` ≥ 1, `s` ≤ `m`). |
| The same, with `Graphite-Meter-Lease: <token>` | Resizes that lease to `m` and `s` from now, answering the same token, or takes a new lease when that one ended. |
| `DELETE /lease` with `Graphite-Meter-Lease: <token>` | `204`; the lease ends. Work it admitted runs on until it ends. |

- **Presented by its work.** HTTP requests carry the token in `Graphite-Meter-Lease`; WebSocket and WebTransport URLs,
  which carry no field a page can set, carry it as `lease=`. Every measurement route reads either.
- **Admits against itself.** A lane, latency bus, progress feed or session presenting the lease takes one of its
  measurements, a WebTransport transfer session one of its sessions too. Work past what it holds is admitted against
  the client's share as work without a lease is, which a reopening lane may need while its old request still ends.
- **Owns its uploads.** An upload first used with the lease belongs to it, whatever address later presents it with
  the lease; one used without a lease belongs to the sign-in or address, as before.
- **Bound** to the sign-in that took it, which it presents for its work and ends with, and to the `Origin` that took
  it. A request naming another `Origin`, or presenting a lease that ended or never was, as after a restart, is served
  as one without a lease; one naming no `Origin`, as a same-origin `GET`, presents it.
- **Lives while used.** It ends on `DELETE`, with its sign-in, or 30 s after nothing presenting it was live.
- `POST /lease` and `DELETE /lease` are served on the discovery origin and the native HTTP/2 and HTTP/3 endpoints, not
  on the HTTP/3 port's TCP side.

## Originating server catalog

`GET /servers` publishes [servers.schema.json](servers.schema.json): at most 32 entries including the synthesized
`self`, and one to four default IDs, within 64 KiB. Without configuration it is the singleton catalog. Clients
read only this catalog and then each selected server's `/preflight`.

IDs and canonical discovery origins are unique; saved choices bind both, so a changed binding needs the user. A `.`
origin is the origin serving the catalog. Discovery may use other ports on the entry's hostname; other hosts need an
exact `additionalOrigins` entry, which discovery cannot extend. Mixed-content and secure-context rules still apply.
`maxStageMs` in preflight capabilities is the longest stage a client may plan against that server (1000 to
86400000); a run uses the smallest among its servers and refuses a longer plan by naming the server.
`uploadCheckpoint: true` in preflight advertises [receiver checkpoints](upload.md); the browser requires it of every
server, the terminal client for a run with an upload stage.

## Browser measurement authorization

Protected servers expose `/auth/browser`, `/auth/browser/approve` and `/auth/browser/token`. The requesting page
needs an exact HTTPS origin. Login and CSRF-protected approval run on the issuing server; the requester polls a
verifier-bound exchange for the approval, which issues one grant. The resulting bearer grant stays in memory, is
bound to the requesting origin and the parent login's lifetime, works only on the issuer's own hostname, and is
revoked by logout. It does not authorize `/servers`.

Both pages show the same comparison code: the first five SHA-256 bytes of the verifier as unpadded RFC 4648 base32
(eight characters). The verifier never leaves the requester.

| Limit | Value |
| --- | --- |
| Grants per login | 8, native and browser together. At capacity a native exchange replaces the login's oldest native grant; a browser exchange, or a native one when all 8 are browser grants, answers HTTP 429 without evicting one. Explicit renewal revokes the old grants and their connections. |
| Pending approvals | 8 per login, 8 per client, 256 in total of which at most 128 opened before sign-in; each expires after 2 minutes. |
| Repeated exchange | For 30 s after the grant was issued, the same verifier from the same origin gets the same grant again, so a requester whose answer was lost can ask again; it counts once toward the 8. Afterwards the exchange answers `410` until the approval would have expired. Once the grant is issued, the approval page and the approve action refuse. |

The exchanges and the session read answer JSON; a native `expires` is an RFC 3339 time, a browser one epoch
milliseconds:

| Request | Answers |
| --- | --- |
| `POST /auth/cli/token` `{"verifier": "…"}` (≤ 128 bytes) | `202 {"status":"pending"}` until approved, also for a malformed body; `200 {"token", "expires"}`; `410` once a repeated exchange's 30 s have passed; `429` at grant capacity. |
| `POST /auth/browser/token` `{"verifier": "…"}` (32 to 128 bytes) from an exact HTTPS `Origin` | `202 {"status":"pending"}` until approved; `200 {"token", "expires", "remainingMs", "maximumLifetimeMs"}`; `410` once a repeated exchange's 30 s have passed; `429` at grant capacity; `403` for a malformed body or origin. |
| `GET /auth/session` with the session cookie | `200 {"name", "provider", "expires", "csrf", "remainingMs", "maximumLifetimeMs"}`; `403` without a session. |

Auth routes read at most 4 KiB of request body and answer with `Cache-Control: no-store`, `X-Frame-Options: DENY`
and a `default-src 'none'` policy.

Cross-origin measurement fetches omit cookies and reject redirects. An unreadable or expired grant affects only its
server. The auth-required marker and token polling permit the bounded CORS flow without exposing protected
discovery. Native approval keeps its own origin boundary, and sockets present the lease; see
[socket credentials](wire.md#socket-credentials).
