# A look around Graphite Meter

[Project overview](../README.md) · [Quick deployment](DEPLOYMENT.md#fast-local-deployment) · [What the numbers mean](MEASUREMENTS.md)

Every capture is a real run. The servers are local, each behind a link shaper that models one client's path into
a 10 GbE home network: a laptop on Wi-Fi 7 for the completed views, a workstation wired at 10 GbE in the README's
hero, and a phone on Wi-Fi 6E. The figures are what the meter measured over those paths, not a benchmark of a
network.

## The completed test

The dial carries the run's headline and every stage's result on one ring; the latency lanes set idle and loaded
latency side by side with what each load added; one card per stage, under its key, keeps the stage's graph with
the latency its load caused underneath.

<img src="assets/desktop.png" alt="Completed test from a laptop on Wi-Fi 7: 3.42 Gbit/s down, 1.86 Gbit/s up, 2.8 ms idle latency, the latency lanes and one card per stage" width="1080">

<img src="assets/light.png" alt="The same completed test in the light theme" width="1080">

## Settings and Details

Settings choose servers, connection paths, stage timings and display options. Details name the tested server and
the paths a run used, separating what the browser observed from what reached the server. On a wide desktop both dock
beside the meter.

<img src="assets/settings.png" alt="Settings docked beside the completed test, with connection paths and stage timings" width="1080">

<img src="assets/endpoint.png" alt="Details beside the completed test: the tested server, the paths used and the protocol evidence from browser and server" width="1080">

<img src="assets/workspace.png" alt="Wide desktop with Settings and Details docked on both sides of the meter" width="1080">

## Several servers

Up to four servers share one run and each is probed for latency. One selector over the dial shows all servers or
one; the stage cards, the latency lanes and Details follow it, and the lanes name their server. A server that
leaves marks its stage **Partial**, and the card names the reason.

<img src="assets/multi-server.png" alt="Three servers on the home network sharing a run, with the server selector over the dial" width="1080">

<img src="assets/partial.png" alt="A three-server run where one server's connection was lost during download, marked Partial with the reason on the card" width="1080">

## History on your device

Saved results are grouped by day. Each row shows the rates with the latency their load added, and a result opens with
the live meter's stage and latency cards, its server selector and the evidence sections.

<img src="assets/history.png" alt="History list grouped by day with a result open beside it" width="1080">

## Phone

<p align="center">
<img src="assets/mobile.png" alt="Phone view of a completed test over Wi-Fi 6E" width="320">
<img src="assets/mobile-history.png" alt="Phone view of a saved result" width="320">
</p>

## Native terminal client

The TUI runs the same measurement against the same servers and ends with the results and a timeline.

<img src="assets/tui.png" alt="Native terminal client after a complete latency, download and upload run" width="1080">

Browser captures: production build, Chrome 154, 1600 × 1000 (workspace 1920 × 1080) and 430 × 864 (the page area
of a 430 × 932 phone) at 2× density, stages latency 4 s, download and upload 8 s, bidirectional 6 s, a 1 s warmup,
the dial's maximum automatic (10 Gbit/s on the workstation).
Paths: the laptop's carries 3.4 Gbit/s down and 1.9 Gbit/s up with 1 ms of one-way delay; the workstation's about
9.4 Gbit/s each way with no added delay; the phone's 1.65 Gbit/s down and 0.86 Gbit/s up with 1.5 ms. Each queues a
few milliseconds of data, so latency under load comes from real queueing. The three servers of the multi-server run
each have their own path, and together they fill the laptop's. The terminal capture is the TUI's own 120 × 40 screen
on the workstation's path, rendered as text. Each capture is shown as a plain screen or in a plain phone outline.
To measure your own network, follow [deployment and configuration](DEPLOYMENT.md).
