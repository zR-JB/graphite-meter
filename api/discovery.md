# Discovery and control response boundary

This is the control-plane entry point for client implementers. Pair it with the
[upload contract](upload.md), [latency and WebTransport wire protocol](wire.md), and
[measurement definitions](../docs/MEASUREMENTS.md). Operators should start with
[advertised measurement paths](../docs/DEPLOYMENT.md#advertised-measurement-paths).

## Validation limits

The browser and native clients limit discovery, probe, and upload-session JSON
responses to 64 KiB of decoded response bytes. This applies to streamed bodies
without a Content-Length header and to decompressed bodies. Native browser-approval
responses and browser WebTransport verification tokens use the same bound. Existing
request cancellation and timeouts continue to own the read lifetime.

## Originating server catalogue

`GET /servers` publishes [servers.schema.json](servers.schema.json), with at most
32 entries including synthesized `self`, and one to four default selection IDs.
The same 64 KiB response bound applies. With no operator configuration it publishes
the singleton catalogue. Clients read only this originating catalogue, then fetch
each selected server's `/preflight`; they never import a peer's catalogue.

Catalogue IDs and canonical HTTP(S) discovery origins are unique. Saved choices
bind both fields; a removed or changed binding needs user reconciliation. A `.`
origin means the origin serving the catalogue. Transport discovery may use other
ports on that configured hostname with the same scheme. Other transport hosts
require an exact entry in `additionalOrigins`. Discovery cannot extend this list.
Browser mixed-content and secure-context restrictions still apply.

`uploadCheckpoint: true` in preflight confirms support for fresh receiver snapshots.
Coordinated uploads require it. See [upload checkpoints](upload.md) and the
[operator catalogue guide](../docs/SERVERS.md).

## Browser measurement authorization

Protected servers expose the bounded `/auth/browser`, `/auth/browser/approve`,
and `/auth/browser/token` approval flow. A requesting UI must have an exact HTTPS
origin. Login and CSRF-protected approval run on the issuing server; verifier-bound
polling exchanges the single-use approval. The resulting measurement bearer stays
in memory, is bound to that requesting origin and parent login lifetime, and is
revoked by logout. It does not authorize importing `/servers`.

Cross-origin measurement fetches omit cookies and reject redirects. An unreadable
or expired authorization affects its server's readiness or active participation.
The auth-required marker and token polling response permit the bounded CORS flow
without exposing protected discovery. Native approval retains its separate origin
boundary. Socket constructors use destination-bound one-use tickets, described in
the [socket protocol](wire.md#socket-credentials).

Discovery accepts at most 32 throughput endpoints and 32 latency endpoints. Server
name, location, engine version, and generation have a 256-byte UTF-8 limit;
generation must be nonempty. An endpoint baseUrl is either `.` (the origin that
served discovery) or an HTTP(S) origin of at most 2048 bytes. Credentials, paths,
queries, and fragments are rejected. Dedicated ports and independently selected
throughput and latency hosts remain valid. Discovery does not grant credential
access to a new host or alter the existing authentication/redirect policy.

Supported transports and protocols are the enumerations in
[preflight.schema.json](preflight.schema.json). Version 0.7 requires an explicit
transport on every target; missing, empty, and unsupported values are rejected.
Engine version is metadata, not a protocol compatibility test. Unknown additive
fields are ignored.

Probe evidence must use the published IP-version, source, and negotiated-protocol
values. IP text is nonempty and bounded to 64 bytes; optional handler occupancy
requires a nonnegative integer active count and a positive integer maximum.
Token and upload ID control fields are strings of at most 8192 bytes. Upload IDs
and authentication tokens must be nonempty. WebTransport mint responses always
include `token` and `expires`, a nonnegative safe integer in epoch milliseconds.
Authentication-off returns `{ "token": "", "expires": 0 }`; expiry-free responses
are rejected.
Invalid discovery fails before its target catalog is published; invalid probe
evidence fails before it is returned to the caller; rejected bodies cannot turn missing evidence into a successful probe.
