# A look around Graphite Meter

[Project overview](../README.md) · [Quick deployment](DEPLOYMENT.md#fast-local-deployment) · [What the numbers mean](MEASUREMENTS.md)

Every capture is a real run against local servers over loopback held to 1 Gbit/s, so the rates show that limit on
one machine, not a network or a benchmark.

## The completed test

Throughput and responsiveness share the screen: the dial, the latency card with idle and loaded box plots and what
each load added, and one card per stage whose graph keeps the ramp-up and the latency under load.

<img src="assets/desktop.png" alt="Completed desktop test with the dial, the latency card and download, upload and bidirectional cards" width="1080">

<img src="assets/light.png" alt="The same completed test in the light theme" width="1080">

## Settings and Details

Settings choose servers, connection paths, stage timings and display options. Details name the tested server and
the paths a run used, separating what the browser observed from what reached the server. On a wide desktop both dock
beside the meter.

<img src="assets/settings.png" alt="Settings docked beside the completed test, with the server checklist and connection paths" width="1080">

<img src="assets/endpoint.png" alt="Details beside the completed test: the tested server, the paths used and the protocol evidence from browser and server" width="1080">

<img src="assets/workspace.png" alt="Wide desktop with Settings and Details docked on both sides of the meter" width="1080">

## Several servers

Up to four servers share one run and each is probed for latency. One selector over the dial shows all servers or
one; the stage cards, the latency card and Details follow it, and the latency card names its server. A server that
leaves marks its stage **Partial**, and the card names the reason.

<img src="assets/multi-server.png" alt="A three-server run with the server selector and the latency card naming its server" width="1080">

<img src="assets/partial.png" alt="A three-server run where one server stopped delivering data during download, marked Partial" width="1080">

## History on your device

Saved results are grouped by day. Each row shows the rates with the latency their load added, and a result opens with
the live meter's stage and latency cards, its server selector and the evidence sections.

<img src="assets/history.png" alt="History list grouped by day with a result open beside it" width="1080">

## Phone

<p align="center">
<img src="assets/mobile.png" alt="Phone view of the completed test" width="320">
<img src="assets/mobile-history.png" alt="Phone view of a saved result" width="320">
</p>

## Native terminal client

The TUI runs the same measurement against the same servers and ends with the results and a timeline.

<img src="assets/tui.png" alt="Native terminal client after a complete latency, download and upload run" width="1080">

Browser captures: production build, Chrome for Testing 151, 1600 × 1000 (workspace 1920 × 1080) and 430 × 932 at
2× density, stages latency 4 s, download and upload 8 s, bidirectional 6 s. The terminal capture is the TUI's own
120 × 40 screen rendered as text. To measure your own network, follow [deployment and configuration](DEPLOYMENT.md).
