# Discovery and control response boundary

Control-plane entry point for client implementers. See also the [upload contract](upload.md), the
[latency and WebTransport wire protocol](wire.md) and [measurement definitions](../docs/MEASUREMENTS.md); operators
start with [advertised measurement paths](../docs/DEPLOYMENT.md#advertised-measurement-paths).

## Validation limits

Both clients cap discovery, probe, upload-session, native approval and browser WebTransport-token JSON responses at
64 KiB of decoded bytes, including streamed and decompressed bodies. Request cancellation and timeouts own the read.

| Field | Limit |
| --- | --- |
| Throughput / latency endpoints | ≤ 32 each, counted as sent (an input bound, before any target is skipped); `transport` (and a throughput target's `protocol`) required and nonempty on every target, never inferred from its absence. A value outside the enumerations in [preflight.schema.json](preflight.schema.json) skips that target, so newer servers stay usable ([preflight.forward.golden.json](preflight.forward.golden.json)). |
| Endpoint `baseUrl` | `.` (the origin that served discovery) or an HTTP(S) origin ≤ 2048 bytes, no credentials, path, query or fragment. |
| Server name, location, engine version, generation | ≤ 256 UTF-8 bytes; generation nonempty. |
| Probe evidence | Published IP-version, source and negotiated-protocol values; IP text 1–64 bytes; optional occupancy = integer active ≥ 0 and maximum > 0. |
| Token and upload ID | Nonempty strings ≤ 8192 bytes. |
| Socket-ticket mint response | `token` plus `expires` (safe integer, epoch ms); authentication off returns `{ "token": "", "expires": 0 }`. |

Engine version is metadata, not a compatibility test; unknown additive fields are ignored. Invalid discovery fails
before its targets are published and invalid probe evidence before it reaches the caller, so a rejected body never
turns into a successful probe.

## Originating server catalogue

`GET /servers` publishes [servers.schema.json](servers.schema.json): at most 32 entries including the synthesized
`self`, and one to four default IDs, within 64 KiB. Without configuration it is the singleton catalogue. Clients
read only this catalogue and then each selected server's `/preflight`.

IDs and canonical discovery origins are unique; saved choices bind both, so a changed binding needs the user. A `.`
origin is the origin serving the catalogue. Discovery may use other ports on the entry's hostname; other hosts need
an exact `additionalOrigins` entry, which discovery cannot extend. Mixed-content and secure-context rules still
apply. `maxStageMs` in preflight capabilities is the longest stage a client may plan against that server (1000 to
86400000; absent means 300000, the limit before servers advertised one); a run uses the smallest among its servers
and refuses a longer plan by naming the server. `uploadCheckpoint: true` in preflight advertises
[receiver checkpoints](upload.md), which coordinated uploads
require.

## Browser measurement authorization

Protected servers expose `/auth/browser`, `/auth/browser/approve` and `/auth/browser/token`. The requesting page
needs an exact HTTPS origin. Login and CSRF-protected approval run on the issuing server; the requester polls a
verifier-bound exchange for the single-use approval. The resulting bearer grant stays in memory, is bound to the
requesting origin and the parent login's lifetime, works only on the issuer's own hostname, and is revoked by
logout. It does not authorize `/servers`.

Both pages show the same comparison code: the first five SHA-256 bytes of the verifier as unpadded RFC 4648 base32
(eight characters). The verifier never leaves the requester.

| Limit | Value |
| --- | --- |
| Grants per login | 8, native and browser together. At capacity a native exchange replaces the login's oldest native grant; a browser exchange, or a native one when all 8 are browser grants, answers HTTP 429 without evicting one. Explicit renewal revokes the old grants and their connections. |
| Pending approvals | 8 per login, 8 per client, 256 in total of which at most 128 opened before sign-in; each expires after 2 minutes. |
| Socket tickets | Single use, ≤ 30 s, at most 8 outstanding per login. |

The exchanges and the session read answer JSON; a native `expires` is an RFC 3339 time, a browser one epoch
milliseconds:

| Request | Answers |
| --- | --- |
| `POST /auth/cli/token` `{"verifier": "…"}` (≤ 128 bytes) | `202 {"status":"pending"}` until approved, also for a malformed body; `200 {"token", "expires"}`; `429` at grant capacity. |
| `POST /auth/browser/token` `{"verifier": "…"}` (32–128 bytes) from an exact HTTPS `Origin` | `202 {"status":"pending"}` until approved; `200 {"token", "expires", "remainingMs", "maximumLifetimeMs"}`; `429` at grant capacity; `403` for a malformed body or origin. |
| `GET /auth/session` with the session cookie | `200 {"name", "provider", "expires", "csrf", "remainingMs", "maximumLifetimeMs"}`; `403` without a session. |

Auth routes read at most 4 KiB of request body and answer with `Cache-Control: no-store`, `X-Frame-Options: DENY`
and a `default-src 'none'` policy.

Cross-origin measurement fetches omit cookies and reject redirects. An unreadable or expired grant affects only its
server. The auth-required marker and token polling permit the bounded CORS flow without exposing protected
discovery. Native approval keeps its own origin boundary. Sockets use destination-bound tickets; see
[socket credentials](wire.md#socket-credentials).
