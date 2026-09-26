# A look around Graphite Meter

[Project overview](../README.md) · [Quick deployment](DEPLOYMENT.md#fast-local-deployment) · [What the numbers mean](MEASUREMENTS.md)

Every capture is a real run against local servers on loopback (the `mise run e2e` fleet), so the rates show software
limits on one machine, not a network or a benchmark.

## The completed test

Throughput and responsiveness share the screen: the gauge, idle and loaded latency lanes, result cards and a
timeline that keeps the transfer ramp-up.

<img src="assets/desktop.png" alt="Completed desktop test with download and upload curves, three latency lanes and result cards" width="1080">

<img src="assets/light.png" alt="The same completed test in the light theme" width="1080">

## Settings and Details

Settings choose servers, connection paths, stage timings and display options. Details name the tested server and
the paths a run used, separating what the browser observed from what reached the server. On a wide desktop both dock
beside the meter.

<img src="assets/settings.png" alt="Settings dock with the server checklist and connection paths beside the completed test" width="1080">

<img src="assets/endpoint.png" alt="Details with the throughput path open, showing browser- and server-observed protocol evidence" width="1080">

<img src="assets/workspace.png" alt="Wide desktop with Settings and Details docked on both sides of the meter" width="1080">

## Several servers

Up to four servers share one run; one **Combined** / per-server selector switches the result cards, and the latency
lanes name the server they show. A server that leaves marks its stage **Partial**, and the stage names the reason.

<img src="assets/multi-server.png" alt="A four-server run with the Combined selector and latency from one server" width="1080">

<img src="assets/partial.png" alt="A three-server run where one server stopped delivering data during download, marked Partial" width="1080">

## History on your device

Saved results are grouped by day and open with the live meter's result cards, per-server selector and evidence
sections.

<img src="assets/history.png" alt="History list grouped by day with a four-server result open" width="1080">

## Phone

<p align="center">
<img src="assets/mobile.png" alt="Phone view of the completed test" width="320">
<img src="assets/mobile-history.png" alt="Phone view of a saved result" width="320">
</p>

## Native terminal client

The TUI runs the same measurement against the same servers and ends with the results and a timeline.

<img src="assets/tui.png" alt="Native terminal client after a complete latency, download and upload run" width="1080">

Browser captures: production build, Chrome for Testing 151, 1600 × 1000 (workspace 1920 × 1080) and 430 × 932 at
2× density, stages latency 4 s, download and upload 8 s. The terminal capture is the TUI's own 120 × 40 screen
rendered as text. To measure your own network, follow [deployment and configuration](DEPLOYMENT.md).
