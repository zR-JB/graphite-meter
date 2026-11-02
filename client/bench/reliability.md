# Shared-link reliability investigation

These are local diagnostic observations, not a failure-rate estimate or a release-readiness claim.
The baseline was main `48181966bb9f062972ed0c0b40114184f97753e6`, Chrome for Testing 156.0.8078.47,
with the Rust server and client built in the `ci` profile on Linux/WSL. The diagnostic and its options are
in `docs/DEVELOPMENT.md`. Each scenario started a fresh local fleet; repetitions used the same browser page.
Unless otherwise stated, the original throughput comparisons used WebSocket latency to isolate throughput.

The shared-link model gives all servers one 30 Mbit/s down, 8 Mbit/s up access link, 100 ms RTT, and
0.3% configured random loss each way. For the changing-link runs, during download the link is reduced to
5/1 Mbit/s and 1.5% loss for two seconds. Loopback TSO, GSO, GRO, UDP segmentation, and GSO-list offloads
were disabled for the HTTP/3 comparisons.
The queue is a synthetic Linux `netem` queue, not a Wi-Fi, cellular, tunnel, or reverse-proxy simulation.

## Default browser paths

The following use HTTP/1.1 throughput and WebTransport latency, with automatic lane counts and corrected
loopback offloads. The server-recorded paths confirmed the selected protocols. Repetitions share a browser
page and link queue, so they are not independent trials.

| Baseline scenario                                        | Complete runs | Observation                                                                                                                                                             |
| -------------------------------------------------------- | ------------: | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Changing 30/8 Mbit/s link, 1 server                      |           2/2 | Both complete.                                                                                                                                                          |
| Changing 30/8 Mbit/s link, 4 servers                     |           2/2 | Both complete.                                                                                                                                                          |
| Two-second UDP outage during download, 1 server          |           2/2 | The affected latency probes timed out, but the runs completed.                                                                                                          |
| Two-second UDP outage during download, 4 servers         |           2/2 | The affected server's probes timed out, but the runs completed.                                                                                                         |
| Steady 30/1 Mbit/s link, 1 server, 10 s transfer stages  |           2/2 | Both complete.                                                                                                                                                          |
| Steady 30/1 Mbit/s link, 4 servers, 10 s transfer stages |           0/2 | The first run lost all upload paths at the 3.5 s readiness deadline. The next start failed its throughput checks while the queue still held packets from the prior run. |

On the same slow-uplink setup, the candidate with the longer preparation window got past upload setup in
its first run, but all four upload progress feeds later went silent and HTTP recovery checks timed out.
Its next start failed its throughput and latency checks. This demonstrates an unresolved failure, not a
validated fix; the second attempt in each case is also a recovery observation, because the queue had not
necessarily drained. The progress-feed watchdog currently treats 1.5 seconds without an observation as a
lost feed, then requires an HTTP response within three seconds to reopen it. A shared congested uplink can
delay both progress and recovery traffic, but these samples do not prove which component caused the delay.

### Follow-up: upload congestion and startup outage

Fresh fleets on Chrome 156.0.8078.79 and merged main `d0b3ae11` used the same 30/1 Mbit/s,
100 ms RTT, 0.3% loss model. With four automatic HTTP/1.1 lanes per server, a sampled run lost
all four upload paths while the uplink queue grew to about 770 KB. A matched WebSocket-latency
run also lost all four upload paths, with a peak queue around 687 KB. With one lane per server,
two independent runs still lost two and three servers respectively; in the sampled run the queue
peaked around 363 KB. These few observations are not a failure-rate estimate.

As a diagnostic control, limiting the synthetic uplink queue to 64 packets kept it around 60–115 KB;
three servers completed and one missed upload preparation. This is evidence that queue delay contributes
to the failure in this model, not a recommendation that users change their router.

Reducing each upload lane's 128 KiB minimum POST was tested and discarded: with 16 KiB, one of three
fresh four-server runs completed and the others lost one and three servers; one 4 KiB run lost three
servers. Those changes are not shown to provide reliable recovery or preserve fast-link measurement.

One matched terminal-client run with four HTTP/1.1 lanes per server also lost all upload paths before
measurement: receiver checkpoints did not arrive in time and the uplink queue peaked near 957 KB. This
shows the shared-link failure can affect both clients; a single run does not compare their general reliability.
Increasing the terminal client's opening window to 15 seconds and each opening checkpoint's budget to
10 seconds was tested and discarded: all four uploads still failed, and the queue grew to about 1.8 MB.
With the 64-packet queue control, one terminal run still lost all four opening checkpoints; in the one
browser control above, three paths completed. These individual runs do not establish relative failure rates.
A temporary terminal build reserved a control connection before starting upload lanes: on the 64-packet
queue it reached checkpoints and entered upload, but three servers later stopped delivering data; on the
1000-packet queue all four still failed before measurement. The experiment was discarded.
In a separate control, four individually shaped 30/1 Mbit/s links completed all four browser paths; that
control has 4 Mbit/s of aggregate uplink capacity, so it isolates shared-link pressure rather than keeping
total capacity equal.

Two fresh four-server browser runs with configured random loss set to zero still lost all upload paths:
the shared uplink queue had no recorded drops in the first run and peaked around 768 KB. In the second,
each server returned two or three successful checkpoint responses during the run. This isolates a failure
under queue delay without injected random loss; it does not prove physical links have the same queue behavior.

One ten-second startup UDP outage affecting the fourth server recovered after the outage: all four
servers completed with WebTransport latency, with one failed handshake recorded. When all four servers'
UDP traffic was interrupted for ten seconds, the original page exhausted its four-failure budget and
all four latency paths stayed unavailable after the link recovered, although HTTP/1.1 transfers completed.
Tracing showed that each initial failed latency dial triggered another dial that the readiness check then
cancelled. The revised check ends on the failed dial; with the page's bounded allowance raised to eight,
two fresh shared-outage runs completed all four paths, with four failed handshakes in each. These are small
local trials; they do not cover tunnel reconnection or reverse-proxy behavior.

With a single server's UDP interrupted for eight seconds during download, both fresh four-server browser
runs completed and the interrupted server recorded 26 and 27 timed-out download-stage latency probes.

| Baseline browser scenario                                               | Complete runs | Observation                                                         |
| ----------------------------------------------------------------------- | ------------: | ------------------------------------------------------------------- |
| HTTP/1.1, shared link, no change, 1 to 4 servers                        |           8/8 | Two runs at each server count; no reported failures.                |
| HTTP/3, shared link, changing link, 1 server, 5 s stages                |           2/2 | Both complete.                                                      |
| HTTP/3, shared link, changing link, 4 servers, 5 s stages               |           0/2 | Download participants dropped; surviving upload preparation failed. |
| HTTP/3, shared link, steady loss, 4 servers, 10 s stages                |           0/2 | Both partial; one or more download participants dropped.            |
| HTTP/3, separate 30/8 Mbit/s links, steady loss, 4 servers, 10 s stages |           2/2 | Both complete.                                                      |

The HTTP/1.1 sweep preceded disabling UDP segmentation; it used TCP. The four-server steady-loss and
changing-link HTTP/3 comparisons above used the corrected offload settings. HTTP/3 Automatic opened one
fetch lane per server, confirmed in browser worker commands; HTTP/1.1 Automatic opened four in this setup.
Chromium's default of six [sockets per destination group](https://chromium.googlesource.com/chromium/src/+/main/net/socket/client_socket_pool_manager.cc)
is not a universal HTTP stream limit.

With matched one-lane HTTP/3 and the changing shared link, the original terminal client was incomplete in
2/2 runs. The opening-checkpoint retry candidate produced one incomplete and one partial run. The revised
browser candidate was partial in 2/2 runs at the default ten-second transfer-stage length. These samples
show that the fixes do not resolve the shared-link failure. They do not establish which client is generally
more reliable; the timing of the injected link change differs slightly between browser and terminal runs.

Two experiments were rejected: increasing mid-stage silence tolerance caused two of three ten-second runs
to lose every server, and a longer initial HTTP/3 download body left all three runs partial. The current
failure still needs investigation with real WANs and tunnel/proxy deployments, more repetitions, and
transport-level traces. In particular, per-server silence while another server moves can occur on a shared
bottleneck; it does not by itself prove the silent server or its independent path is down.
