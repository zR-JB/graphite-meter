# Terminal client

`graphite-meter-client` runs the browser's measurement from a terminal. It tests against one to four test servers,
lets you choose the throughput and latency paths independently, measures idle and loaded latency, and times
throughput at the receiver. It has an interactive console and a report mode for scripts.

<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/terminal-dark.png">
  <img src="assets/terminal-light.png" width="100%" alt="The terminal client after a completed test: 2.35 Gbit/s each way, 0.4 ms idle latency, 2.7 ms added under download load and 2.8 ms under upload load">
</picture>

</div>

## Install

Each [release](https://github.com/zR-JB/graphite-meter/releases) attaches one archive per platform, holding the
executable and its license notices (`--legal` prints them too). The Linux builds are static.

| Platform       | Archive                                          |
| -------------- | ------------------------------------------------ |
| Linux, amd64   | `graphite-meter-client_X.Y.Z_linux_amd64.tar.gz` |
| Linux, arm64   | `graphite-meter-client_X.Y.Z_linux_arm64.tar.gz` |
| Windows, amd64 | `graphite-meter-client_X.Y.Z_windows_amd64.zip`  |
| Windows, arm64 | `graphite-meter-client_X.Y.Z_windows_arm64.zip`  |

To check that a download is the one the release published, run
`gh release verify-asset vX.Y.Z FILE -R zR-JB/graphite-meter` ([details](DEPLOYMENT.md#without-docker)).

```sh
tar -xzf graphite-meter-client_X.Y.Z_linux_amd64.tar.gz
./graphite-meter-client_X.Y.Z_linux_amd64/graphite-meter-client --url http://192.168.1.20:7246
```

On Windows, extract the zip and run the client in PowerShell:

```powershell
Expand-Archive graphite-meter-client_X.Y.Z_windows_amd64.zip -DestinationPath .
.\graphite-meter-client_X.Y.Z_windows_amd64\graphite-meter-client.exe --url http://192.168.1.20:7246
```

The server itself ships for Linux; on Windows, see [running the server on Windows](DEPLOYMENT.md#on-windows). To
build the client from source, see the [development guide](DEVELOPMENT.md#commands).

## The server address

`--url` names the Graphite Meter server, and its server catalog lists the test servers. Without `--url` the console
asks for the address. A pasted page URL works too, because only its origin is used.

An address without a scheme gets `http://` when its host is on the local network and `https://` otherwise. Local
means a loopback, private or link-local IP address, a name without a dot (`nas:7246`), or a name ending in
`.localhost`, `.local`, `.lan`, `.home.arpa` or `.internal`. HTTPS never falls back to HTTP.

When Remember this server is on (Tab on the address card, or the row in the settings), the client keeps the address
for the next start, and that start needs no `--url`. Turning it off deletes the file, and nothing else is kept.

| System  | File                                                                             |
| ------- | -------------------------------------------------------------------------------- |
| Linux   | `$XDG_CONFIG_HOME/graphite-meter/server`, else `~/.config/graphite-meter/server` |
| Windows | `%APPDATA%\graphite-meter\server`                                                |

## Sign-in

A server with [authentication](DEPLOYMENT.md#authentication) asks the console to sign in. The console shows a short
code and the server's approval page, and Enter (or o) opens the page in your browser. Sign in there as usual, check
that the page shows the same code, and approve. The approval waits up to 2 minutes.

The client never sees the operator password. It receives a measurement-only grant bound to that server's HTTPS
origin and keeps it in memory until it exits, and signing out on the server revokes it. Sign-in needs an `https://`
address and TLS verification, so it is refused over HTTP and with `--insecure`. Report mode cannot sign in, so
against a protected server it ends with "Sign-in required" and status 1.

## Flags

Durations take units such as `800ms`, `4s` or `1m30s`. Boolean flags accept `true`, `false`, `1` or `0`
(`--skip-loaded-latency=false`).

| Flag                                             | Default                   | Meaning                                                                                   |
| ------------------------------------------------ | ------------------------- | ----------------------------------------------------------------------------------------- |
| `--url <origin>`                                 | the remembered server     | The Graphite Meter server. Required in report mode unless a server is remembered.         |
| `--server <id>`                                  | the catalog's default     | A test server ID from the server catalog; repeat for up to 4.                             |
| `--stages <list>`                                | `latency,download,upload` | `latency` (`ping`), `download` (`down`), `upload` (`up`), `bidirectional` (`bidi`); they run in this order. |
| `--latency-duration`                             | `4s`                      | Measured time of the latency stage, 1 s to 24 h and within every test server's limit.     |
| `--download-/--upload-/--bidirectional-duration` | `10s`                     | The same for each transfer stage.                                                         |
| `--warmup`                                       | `800ms`                   | Before each stage, 0 to 4 s; stretched to at least 10 idle round trips, at most 4 s.      |
| `--throughput-origin`, `--latency-origin`        | `auto`                    | An origin from discovery, or `auto`.                                                      |
| `--throughput-protocol`                          | `auto`                    | `http1`, `http2` or `http3` for a negotiated throughput origin.                           |
| `--throughput-transport`                         | `auto`                    | `fetch-stream` or `webtransport`.                                                         |
| `--latency-transport`                            | `auto`                    | `websocket` or `webtransport`.                                                            |
| `--ping`                                         | `reply-driven`            | Idle probe interval: `reply-driven`, `fast` (80 ms), `medium` (250 ms), `slow` (600 ms) or 80 ms to 15 s. |
| `--loaded-ping`                                  | `medium`                  | Loaded probe interval, the same values.                                                   |
| `--skip-loaded-latency`                          | `true`                    | Skip loaded latency if the Latency stage is off; with that stage on, transfers probe.     |
| `--auto-streams`                                 | `6`                       | HTTP/1.1 stream limit per direction, 1 to 14.                                             |
| `--streams`                                      | `0`                       | Exact stream count per server and direction, 1 to 14; `0` is automatic.                   |
| `--insecure`                                     | `false`                   | Skip TLS certificate verification. Sign-in is refused.                                    |
| `--report`                                       | `false`                   | Run once without the console; automatic when stdout is not a terminal.                   |
| `--version`, `--legal`, `--help`                 |                           | Print the version, the license notices or the usage, and exit.                            |

## Console

The console shows the dial, the latency lanes, the run key over a chip per stage, and a card per stage. s opens
the settings beside it. The settings list the server and its test servers, connection paths, stages and then
advanced settings, and the foot of the settings explains the focused row. d opens Details with servers, paths,
rates and failures. ? lists every key.

Every selected test server is probed for latency. The console shows the first selected server's latency, or a
surviving server's once the first one leaves the test. l switches the server shown. A test server that fails its
check leaves the test, and the test continues and ends Partial.

| Keys                       | Where         | Action                                                             |
| -------------------------- | ------------- | ------------------------------------------------------------------ |
| enter, space, r            | console       | Start a test, or run again.                                        |
| esc, space, r              | console, test | Stop the test; esc first closes Settings, Details or Keys.         |
| 1, 2, 3, 4                 | console       | Switch latency, download, upload or bidirectional on or off.       |
| s, d, ?                    | console       | Settings, Details, Keys; esc or the same key closes them.          |
| l                          | console       | With several test servers, the server whose latency is shown.      |
| v                          | console       | Check the server again.                                            |
| ↑ ↓ (k j, tab), ← →        | settings      | Move; change the focused value.                                    |
| enter, space, r            | settings      | Edit or open the row; switch it on or off; start or stop a test.   |
| ← →, home, end, enter, esc | editing       | Move the cursor, apply, cancel.                                    |
| tab                        | address card  | Remember this server, or forget it.                                |
| ↑ ↓, space, enter, esc     | test servers  | Move, select, apply, cancel.                                       |
| enter (o), esc             | sign-in       | Open the sign-in page, cancel sign-in.                             |
| ↑ ↓                        | Details, Keys | Scroll.                                                            |
| pgup, pgdn, home, end      | Details       | Scroll by a page, to the top, to the end.                         |
| q                          | anywhere      | Quit; a running test stops first. While editing, q is typed.       |
| ctrl+c                     | anywhere      | Stop the test and quit; a second ctrl+c quits at once.             |

The mouse works too. A click presses the run key, a stage's chip, a key in the bottom bar or a row, and the wheel
scrolls a sheet or moves the focus in the settings. Hold Shift to select text. When the console exits after a test,
it prints the report to stdout.

The console asks the terminal for its background color and picks a light or dark palette to match; on Windows it
assumes a dark background. It sets the window title while it runs and restores the previous one on exit. Windows
Terminal, ConEmu and Ghostty also show the test's progress in their tab or taskbar.

## Report mode

With `--report`, or when stdout is not a terminal, the client runs one test without the console. It writes each
stage's name to stderr as the stage starts, and it writes the report to stdout at the terminal's width, or at 100
columns when stdout is a file or pipe.

| Status | Meaning                                                                         |
| ------ | ------------------------------------------------------------------------------- |
| 0      | The test completed, or the console quit before any test.                        |
| 1      | The test ended Partial, Incomplete, Stopped or Failed, or an error such as "Sign-in required" occurred. |
| 2      | The flags or arguments were invalid, or report mode had no server address.      |
| 130    | SIGINT (ctrl+c) stopped the test.                                               |
| 141    | On Unix, the report was written into a closed pipe.                             |
| 143    | SIGTERM, or closing the Windows console, stopped the test.                      |

A signal stops the test, and the report still prints; before the test started, stderr says "Test stopped before it
started." instead. A second signal exits at once with its own status, and a signal after the test finished does not
change the status.

## Environment

| Variable                        | Effect                                                                                                                 |
| ------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `HTTPS_PROXY`, `HTTP_PROXY`     | The proxy for `https://` and `http://` servers, given as an `http://`, `https://`, `socks5://` or `socks5h://` URL with optional credentials. Lowercase names work too, and `ALL_PROXY` is not read. |
| `NO_PROXY`                      | A comma-separated list of names (`example.com` with its subdomains, `.example.com` the subdomains only), IP addresses, CIDRs and `host:port` entries. `*` bypasses every proxy. Loopback and `localhost` never use a proxy. |
| `SSL_CERT_FILE`, `SSL_CERT_DIR` | On Linux, `SSL_CERT_FILE` replaces the system CA bundle and `SSL_CERT_DIR`, a list separated like `PATH`, the system certificate directories; each one unset keeps the system's. On Windows, either one replaces the system's certificates. |
| `NO_COLOR`                      | No color, though a terminal still gets bold text.                                                                      |
| `CLICOLOR_FORCE`, `FORCE_COLOR` | Color even when stdout is not a terminal or `TERM=dumb`.                                                               |
| `COLORTERM`, `TERM`             | The color depth. `COLORTERM=truecolor` or `24bit`, a `TERM` ending in `direct` or naming a 24-bit terminal such as `kitty`, `foot` or `wezterm`, and Windows Terminal give 24-bit color. `screen` and `tmux` give 256 colors even with `COLORTERM` set, any other `TERM` with `256color` gives 256 colors, and anything else gives 16. A Windows console without `TERM` gets 24-bit color. |

If the client cannot use a proxy variable, every request that would go through that proxy fails and names the
variable. HTTP/3 and WebTransport connect directly over UDP, without the proxy.
