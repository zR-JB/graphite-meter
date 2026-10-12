# Browser client v2

The Graphite Meter's browser client, rewritten in plain TypeScript beside the Svelte client in `client/` (v1), which
stays the default until v2 replaces it. Same look and behaviour; one owner per piece of state, no runtime
dependencies, and per-frame work kept to what moves.

`GM_CLIENT=v2` points the client, `dev`, `prod`, `e2e` and the container build here (the Rust image keeps v1; see
`docs/DEVELOPMENT.md`); `mise run client-v2-ci`, `client-v2-audit`, `e2e-v2` and `container-smoke-v2` are v2's own
gates.

## Layers

Each layer imports only from the layers above it in this table; the composition root (`src/app/app.ts`, mounted by
`src/main.tsx`) is the one place that wires across them.

| Folder | Owns |
| --- | --- |
| `auth` | Sources of the sign-in pages' assets; `bun run build:auth` writes `go/internal/auth/assets`, which both servers embed. |
| `src/lib` | Signals (push-pull, owners, batched effects), a JSX runtime that builds real DOM, keyed lists, the one frame clock, motion (glides, hand-offs, springs), decoders. |
| `src/protocol` | Server documents and their decoders, routes, wire and progress records, failure reasons, worker messages. |
| `src/engine` | Measurement without a DOM: `net` (catalogue, selection, credentials, connection checks, readiness, sign-in), `lanes` + `workers` (fetch and WebTransport lanes, the probe channel, the upload receiver), `measure` (aggregation, latency, live rates, stability), `run` (schedule, clocks, the coordinator, results). |
| `src/app` | The application: preferences in the shared storage key, the run model views read, History storage and saving, the composition root (`app.ts`). |
| `src/view` | Pure presentation rules: every figure, unit, scale and word the views show. |
| `src/ui` | Views: `shell` (console frame, sheets, routes, keys, focus), `measure` (the instrument), `settings`, `details`, `history`, `legal`, `account`, and the `kit` they share. |

## Compatibility

Both clients run on one origin, so v2 keeps v1's persisted formats and URLs exactly: `localStorage`
`graphite-meter:v1` and `graphite-meter:server-selection:v1`, the IndexedDB database `graphite-meter` v2 with
schema-5 records, the BroadcastChannel `graphite-meter-history`, and the hash routes (the Details panel is `endpoint`
in URLs). The page keeps the servers' shell contract: one `</head>`, one attribute-less inline script and style
(hashed into the content security policy), `<div id="app">`, hashed assets under `/assets/`; `build.ts` checks it.

## Build and checks

`bun build.ts` (`build:bundle`) bundles the workers, then the app with their hashed URLs, minifies the shell's inline
blocks, copies `client/public` and writes compressed siblings. `bun check` type-checks with the TypeScript 7 port Bun
ships; Biome formats. Unit tests (`bun test src`) cover pure logic and the engine against fake clocks and fakes;
the browser suite in `e2e/` runs real Chrome and Firefox against local Go servers through v1's harness, so both
clients are held to the same scenarios.
