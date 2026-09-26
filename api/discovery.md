# Discovery and control response boundary

Control-plane entry point for client implementers. See also the [upload contract](upload.md), the
[latency and WebTransport wire protocol](wire.md) and [measurement definitions](../docs/MEASUREMENTS.md); operators
start with [advertised measurement paths](../docs/DEPLOYMENT.md#advertised-measurement-paths).

## Validation limits

Both clients cap discovery, probe, upload-session, native approval and browser WebTransport-token JSON responses at
64 KiB of decoded bytes, including streamed and decompressed bodies. Request cancellation and timeouts own the read.

| Field | Limit |
| --- | --- |
| Throughput / latency endpoints | ≤ 32 each; `transport` required on every target, from the enumerations in [preflight.schema.json](preflight.schema.json). |
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
apply. `uploadCheckpoint: true` in preflight advertises [receiver checkpoints](upload.md), which coordinated uploads
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
| Browser grants per login | 8; a full login answers HTTP 429 without evicting a grant. Explicit renewal revokes the old grants and their connections. |
| Pending approvals | 8 per login, 8 per client, 256 in total; each expires after 2 minutes. |
| Socket tickets | Single use, ≤ 30 s, at most 8 outstanding per login. |

Cross-origin measurement fetches omit cookies and reject redirects. An unreadable or expired grant affects only its
server. The auth-required marker and token polling permit the bounded CORS flow without exposing protected
discovery. Native approval keeps its own origin boundary. Sockets use destination-bound tickets; see
[socket credentials](wire.md#socket-credentials).
