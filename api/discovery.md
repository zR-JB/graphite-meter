# Discovery and control response boundary

The browser and native clients limit discovery, probe, and upload-session JSON
responses to 64 KiB of decoded response bytes. This applies to streamed bodies
without a Content-Length header and to decompressed bodies. Native browser-approval
responses and browser WebTransport verification tokens use the same bound. Existing
request cancellation and timeouts continue to own the read lifetime.

Discovery accepts at most 32 throughput endpoints and 32 latency endpoints. Server
name, location, engine version, and generation have a 256-byte UTF-8 limit;
generation must be nonempty. An endpoint baseUrl is either `.` (the origin that
served discovery) or an HTTP(S) origin of at most 2048 bytes. Credentials, paths,
queries, and fragments are rejected. Dedicated ports and independently selected
throughput and latency hosts remain valid. Discovery does not grant credential
access to a new host or alter the existing authentication/redirect policy.

Supported transports and protocols are the enumerations in
[preflight.schema.json](preflight.schema.json). The legacy omission of transport
retains fetch-stream for throughput and websocket for latency. Engine version is
metadata, not a protocol compatibility test. Unknown additive fields are ignored.

Probe evidence must use the published IP-version, source, and negotiated-protocol
values. IP text is nonempty and bounded to 64 bytes; optional handler occupancy
requires a nonnegative integer active count and a positive integer maximum.
Token and upload ID control fields must be nonempty strings of at most 8192 bytes.
Invalid discovery fails before its target catalog is published; invalid probe
evidence fails before it is returned to the caller; rejected bodies cannot turn missing evidence into a successful probe.
