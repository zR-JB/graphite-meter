# Benchmarks

**What a browser can move, what it costs the server, and what each measured tuning knob is worth.**
Two passes, July 2026, on the `feat/webtransport` branch. The raw NDJSON rows are not kept in the
repository: the tables below are the surviving statistics. **The campaign is closed and the knob
sweeps have been removed with it.** The harness under `client/bench/` keeps only the lane sweep —
the one finding that changed a shipped default, and the one to revisit when a new protocol lands —
so every figure below that names a `tuning.ts` field is a historical measurement with no cell left
to re-run. Re-taking one means restoring the sweep and the override plumbing it drove.

Pass one ran on an 8-core VM with 7 GB of RAM; pass two on an 8-core/16-thread x86 desktop with
30 GiB, recent Linux, `performance` governor, and enlarged TCP buffers (`tcp_rmem 4096 131072
33554432`, `tcp_wmem 4096 16384 4194304`). Browsers were the Playwright-pinned Chromium and Firefox
builds of each pass plus a Mozilla-distributed Firefox binary; the pass-two browser build ids were
not recorded.

## Method

Playwright drives the **production workers** through a bench-only `window.__gmBench` hook, not a
reimplementation of the transfer loop. A cell is a warmup, a measured window and a teardown —
3 s and 8 s by default, `GM_BENCH_WARMUP_MS` and `GM_BENCH_MEASURE_MS`, and every figure here was
taken at those defaults — and appends one NDJSON row as it completes; cells run in a fresh
permutation each repeat round, so drift over a multi-hour session widens the spread rather than
favouring whichever cell ran first. Rig A is loopback — no delay, no loss, no rate cap — and prices code, because the path
contributes nothing. Rig B (`client/bench/rig.sh`) is a network namespace joined by a veth pair
under `netem` shaping, and is where lane counts and protocol choices mean anything.

### The decision rule

A claim needs both: interquartile ranges disjoint, **and** medians differing by more than
`max(2ε, 5%)`. ε comes from matrix cells that are the same configuration under different names —
2.33% download and 0.35% upload, Chromium, pass two. Medians throughout, nearest-rank percentiles.

The Chromium matrix ran twice. Within a run the spread is that 2.33%; between runs the same cells
move by up to 6%, and h2 by 13%. The headline h1-clear two-lane download cell is one of them: 49.00
in the run tabulated below, 51.9 in the repeat. The drift is TCP-specific — QUIC cells moved under
1%. **Absolute figures carry roughly 6% uncertainty**, though no ordering or verdict differs
between the two runs.

Published figures are **sustained window means**. One run's 200 ms buckets were tabulated to
contrast mean with peak: a single download lane touched 68.30 Gbit/s instantaneously against a
53.89 window mean, so a peak quoted as throughput overstates by ~26%. **That 53.89 is one run, not
a median, and does not beat the two-lane headline.** Repeated five times, one lane medians 42.37
(range 36.34–51.33) against two lanes' 49.00 (IQR 48.77–49.65). One lane is burstier, not faster;
two lanes is both the fastest and the steadiest cell, in both Chromium runs.

### Fidelity

- The shipped app measured **−1.6% against the matched harness cell** on Chromium download
  (Firefox −3.9%, one fresh-process run each). Upload parity was not separately confirmed.
- The parity page is served from the server's own embedded bundle, so the same comparison prices
  the production build against the dev bundle the harness uses.
- The server's independent byte counter agreed with the client within about 1% both directions.
- Requests whose `ProtoMajor` mismatches the listener are 404ed, so a cell that fell back to
  another transport reports **zero bytes, never a plausible wrong number**.
- Stalls are detected by page-tick lag (`maxTickMs`) rather than inferred from the rate.
- Rate buckets carry their own elapsed time. An early bug averaged over assumed intervals, and a
  failing test once restarted the Playwright worker and wiped in-memory results; hence per-run
  NDJSON appends.

### Cross-machine rule, and the calibration gate

Orderings transfer between machines; magnitudes do not. Every null replicated on the second
machine, and exactly one verdict changed: raising the upload reservoir from 256 to 1024 MiB was
worth +24% on the VM and **0.0%** natively.

On Rig B the rig must deliver about 2× the intended cap, measured with the native Go client, before
any browser cell on that profile counts. `netem`'s loss knob under-delivers by roughly 22×, so all
"loss" figures are nominal, `wifi-poor` is latency evidence only, and every loss conclusion here
rests on `lan-fast-lossy`.

## Results

### Peak throughput, Gbit/s

| Configuration                                      | Download  | Upload    |
| -------------------------------------------------- | --------- | --------- |
| **Chromium, h1 clear, 2 lanes, loopback**          | **49.00** | **16.95** |
| Chromium, h1 clear, 1 lane, instantaneous, one run | 68.30     | —         |
| Firefox, h1 clear, fresh process                   | 9.22      | —         |
| Firefox, h1 clear, settled, best lanes             | 6.45      | 7.53      |
| Firefox, Mozilla build, h1 clear                   | 14.52     | —         |
| Go native client, 1 lane                           | 71.71     | 42.83     |
| Go native client, 8 lanes                          | 362.59    | 239.71    |

Firefox has two answers because a Firefox figure depends on how long the tab has been open; see
[Firefox](#firefox). The Mozilla-build 14.52 is a single fresh-process screening run over a ~4 s
window, so it is partly window-confounded — repeated cells on that build median 11.25. The native
client is an **upper bound, not a target**: it sets 256 KiB socket buffers and an exact
`Content-Length`, neither available to a browser.

### Per transport, loopback, Chromium, n=5, best lane count

| Transport              | ↓         | at lanes | ↑         | at lanes |
| ---------------------- | --------- | -------- | --------- | -------- |
| h1 clear               | **49.00** | 2        | **16.95** | 2        |
| h1 TLS                 | 25.45     | 2        | 13.35     | 1        |
| h2                     | 13.30     | 4        | 9.77      | 4        |
| h3                     | 2.83      | 1        | 1.66      | 1        |
| WebTransport streams   | 3.01      | 1        | 1.78      | 2        |
| WebTransport datagrams | 1.62      | 1        | 0.36      | 1        |

TLS costs half the download. h2 upload is worse than h1-TLS upload on the same crypto and the same
TCP, which reproduced across both machines and is still unexplained. For h1 TLS and h2 the lane
column is not a measurement: their candidates sit within a couple of percent and the ordering flips
between runs, so read those two rows as "one to four, flat".

**Firefox inverts the ordering**, reaching 10.88 on h3 download (Mozilla build screening; 8.94
median over repeated cells) and 6.69–7.74 on WebTransport against roughly 9 on h1 clear. Which
transport is fastest has no engine-independent answer.

### Under loss, `lan-fast-lossy`, Chromium, n=5

| Transport    | ↓ best           | ↓ at 1 lane | ↑ best          | ↑ at 1 lane |
| ------------ | ---------------- | ----------- | --------------- | ----------- |
| h1 clear     | **2 lanes** 8.12 | 6.98        | **1 lane** 8.71 | 8.71        |
| h1 TLS       | **2 lanes** 6.62 | 5.42        | **1 lane** 7.60 | 7.60        |
| h2           | **1 lane** 4.02  | 4.02        | 4 lanes 5.90    | 5.36        |
| h3           | **1 lane** 2.75  | 2.75        | **1 lane** 1.55 | 1.55        |
| WebTransport | **1 lane** 2.95  | 2.95        | **1 lane** 1.62 | 1.62        |
| WT datagrams | 1.54 (no lanes)  | —           | 0.36            | —           |

**Parallel lanes pay only where a lane is a separate connection.** h1 clear gains 16% and h1 TLS
22% at two lanes, because each connection carries its own congestion window. Every multiplexed
transport peaks at a single lane on Chromium: its lanes are streams sharing one connection, so they
add no loss resilience, and h2's head-of-line blocking makes them actively worse. Firefox runs the
other way on loopback — h2 download gains about 50% from 1 to 8 lanes and WebTransport download is
best at 16 — so engine-conditional lane policy is the strongest candidate for a future pass.

**h3 upload collapses under latency.** On a 4 ms path at 3 lanes it measures 0.0153 Gbit/s against
h1 clear's 0.2705 — 18× below h1, where the same comparison on loopback is 10×. Latency makes it
worse, not better. Do not use h3 for upload.

### Server cost

Across the whole Chromium matrix as it stood then — 67 cells, 335 runs at `GM_BENCH_REPS=5`; the
shipped matrix is 44 cells since the knob sweeps were removed — the server never
exceeded **1.73 cores or 27 MiB** while delivering up to 49.00 Gbit/s. Compare the matrices by their **CPU means**: 70% of a core under
Chromium against 148% under Firefox, **2.1× the CPU for a fifth of the throughput**. The maxima
are closer than the means — 1.73 cores against 2.12, and 27 MiB against 48 — because the bytes are
delivered either way and the browser is what cannot retire them.

Throughput per fully loaded server core: 74.2 h1-clear down, 28.6 h1-TLS down, 11.6 h2 down, 3.3 h3
down, 1.1 datagram down, 0.6 datagram up. **Server efficiency spans 124× across transports**,
because the slow transports are expensive as well as slow. Transport choice is therefore a server
capacity decision: 10 Gbit/s over h1 TLS needs about 0.35 of a core, the same over h3 about three.

### The browser ceiling

The limit is the browser's own internal byte transfer, not the socket path and not the JavaScript.
Payload bytes cross a process boundary between Chromium's network service and the renderer;
collapsing that away with a `--single-process` diagnostic lifts the sustained rate from 47.6 to
69.2 Gbit/s, roughly to where the instantaneous rate already peaked. It crashes on a second browser
context and is not shippable. Removing contention between browser and server **widened** the
native/browser gap rather than closing it (4.3× on the VM, 7.4× natively), which retires contention
as the explanation. Practically: Chromium can measure a 20 Gbit link and probably not a 40 Gbit
one, and Firefox tops out between 6.5 and 14.5 Gbit/s.

### Refuted levers

| Hypothesis                        | Result                                                                                 |
| --------------------------------- | -------------------------------------------------------------------------------------- |
| Cross-origin fetch costs bytes    | **Null.** Same-origin is within noise in both directions.                              |
| Headless is slower than headed    | **~3%** across three builds, not ordered consistently.                                 |
| An optimised Chromium fork helps  | **No.** The fork is 4% _slower_.                                                       |
| A bigger read buffer helps        | **Null**, 64 KiB to 16 MiB — a 256× change in loop frequency at no cost, which also prices the per-chunk JavaScript at zero. |
| WASM can move bytes faster        | **No.** It cannot reach `fetch` or `ReadableStream`; every byte gains a copy in glue.  |
| The read loop can be written better | **No.** In the four-way screening sweep the shipped BYOB loop beats `pipeTo`, the async iterator and the default reader by ~13%. The matrix cell below prices it against the default reader alone. |

## Knob verdicts and shipped defaults

**Every verdict in this table is Chromium, h1 clear, loopback, n=5.** Every row is the value the
client ships. The constants themselves are no longer overridable and no longer live in one table:
`readBufBytes` and `reportGapMs` are `READ_BUF_BYTES`/`REPORT_GAP_MS` in
`client/src/lib/runner/workers/tuning.ts`, read by the fetch download lane and the WebTransport
session lane alike; the upload constants are module constants in `workers/upload-worker.ts` and the
session ones in `workers/wt-transfer-worker.ts`. The rows marked † were never worker constants: the
lane counts live in `client/src/lib/state/defaults.ts` and `real/streamPolicy.ts`, and chunked
download is a user setting in `state/defaults.ts`. Request streaming shipped no setting at all —
its `uploadBody: "stream"` path has since been deleted, since nothing but the sweep could reach it.

| Knob                   | Default       | Swept                        | Effect     | Verdict                    |
| ---------------------- | ------------- | ---------------------------- | ---------- | -------------------------- |
| `uploadBody`           | `blob`        | blob vs arrayBuffer          | **+98.9%** | **real**, largest measured |
| `uploadTotalPoolBytes` | 256 MiB       | 16 → 64 MiB                  | **+29.7%** | **real**                   |
| `reader`               | `byob`        | byob vs default, matrix cell | **+15.4%** | **real**                   |
| `uploadTotalPoolBytes` | 256 MiB       | 64 → 256 MiB                 | **+10.9%** | **real**                   |
| lanes, download †      | see below     | 2 vs 4                       | **+9.6%**  | **real**                   |
| lanes, upload †        | see below     | 2 vs 8                       | **+5.3%**  | **real**, barely           |
| request streaming †    | off           | blob vs streamed, 4 ms RTT   | +4.2%      | null, under the bar        |
| lanes, upload †        | see below     | 1 → 4                        | 3.0%       | null                       |
| `reportGapMs`          | 50 ms         | 50 vs 200                    | 2.5%       | null                       |
| chunked download †     | off           | false vs true                | 2.1%       | null on Chromium           |
| `readBufBytes`         | 1 MiB         | 64 KiB → 16 MiB              | 2.1%       | null                       |
| `targetPostMs`         | 500 ms        | 250 → 2000                   | 1.6%       | null                       |
| `uploadDrain`          | `arrayBuffer` | arrayBuffer vs cancel        | 0.2%       | null                       |
| `uploadTotalPoolBytes` | 256 MiB       | 256 → 1024 MiB               | **0.0%**   | null                       |

**Effect** is the first swept arm against the second, or the low end against the high end where the
sweep is ordered.

`minPostBytes`, `writeChunkBytes` and `congestionControl` are **unmeasured,
not null**. Sweeps are one-knob-at-a-time coordinate descent against a fixed baseline, so a jointly
better (lanes, buffer) ridge would be invisible to them.

**The two biggest effects are Chromium-only.** On Firefox both are null: `uploadBody` reads 4.75
against 5.10 and `reader` 6.35 against 6.26, with overlapping interquartile ranges. Two effects run
the other way and are larger on Firefox: the reservoir matters four times as much (16 → 256 MiB is
+347% there against +43.7% on Chromium), and Firefox upload gains 80–84% from eight lanes where
Chromium loses. Firefox's upload noise floor is 15.6%, so an upload effect there must reach 31% to
be reportable at all.

The reservoir mechanism explains an older result: the per-lane pool is `total / lanes` and is also
the adaptive sizer's ceiling, so a small pool made upload appear to degrade with lane count. That
artefact, not the transport, produced pass one's lane-count verdicts.

### Shipped defaults

The values this change ships. **Every row is the browser client's unless it names the native
client.** The worker measurement constants, the upload reservoir, chunked download and the
connection budget exist only in the browser and have no native counterpart. The h2/h3 per-direction counts and
the one-stream-per-direction WebTransport rule are the same table in both clients
(`client/src/lib/runner/real/streamPolicy.ts`, `go/internal/goclient/config.go`).

| Setting                      | Value                                    | Why                                                                                                                          |
| ---------------------------- | ---------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| Automatic H1 stream ceiling, browser | **4** (was 6)                    | 5 was an arithmetic accident of the 6-connection budget split; 4 is a measured point on the plateau for h1 clear and h1 TLS under loss. 2 is the loopback optimum and stays available as an explicit setting. |
| Automatic H1 stream ceiling, native client | **6**, unchanged           | The two differ because lane count is a loss-resilience mechanism whose plateau under loss was flat from 2 to 6 on h1 clear, the one origin carrying the full lane sweep (h1 TLS was swept at 1, 2, 4 and 8), so nothing in that range forces a choice, and each client's optimum is then set by its own ceiling: the browser's is its internal byte transfer, which 2 lanes already saturates and 4 does not exceed, while the native client has none and scales 71.71 → 362.59 Gbit/s down from 1 to 8 lanes on loopback. Above 4 the browser only loses lanes to its own connection budget. The native figures are loopback peaks, not loss-rig measurements: they establish that the client has no internal ceiling of its own, not that 6 lanes is its optimum under loss. The 6-connection budget split is browser-only. |
| h2 upload streams            | **4**                                    | +10.1% from 1 to 4 lanes under loss, disjoint IQRs; loopback agrees independently.                                            |
| h3 upload streams            | **1**                                    | −9.3% over the same range under loss, disjoint IQRs; loopback agrees. One constant cannot serve both protocols.               |
| Multiplexed download streams | **1**                                    | h3 download declines monotonically with lanes, so 1 is its measured optimum. h2 download is flat across 1 to 4 within noise, so 1 is taken as the cheapest point, not as a measured optimum — no h2 lane conclusion is drawn from this data (see [Limitations](#limitations-and-open-questions)). |
| WebTransport streams, automatic | **1** per direction                   | Mechanism, not measurement: a WebTransport lane is one continuous stream per direction and nothing turns around per request, so `real/streamPolicy.ts` returns 1 for both. The one WT upload measurement (best at 2 lanes, 1.78 Gbit/s) was not acted on — the transport sits an order of magnitude below the TCP transports, where lane tuning decides nothing. |
| `BROWSER_CONNECTION_BUDGET`  | 6, unchanged                             | Nothing measured argues against it; it exists to avoid starving the browser's own per-origin pool.                            |
| Upload reservoir             | **256 MiB**; 24 MiB at `deviceMemory` ≤ 4, 16 MiB at ≤ 2, 128 MiB when `deviceMemory` is absent | Four tiers (`upload-worker.ts`, `uploadPoolBytes`), each then divided by the lane count and floored at 2 MiB. Only Chromium reports `navigator.deviceMemory`, and an unknown device is not evidence of a large one. Pass one OOM-killed a 7 GB VM on the full reservoir. The 16 and 24 MiB thresholds are **unmeasured** — no mobile device was benchmarked — but they do ship. |
| `PER_STREAM_BYTES`           | 64 GiB, unchanged                        | Bounding it enough to matter costs Firefox a third to a half of its throughput; the mitigation is the chunked-download setting. |
| Chunked download             | off, user setting                        | −94% Firefox peak RSS for −40% throughput; free and pointless on Chromium. An engine-conditional default would make two engines answer differently about the same link. |

**No new server-side knob follows from any of this.** The unresolved compromises — lane count per
engine, chunked download, reservoir size — are device- and engine-scoped, which a server operator
cannot know, and the deployment-scoped bounds an operator can know already exist
(`GM_MAX_SESSIONS_PER_CLIENT`, `GM_MAX_SESSION_DURATION`). An engine-conditional upload lane count
is the strongest candidate for a future pass, pending data from a second machine.

## Firefox

Firefox is **3.4× to 7.6× slower than Chromium** depending on its state: its own endpoints are
14.52 and 6.45 against Chromium's 49.00, so no single multiplier describes it. **A Firefox number
is meaningless without saying when it was taken.** The first substantial download in a fresh
process runs faster than every one after it. The direction replicates in every
session; the magnitude does not, spanning 12–43% across three sessions, because Firefox's settled
rate is itself unstable between browser processes. Chromium has no equivalent effect, reading
within 0.6% across the four quarters of a 335-run session. Every matrix figure here is
settled-state; every screening figure is first-in-process.

### The memory defect

Firefox retains roughly **1.6 bytes resident per byte downloaded**, releasing them only when the
transfer ends. The ratio is flat from 1 to 10 Gbit/s: there is no rate below which this stops, only
a slower path to the same total. It reproduces in an independent implementation (self-hosted
OpenSpeedTest) on the same host, so it is not this code.

Response size is what triggers it: a download lane asks for `PER_STREAM_BYTES` in one response, and
Firefox accumulates within a response and releases between them. The RSS cliff sits between a
256 MiB and a 1 GiB response; 64 MiB responses hold about 526 MiB resident at ~5.6 Gbit/s. Capping
responses at 4 GiB is throughput-free and buys back no memory at all.

Decision: `PER_STREAM_BYTES` stays at 64 GiB and the mitigation is the chunked-download setting,
which trades 40% of Firefox's throughput for 94% of its peak RSS. Above roughly 2 Gbit/s a Firefox
client needs chunked download or a byte-bounded rather than time-bounded window; at 20 Gbit/s an
8 s window is about 32 GB resident, which is an out-of-memory kill on a 32 GB machine. The "memory
ceiling" that truncated Firefox matrix runs during this campaign was an operator-imposed cgroup at
about 26 GiB — a machine with more RAM will not reproduce that kill.

**Do not raise Firefox's socket-buffer pref.** Moving it from 32 to 64 KiB is worth +31% on
loopback, nothing at all on a gigabit link, and costs −12% to −29% on an uncapped lossy path, the
cost growing with lane count. It also confounds lane measurements: with the pref raised Firefox's
download lane curve reads flat, and the default arm — what a user actually has — does not.

**The OpenSpeedTest control, stated honestly.** This tool's download advantage is 3.2× on Chromium
and 2.0× on Firefox; upload is roughly at parity on both. The worker pool and the zero-copy BYOB
read loop are a download advantage, not a general one. Control gotcha: rootless podman's userspace
network stack caps throughput in the low single-digit Gbit/s, so the control must run
`--network=host` or it measures the container runtime.

## Reproduction

The harness stays under `client/bench/`; this document is its README. **`../.dev-certs` must exist
for every run**, including an h1-clear-only one: the config starts all four listeners and points
`GM_TLS_CERT`/`GM_TLS_KEY` at that pair unconditionally, and the server refuses to start when it
cannot load them. Generate them as [DEVELOPMENT.md](DEVELOPMENT.md) describes.

The harness runs against an ordinary dev server: with the knob sweeps gone it sends no measurement
overrides, so there is nothing to compile in and no build flag to remember. Everything below is
env-only. A project whose prerequisite variable is unset is dropped rather than guessed at, so
Playwright rejects `--project=<name>` by name instead of measuring nothing.

| Variable                | Read by                     | Effect                                                                                     |
| ----------------------- | --------------------------- | ------------------------------------------------------------------------------------------ |
| `GM_BENCH_SPKI`         | `playwright.bench.config.ts` | Base64 SHA-256 of the dev leaf's SPKI, for QUIC. **Required**: unset, the `chromium` project does not exist and `--project=chromium` fails with "Project(s) not found". |
| `GM_BENCH_FIREFOX`      | `playwright.bench.config.ts` | Path to a Mozilla-built binary; gates the `firefox-stock` project the same way.            |
| `GM_BENCH_HOST`         | `playwright.bench.config.ts` | Address the server binds and the browser dials. Default `127.0.0.1`; Rig B uses the namespace address. |
| `GM_BENCH_NETNS`        | `playwright.bench.config.ts` | Prefixes the server command with `ip netns exec <name>`. **Unset means no prefix at all.**  |
| `GM_BENCH_FF_NETBUF`    | `playwright.bench.config.ts` | Sets Firefox's `network.buffer.cache.size` on the `firefox` project only. Off by default — see [Firefox](#firefox). |
| `GM_BENCH_TLS_CERT`     | `playwright.bench.config.ts` | Leaf the TLS listeners serve. Default `../.dev-certs/localhost.pem`, and it is read on every run. |
| `GM_BENCH_TLS_KEY`      | `playwright.bench.config.ts` | Its private key. Default `../.dev-certs/localhost-key.pem`.                                |
| `GM_BENCH_ORIGINS`      | `throughput.bench.pw.ts`     | Comma list of origins to measure. Default `h1-clear`; the matrices here used all four.     |
| `GM_BENCH_REPS`         | `matrix.ts`                  | Repeat rounds per cell. Default 3; every n=5 figure here needs `5`.                        |
| `GM_BENCH_WARMUP_MS`    | `matrix.ts`                  | Warmup discarded before each cell's window. Default 3000, which every figure here used.    |
| `GM_BENCH_MEASURE_MS`   | `matrix.ts`                  | Measured window per cell. Default 8000, which every figure here used.                      |
| `GM_BENCH_SEED`         | `matrix.ts`                  | Seed for the per-round cell permutation. Default 1; changing it reorders cells, not the set. |

```sh
# Full matrix, one engine. ALWAYS pass --project: without it every engine runs,
# which on a memory-limited machine means the Firefox arm and an OOM kill.
cd client && GM_BENCH_SPKI=<pin> GM_BENCH_ORIGINS=h1-clear,h1-tls,h2,h3 GM_BENCH_REPS=5 \
  bunx playwright test -c playwright.bench.config.ts --project=chromium

# One cell, one engine. The recipe's second argument is the --project passthrough;
# without it the filter still runs against every project.
GM_BENCH_SPKI=<pin> just bench-throughput 'h1-clear/down/lanes=2' chromium

# Shaped path. The server is started inside the namespace by hand: entering one
# needs CAP_SYS_ADMIN, and running Playwright under sudo would run the browser as
# root. GM_BENCH_NETNS is left unset here, so the config adds no `ip netns exec`
# prefix of its own.
sudo client/bench/rig.sh up lan-fast-lossy
sudo ip netns exec gmbench <server binary, bound to 10.77.0.2>
GM_BENCH_SPKI=<pin> GM_BENCH_HOST=10.77.0.2 \
  bunx playwright test -c playwright.bench.config.ts --project=chromium
sudo client/bench/rig.sh down
```

`reuseExistingServer` then picks up the namespace server rather than starting its own. `just
bench-wire` carries the ping-bus encoding evidence, and `just stress` the server saturation
envelope described in [ARCHITECTURE.md](ARCHITECTURE.md#saturation-envelope-just-stress).

### Gotchas that cost hours

- Firefox needs the local CA **genuinely** trusted for QUIC (`security.enterprise_roots.enabled`);
  `ignoreHTTPSErrors` does not cover QUIC. It also needs h3 unblocked on loopback.
- `Alt-Svc` is advertised only on the h3 TCP companion's `/probe`, and that companion carries no
  transfer routes, so lanes opened before the upgrade lands hit routes it does not serve. Poll
  until the **server** reports h3 negotiated; `nextHopProtocol` is masked cross-origin. Chromium's
  `--origin-to-force-quic-on` masks that whole bootstrap problem, so verify it on Firefox.
- A parity run compares whatever the server actually serves. Rebuild and re-embed the client first;
  a bare `go build` serves the stale bundle. (The parity test itself is gone: it printed a slice of
  page text and asserted it was non-empty, so it never read the harness number it claimed to check.
  The −1.6% above was taken by hand.)
- `wt-datagram` download once hung a browser to death — the only browser death of the campaign.

## Limitations and open questions

Stated so a blank is never mistaken for a null. Everything here is one machine per pass, so
absolute rates are machine-specific.

- **Knob sweeps are h1 clear only**, at one lane count each. Whether `arrayBuffer` collapses the
  same way on h2 or h3 is unmeasured, and the multiplexed protocols frame request bodies
  differently. Knob generalisation has already failed across engines twice.
- **h2 varies by more than 13% between identical runs**, with a within-run interquartile range at
  one lane spanning 9.71 to 14.82. No h2 lane-count conclusion should be drawn from this data.
- **QUIC upload regressed absolutely between passes** on a faster machine — h3 3.1 → 1.66 and
  WebTransport streams 2.9 → 1.78. Unexplained, possibly a browser-build difference, and the
  pass-two build ids were not recorded.
- **Firefox shaped coverage is two profiles**, not four; the engines diverge on every transport
  where both were measured, so the Chromium-only profiles are not engine-general.
- **Nothing was measured** over a real internet path, over wifi hardware, on a mobile device, on
  Safari (which also lacks `deviceMemory`), or above 10 Gbit/s of shaped capacity.
- **The raw rows are gone.** Roughly 2,600 NDJSON rows across both passes backed the matrix
  figures; these tables are what survives them. Screening and one-off sweeps — drain strategies,
  launch flags, response-size, resource sampling, native-client runs, rig calibration, and the
  app-parity comparison, whose test asserts on the page's own text and writes no row — were never
  NDJSON-backed in the first place.
