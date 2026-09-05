# Upload measurement protocol (0.7)

Every upload belongs to a server-minted, owner-bound measurement session. Clients
must update alongside the server for 0.7: HTTP uploads without an ID are rejected,
and counter records always contain explicit `bytes` and `nanos` fields.

Read this when implementing receiver-authoritative upload accounting. The [discovery boundary](discovery.md)
defines control-response validation; the [wire protocol](wire.md#webtransport-routes) defines
WebTransport routing. [Measurement definitions](../docs/MEASUREMENTS.md#throughput) distinguish
receiver windows from presentation.

## Session and data ownership

1. `POST /upload/session` returns `{"uploadId":"..."}`. The caller uses that ID
   for the aggregate's data lanes and progress feed.
2. `GET /upload/progress?id=...` attaches to the receiver's NDJSON feed. Its
   `ready` record confirms that the aggregate and reader claim are established.
3. `POST /upload?id=...` sends raw upload bytes. The server validates the ID and
   owner before reading the body. Missing, forged, expired, or foreign IDs are
   refused without counting data. A completed POST replies with that POST's
   byte count; this response does not replace the aggregate progress feed.
4. `DELETE /upload/progress?id=...` finalizes the aggregate. The progress feed
   emits `complete` after active data lanes have drained, then closes.

WebTransport `/wt/upload?id=...` uses the same aggregate and ownership rules. Its
server-opened unidirectional stream carries the same NDJSON feed; client streams
or explicitly requested datagrams carry data. Minting and finalizing still use
HTTP. All authentication, origin, admission, and resource policies apply before
endpoint work.

Reconnecting retains the same ID and receiver counters. A replacement progress
reader supersedes its predecessor. Watching a feed does not keep an idle upload
alive; existing idle expiry, finalization retention, and capacity limits remain
in force. A refused WebTransport lane can be reported on another control stream.
See [upload refusal codes](uploadrefusals.txt) for the shared classifications.

## Progress records

Each record is a JSON object followed by a newline. Blank lines are heartbeats.

| `type`     | Fields                            | Meaning                                                       |
| ---------- | --------------------------------- | ------------------------------------------------------------- |
| `ready`    | none                              | The feed is attached; this does not prove byte delivery.      |
| `progress` | `bytes`, `nanos`                  | One cumulative receiver observation.                          |
| `complete` | `bytes`, `nanos`                  | Final receiver observation after finalization and lane drain. |
| `error`    | optional string `code`, `message` | Explicit refusal; no counter observation.                     |

`bytes` is the number of payload bytes received across this upload aggregate's
lanes. `nanos` is elapsed time in nanoseconds on the receiver's monotonic clock,
from its first accepted payload chunk through the corresponding observation.
An aggregate with no accepted chunk has `bytes: 0, nanos: 0`. Counters are JSON
numbers representing integers between 0 and 9,007,199,254,740,991, inclusive, so
both clients can represent them exactly. Strings, null, missing values,
fractions, negative values, and larger numbers are invalid.

Clients retain byte/time observations together. If either counter regresses,
the entire observation is stale and is ignored; a client never combines retained
bytes with a new timestamp. Equal counters can be repeated, and a final record
can confirm the last observation without adding bytes. Monotonic state belongs
to the upload ID and survives feed replacement.

Malformed or unknown records are not observations. An invalid or stale
`complete` record is not evidence that the receiver aggregate finalized. EOF
during an active measurement follows bounded feed recovery; failure to recover
may interrupt that measurement. After the measurement window ends, finalization
and terminal-record collection are bounded cleanup. Readers bound each record
to 64 Ki units (bytes in Go, UTF-16 code units in the browser) and release their
input on termination.

Explicit zero counters are valid data, but a zero-duration window cannot produce
a rate. Measurement starts from a receiver checkpoint after warmup; rates use
subsequent byte/time deltas. Missing progress or a missing terminal record must
not fabricate counters or a zero rate. A successfully sampled receiver window
can remain valid without a terminal record; `complete` confirms aggregate
finalization independently of the measured window outcome.

The Go and TypeScript decoders share
[record conformance fixtures](upload-progress.testvectors.json).
