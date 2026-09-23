# Transport dependency provenance

These MIT-licensed source copies derive from published Hyperium crates. Each directory retains its upstream license and readme. Cargo manifests retain dependency features and upstream test dependencies; package metadata is reduced. Registry marker files, lockfiles and build artifacts are omitted. Vendor packages are excluded from the application workspace and keep their own licenses.

| Directory | Published source | SHA-256 of `.crate` archive |
| --- | --- | --- |
| `h3` | [h3 0.0.8](https://crates.io/crates/h3/0.0.8) | `10872b55cfb02a821b69dc7cf8dc6a71d6af25eb9a79662bec4a9d016056b3be` |
| `h3-noq` | [h3-quinn 0.0.10](https://crates.io/crates/h3-quinn/0.0.10) | `8b2e732c8d91a74731663ac8479ab505042fbf547b9a207213ab7fbcbfc4f8b4` |

Upstream sources: [Hyperium h3](https://github.com/hyperium/h3). The adapter and server pin [Noq ca4b44552c6df355906f45c58e992ddbb7986bbb](https://github.com/n0-computer/noq/commit/ca4b44552c6df355906f45c58e992ddbb7986bbb), which includes reliable stream reset support. Noq and noq-udp are fetched by Cargo. The noq-proto package at the same revision is patched to the local source copy described below. All retain upstream MIT/Apache-2.0 licensing. Runtime crypto uses rustls with ring.

## Local changes

- `h3/src/{connection,stream}.rs`: expose IDs of unidirectional streams whose type/association header is incomplete, so the application can enforce a per-stream header deadline without adding runtime-specific timers to the HTTP/3 library.

- `h3/src/proto/frame.rs`: recognize current `SETTINGS_WT_ENABLED` (`0x2c7cf000`) and `SETTINGS_WT_MAX_SESSIONS` (`0x14e9cd29`), alongside legacy identifiers; increase settings storage from eight to ten entries.
- `h3/src/config.rs`: emit both generations of settings; prefer current identifiers when reading, falling back to legacy identifiers.
- `h3/src/ext.rs`: accept the current `webtransport-h3` extended CONNECT protocol spelling as well as `webtransport`.
- `h3/src/webtransport/session_id.rs`: preserve the full CONNECT stream ID rather than its quarter-stream index; add a nonzero-ID roundtrip test.
- `h3-noq/Cargo.toml`: rename the package/library and alias its `quinn` dependency to pinned Noq, retaining `futures-io` and disabled defaults.
- `h3-noq/src/lib.rs`: adapt ordered receive chunks to Noq's single-argument `read_chunk` returning `Bytes`; remove the obsolete `IllegalOrderedRead` error match. Retain the raw receive stream directly and poll a fresh cancellation-safe read future, so stopping a cancelled pending read and querying its ID cannot unwrap an absent stream. Cap zero-copy chunks at 16 KiB to bound header-parser prefetch.

Current settings and protocol spelling were corroborated against [webtransport-go v0.13.0](https://github.com/quic-go/webtransport-go/tree/v0.13.0). The application owns connection/session dispatch and datagram framing; it no longer depends on `h3-webtransport` or `h3-datagram`. The adapter's optional datagram feature remains upstream code and is disabled. Focused regression tests cover nonzero datagram and session IDs.

## Integration limits

The pinned Noq branch is not production-ready. The local noq-proto patch below
corrects reliable-reset delivery and connection credit for both ordered and
unordered reads. The application and HTTP/3 adapter still use ordered reads;
the broader upstream transport has not been exhaustively audited. Passing the
interoperability probe or an advisory scan does not establish transport
correctness.

The experimental server integrates these data paths, but complete current-draft
WebTransport conformance is unproven. Per-session flow control is deliberately
unnegotiated. The server advertises and enforces one active WebTransport session
per HTTP/3 connection; ordinary HTTP/3 requests may coexist with that session.
Stream-association headers have a ten-second deadline; classified
streams arriving before CONNECT are bounded to 64 per connection with the same
expiry. Queued datagram payload is bounded to 256 KiB per connection. Broader
reordering, cancellation and hostile-peer behavior still need validation.
An immediate-reset probe identifies an existing Go byte-reader defect; see the
parent README for the unchanged-peer failure and diagnostic correction.
Advertising the current setting does not establish complete protocol guarantees.

## Noq protocol source patch

`noq-proto/` is copied from the `noq-proto` directory at exact upstream commit
`ca4b44552c6df355906f45c58e992ddbb7986bbb` (package version `1.0.0-rc.1`).
`LICENSE-MIT` and `LICENSE-APACHE` contain the original license texts rather than
upstream workspace-relative symlinks. Source files, benches and upstream
property-test regression seeds are retained; build products and unrelated
workspace packages are omitted. `Cargo.toml` resolves workspace-inherited
package fields, dependencies, feature unions and lints to their original values.
The application workspace's Git-source patch makes this package replace the
pinned Git noq-proto dependency; Noq itself and noq-udp remain Git dependencies.

The production source change is confined to
`noq-proto/src/connection/streams/recv.rs`, `Recv::stop`: stop releases credit
through the stored reliable delivery cap instead of the stream's final size.
RESET_STREAM_AT already releases credit for the undeliverable tail; releasing
that tail again inflates connection flow-control credit. The stored cap is the
maximum of the wire reliable size and bytes already read, preserving subtraction
when an ordered application read beyond the later reliable size.

Regressions in `src/connection/streams/state.rs` cover both event orders:
reset(final 100, reliable 40), read 10, stop; and read 80, reset(final 100,
reliable 40), stop. Each must release exactly 100 credits over its lifetime.
Unpatched upstream releases 160 and 120 respectively. Unordered reads now track
delivered byte ranges, so a consumed tail neither masquerades as the reliable
prefix nor earns connection credit twice. Plain and reliable reset after a
stopped receive stream now release only the unseen final tail. The focused reset
tests and complete 410-test noq-proto suite pass locally.

## Native client support

- `h3/src/client/builder.rs`: expose the same WebTransport settings controls as
  the server builder. Existing connection internals supply stream dispatch; no
  HTTP/3 polling behavior changes.
- `h3-noq/src/webtransport_send.rs`: shared application-authored bounded reset
  queue, relocated from the server so native clients use the identical prefix
  cancellation discipline. Association encoding uses h3's existing QUIC varint
  codec. `src/lib.rs` exports it and the manifest enables Tokio `sync` explicitly.
  The server module reexports this implementation. Cleanup owners must finish
  queued prefixes or close their connection before dropping cleanup futures.
