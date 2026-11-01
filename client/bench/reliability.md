# Shared-link reliability investigation

These are local diagnostic observations, not a failure-rate estimate or a release-readiness claim.
The baseline was main `48181966bb9f062972ed0c0b40114184f97753e6`, Chrome for Testing 156.0.8078.47,
with the Rust server and client built in the `ci` profile on Linux/WSL. The diagnostic and its options are
in `docs/DEVELOPMENT.md`. Each scenario started a fresh local fleet; repetitions used the same browser page.

The shared-link model gives all servers one 30 Mbit/s down, 8 Mbit/s up access link, 100 ms RTT, and
0.3% configured random loss each way. For the changing-link runs, during download the link is reduced to
5/1 Mbit/s and 1.5% loss for two seconds. Loopback TSO, GSO, GRO, UDP segmentation, and GSO-list offloads
were disabled for the HTTP/3 comparisons.
The queue is a synthetic Linux `netem` queue, not a Wi-Fi, cellular, tunnel, or reverse-proxy simulation.

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
