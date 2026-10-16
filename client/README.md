# Browser client

Graphite Meter's browser client, in plain TypeScript: every task, both images and CI build and embed it. One owner
per piece of state, no runtime dependencies, and per-frame work kept to what moves.

## Layers

Each layer imports only from the layers above it in this table; the composition root (`src/app/app.ts`, mounted by
`src/main.tsx`) is the one place that wires across them.

| Folder | Owns |
| --- | --- |
| `auth` | Sources of the sign-in pages' assets; `bun run build:auth` writes `rust/server/src/auth/assets`, which the server embeds. |
| `src/lib` | Signals (push-pull, owners, batched effects), a JSX runtime that builds real DOM, keyed lists, the one frame clock, motion (glides, hand-offs, springs), decoders. |
| `src/protocol` | Server documents and their decoders, routes, wire and progress records, failure reasons, worker messages. |
| `src/engine` | Measurement without a DOM: `net` (catalog, selection, credentials, connection checks, readiness, sign-in), `lanes` + `workers` (fetch and WebTransport lanes, the probe channel, the upload receiver), `measure` (aggregation, latency, live rates, stability), `run` (schedule, clocks, the coordinator, results). |
| `src/app` | The application: preferences in the shared storage key, the run model views read, History storage and saving, the composition root (`app.ts`). |
| `src/view` | Pure presentation rules: every figure, unit, scale and word the views show. |
| `src/ui` | Views: `shell` (console frame, sheets, routes, keys, focus), `measure` (the instrument), `settings`, `details`, `history`, `legal`, `account`, and the `kit` they share. |
| `public` | Fonts, the favicon and the legal notices, copied into the build as they are. |

## Compatibility

Saved data and links from earlier releases keep working, so these are compatibility boundaries: `localStorage`
`graphite-meter:v1` and `graphite-meter:server-selection:v1`, the IndexedDB database `graphite-meter` at version 3
(upgrading version 2) with schema-5 records, the BroadcastChannel `graphite-meter-history`, and the hash routes (the Details panel
is `endpoint` in URLs). The page keeps the servers' shell contract: one `</head>`, one attribute-less inline script
and style (hashed into the content security policy), one `graphite-meter-trusted-types` meta naming the Trusted Types
policy `src/lib/trusted.ts` creates (the servers then require Trusted Types), `<div id="app">`, hashed assets under
`/assets/`; `build.ts` checks it.

## Build and checks

`bun build.ts` (`build:bundle`) bundles the workers, then the app with their hashed URLs, minifies the shell's inline
blocks, copies `public` and writes compressed siblings. `bun check` type-checks with the TypeScript 7 port Bun ships;
Biome formats. Unit tests (`bun test src`) cover pure logic and the engine against fake clocks and fakes. The browser
suite in `e2e/` runs real Chrome and Firefox against local servers; `scripts/e2e.ts`
starts the server fleet through `e2e/servers.ts`, and `e2e/webview.ts` and `e2e/browsers.ts` drive the browsers.
