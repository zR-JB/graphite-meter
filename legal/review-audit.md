# Independent reviewed-components audit

Audit date: 2026-08-14. This is an independent read-only audit of all 37
entries in `legal/reviewed-components.json`.

Method: every recorded legal-file SHA-256 was compared with the exact
installed module-cache or `client/node_modules/svelte` file; upstream tags or
pseudo-version commits and legal-file names were checked; and the selected
identifiers were cross-checked against SPDX (`MIT`, `ISC`, `BSD-3-Clause`, and
`Apache-2.0`). Apache `NOTICE` and Go `PATENTS` files were checked where
present. `go-localereader` intentionally uses its upstream README because
that package places its MIT grant there.

Result: all 37 entries are **confirmed**. None is changed or unresolved, and
there are no recommended edits to `legal/reviewed-components.json`. The
recorded local fingerprints matched byte-for-byte.

## Itemized result

Each row gives the exact installed revision, upstream legal-file URL, and
recommended edit. `none` means the existing fail-closed record is correct.

- confirmed — `github.com/atotto/clipboard` — `v0.1.4` — BSD-3-Clause — [LICENSE at tag](https://github.com/atotto/clipboard/tree/v0.1.4/LICENSE) — none
- confirmed — `github.com/aymanbagabas/go-osc52/v2` — `v2.0.1` — MIT — [LICENSE at tag](https://github.com/aymanbagabas/go-osc52/tree/v2.0.1/LICENSE) — none
- confirmed — `github.com/charmbracelet/bubbles` — `v1.0.0` — MIT — [LICENSE at tag](https://github.com/charmbracelet/bubbles/tree/v1.0.0/LICENSE) — none
- confirmed — `github.com/charmbracelet/bubbletea` — `v1.3.10` — MIT — [LICENSE at tag](https://github.com/charmbracelet/bubbletea/tree/v1.3.10/LICENSE) — none
- confirmed — `github.com/charmbracelet/colorprofile` — `v0.4.1` — MIT — [LICENSE at tag](https://github.com/charmbracelet/colorprofile/tree/v0.4.1/LICENSE) — none
- confirmed — `github.com/charmbracelet/lipgloss` — `v1.1.0` — MIT — [LICENSE at tag](https://github.com/charmbracelet/lipgloss/tree/v1.1.0/LICENSE) — none
- confirmed — `github.com/charmbracelet/x/ansi` — `v0.11.6` — MIT — [LICENSE at tag](https://github.com/charmbracelet/x/tree/v0.11.6/LICENSE) — none
- confirmed — `github.com/charmbracelet/x/cellbuf` — `v0.0.15` — MIT — [LICENSE at tag](https://github.com/charmbracelet/x/tree/v0.0.15/LICENSE) — none
- confirmed — `github.com/charmbracelet/x/term` — `v0.2.2` — MIT — [LICENSE at tag](https://github.com/charmbracelet/x/tree/v0.2.2/LICENSE) — none
- confirmed — `github.com/clipperhouse/displaywidth` — `v0.9.0` — MIT — [LICENSE at tag](https://github.com/clipperhouse/displaywidth/tree/v0.9.0/LICENSE) — none
- confirmed — `github.com/clipperhouse/stringish` — `v0.1.1` — MIT — [LICENSE at tag](https://github.com/clipperhouse/stringish/tree/v0.1.1/LICENSE) — none
- confirmed — `github.com/clipperhouse/uax29/v2` — `v2.5.0` — MIT — [LICENSE at tag](https://github.com/clipperhouse/uax29/tree/v2.5.0/LICENSE) — none
- confirmed — `github.com/coder/websocket` — `v1.8.15` — ISC — [LICENSE.txt at tag](https://github.com/coder/websocket/tree/v1.8.15/LICENSE.txt) — none
- confirmed — `github.com/coreos/go-oidc/v3` — `v3.20.0` — Apache-2.0 — [LICENSE](https://github.com/coreos/go-oidc/tree/v3.20.0/LICENSE) and [NOTICE](https://github.com/coreos/go-oidc/tree/v3.20.0/NOTICE) — none
- confirmed — `github.com/dunglas/httpsfv` — `v1.1.0` — BSD-3-Clause — [LICENSE at tag](https://github.com/dunglas/httpsfv/tree/v1.1.0/LICENSE) — none
- confirmed — `github.com/erikgeiser/coninput` — `1c3628e74d0f` — MIT — [LICENSE at commit](https://github.com/erikgeiser/coninput/tree/1c3628e74d0f/LICENSE) — none
- confirmed — `github.com/go-jose/go-jose/v4` — `v4.1.4` — Apache-2.0 — [LICENSE at tag](https://github.com/go-jose/go-jose/tree/v4.1.4/LICENSE) — none
- confirmed — `github.com/lucasb-eyer/go-colorful` — `v1.3.0` — MIT — [LICENSE at tag](https://github.com/lucasb-eyer/go-colorful/tree/v1.3.0/LICENSE) — none
- confirmed — `github.com/mattn/go-isatty` — `v0.0.20` — MIT — [LICENSE at tag](https://github.com/mattn/go-isatty/tree/v0.0.20/LICENSE) — none
- confirmed — `github.com/mattn/go-localereader` — `v0.0.1` — MIT — [README at tag](https://github.com/mattn/go-localereader/tree/v0.0.1/README.md) — none
- confirmed — `github.com/mattn/go-runewidth` — `v0.0.19` — MIT — [LICENSE at tag](https://github.com/mattn/go-runewidth/tree/v0.0.19/LICENSE) — none
- confirmed — `github.com/muesli/ansi` — `276c6243b2f6` — MIT — [LICENSE at commit](https://github.com/muesli/ansi/tree/276c6243b2f6/LICENSE) — none
- confirmed — `github.com/muesli/cancelreader` — `v0.2.2` — MIT — [LICENSE at tag](https://github.com/muesli/cancelreader/tree/v0.2.2/LICENSE) — none
- confirmed — `github.com/muesli/termenv` — `v0.16.0` — MIT — [LICENSE at tag](https://github.com/muesli/termenv/tree/v0.16.0/LICENSE) — none
- confirmed — `github.com/quic-go/qpack` — `v0.6.0` — MIT — [LICENSE.md at tag](https://github.com/quic-go/qpack/tree/v0.6.0/LICENSE.md) — none
- confirmed — `github.com/quic-go/quic-go` — `v0.61.0` — MIT — [LICENSE at tag](https://github.com/quic-go/quic-go/tree/v0.61.0/LICENSE) — none
- confirmed — `github.com/quic-go/webtransport-go` — `v0.12.0` — MIT — [LICENSE at tag](https://github.com/quic-go/webtransport-go/tree/v0.12.0/LICENSE) — none
- confirmed — `github.com/rivo/uniseg` — `v0.4.7` — MIT — [LICENSE.txt at tag](https://github.com/rivo/uniseg/tree/v0.4.7/LICENSE.txt) — none
- confirmed — `github.com/xo/terminfo` — `abceb7e1c41e` — MIT — [LICENSE at commit](https://github.com/xo/terminfo/tree/abceb7e1c41e/LICENSE) — none
- confirmed — `golang.org/x/crypto` — `v0.54.0` — BSD-3-Clause — [LICENSE](https://go.googlesource.com/crypto/+/refs/tags/v0.54.0/LICENSE) and [PATENTS](https://go.googlesource.com/crypto/+/refs/tags/v0.54.0/PATENTS) — none
- confirmed — `golang.org/x/net` — `v0.56.0` — BSD-3-Clause — [LICENSE](https://go.googlesource.com/net/+/refs/tags/v0.56.0/LICENSE) and [PATENTS](https://go.googlesource.com/net/+/refs/tags/v0.56.0/PATENTS) — none
- confirmed — `golang.org/x/oauth2` — `v0.36.0` — BSD-3-Clause — [LICENSE](https://go.googlesource.com/oauth2/+/refs/tags/v0.36.0/LICENSE) — none
- confirmed — `golang.org/x/sys` — `v0.47.0` — BSD-3-Clause — [LICENSE](https://go.googlesource.com/sys/+/refs/tags/v0.47.0/LICENSE) and [PATENTS](https://go.googlesource.com/sys/+/refs/tags/v0.47.0/PATENTS) — none
- confirmed — `golang.org/x/term` — `v0.45.0` — BSD-3-Clause — [LICENSE](https://go.googlesource.com/term/+/refs/tags/v0.45.0/LICENSE) and [PATENTS](https://go.googlesource.com/term/+/refs/tags/v0.45.0/PATENTS) — none
- confirmed — `golang.org/x/text` — `v0.40.0` — BSD-3-Clause — [LICENSE](https://go.googlesource.com/text/+/refs/tags/v0.40.0/LICENSE) and [PATENTS](https://go.googlesource.com/text/+/refs/tags/v0.40.0/PATENTS) — none
- confirmed — Go standard library — `go1.26.6-X:nodwarf5` — BSD-3-Clause — [Go LICENSE](https://go.dev/LICENSE) and [Go PATENTS](https://go.dev/PATENTS) — none
- confirmed — `svelte` — `5.56.8` — MIT — [LICENSE.md at tag](https://github.com/sveltejs/svelte/tree/svelte%405.56.8/LICENSE.md) — none

This confirms the current state only. A future version, upstream legal-file
change, declared-license change, or local fingerprint mismatch must remain
unapproved until a new audit updates the record and generated outputs.
