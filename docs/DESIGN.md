# Design system

The browser client's look is owned by the tokens and primitives in `client/src/app.css`. This page states the rules
they encode. A component uses tokens and primitives; it never restates a colour, radius, shadow or type recipe.

[Project overview](../README.md) · [Development](DEVELOPMENT.md) · [Measurement definitions](MEASUREMENTS.md)

## Concept: a precision instrument with a living readout

Graphite Meter is a calibrated bench instrument. The housing is quiet, and the readout is alive.

- **Colour** means a channel. Each phase has one hue, and that hue is the same on every surface. Neutrals are graphite
  with a cool cast. Status colours appear only when there is a status to report.
- **Material** is machined. The canvas is matte with a fine grain, readouts sit in recessed wells, cards are raised
  plates with a lit top edge, and only floating chrome is glass. Edges are hairlines.
- **Type** is engineered: IBM Plex Sans for words and records, IBM Plex Mono for instrument chrome whose digits change
  in place. All figures are tabular.
- **Motion** comes from the measurement. Live values glide on one frame clock, and interface state changes ease out
  quickly. Nothing decorates.
- **Density** is that of a working tool: scan first, with the boldness spent in one place, the live readout.

This rules out: dashboard card kits, neon glow, aurora or mesh backgrounds, glass tiles, judgement colours and grades,
and ornament that carries no information.

Considered and not chosen: a *studio hi-fi* concept (warm anodised neutrals, an amber-lit readout, deep bezels) and a
*living light* concept (translucent glass everywhere over a lit, hue-washed canvas). The first reads as nostalgia and
turns muddy in light mode. The second puts blur behind data and lets the background compete with the chart.

## Principles

1. **Values, not grades.** Show measured values with units. Never rate them, and never colour a value by how good
   it is. Interpretation belongs to the user.
2. **Calm is not grey.** Calm means no clutter. Colour tied to meaning stays: phase hues on stage tiles, gauge arcs,
   lanes, chart traces, card icons and history columns.
3. **Precise.** Use hairlines, a 4 px grid, concentric radii, tabular figures and aligned columns. Every number can be
   explained on hover or focus: a title line, then short lines.
4. **One owner.** A second component that needs a recipe means the recipe belongs in `app.css`.

## Banned

- Identical rounded grey cards with soft grey shadows (the "SaaS-card kit"). Use a card only where elevation carries
  hierarchy; otherwise use spacing, alignment or a hairline.
- Middle-dot metadata strings such as "373.8 MB transferred · 1034 Mbit/s peak". Lay facts out as aligned
  label/value pairs.
- Tracked all-caps mono eyebrow labels on every tile, lane or section ("LOADED DOWN", "TEST STAGES"). Values use
  tabular figures; labels are sentence-case sans. `.caps` is reserved for axis names and unit chrome.
- A fade-and-slide-up on every block. Motion must explain a state change, with one orchestrated sequence per change.
- Flat grey fills with no atmosphere, and colour that carries no meaning.
- Judgement colours or grades, dotted underlines, help cursors, and focus rings after mouse clicks.
- Black shadows (shadows use `--shade`, a tint of the neutral hue), mixed radius systems, and pill buttons by default.
- Ease-in for user-triggered motion, and popovers that grow from scale 0 (start at about 0.95 from the trigger).
- Labels right-aligned against right-aligned controls. Labels sit on the leading edge.

## Colour

All colours are OKLCH `light-dark()` pairs, so a theme switch changes only `color-scheme`. Neutrals use hue 255.

| Role | Tokens | Rule |
| --- | --- | --- |
| Canvas | `--canvas`, `--bg` | `--bg` paints the app shell: grain, a soft top light, then `--canvas`. |
| Surfaces | `--surface-inset` < `--canvas` < `--surface-1` < `--surface-2` | Wells sit below the canvas and plates above it. `--surface-2` is for controls and glass. |
| Text | `--text`, `--text-muted`, `--text-soft` | ≥ 4.5:1 on every surface in both themes. `--text-soft` is the floor for any text. |
| Edges | `--border-subtle`, `--border`, `--border-strong`, `--field-edge` | Hairline decoration uses `--border*`. A control's identifying edge uses `--field-edge` (≥ 3:1). |
| Phases | `--phase-latency` (= `--signal`), `--phase-download` (= `--brand`), `--phase-upload`, `--phase-bidirectional` | Equal lightness within a theme, so no hue outranks another. Each works as text (≥ 4.5:1). |
| Status | `--ok`, `--warn`, `--err`, each with `-soft` | Only for states (failed, reachable, stale), never for how good a value is. |
| Brand | `--brand`, `--brand-strong`, `--brand-soft`, `--brand-line` | The download hue. Used for the run control, selection and focus. |

A tone gets its variants from one hue. Set `data-tone` (or use `.badge`, `.notice`, `.status-dot`), and then use
`--tone` for the line, trace or icon, `--tone-wash` for fills, `--tone-line` for edges and `--tone-ink` for small text on
the wash.

**Gamut.** The base values fit sRGB. `@media (color-gamut: p3)` raises the chroma of brand, signal, phases and
status by at most 1.25× and stays inside P3. Lightness does not change, so contrast holds on both gamuts. The chart
canvas uses a `display-p3` context, so its traces match the DOM.

**Contrast modes.** `prefers-contrast: more` strengthens subtle edges and raises `--text-soft` to `--text-muted`.
Both `prefers-contrast: more` and `prefers-reduced-transparency` make glass opaque.

## Type

| Role | Family | Size | Weight |
| --- | --- | --- | --- |
| Readout (gauge, card headline) | Plex Sans (`--font-display`) | fluid, per component | `--w-strong` 600, `--track-tight` |
| Panel and dialog title | Plex Sans | `--type-lg` (16–18 px) | 600 |
| Body, grouped lists, settings rows | Plex Sans | `--type-body` 13 px | `--w-normal` 450 values, 400 labels |
| Secondary text, hints | Plex Sans | `--type-sm` 12 px, `--type-xs` 11 px | 450 |
| Instrument chrome: axes, units, status bar, `kbd` | Plex Mono (`--font-mono`) | `--type-xs` to `--type-sm` | 500 to 600 |
| Axis names and unit captions (`.caps`) | Plex Mono | `--type-2xs` 10 px, `--track-caps` | `--w-heavy` |

- 10 px is the floor for any text. Only headings scale with the viewport.
- Figures are tabular everywhere (Plex draws tabular digits by default, and `body` sets `tabular-nums`).
- Plex distinguishes `I`, `l` and `1` in the sans, and `0` and `O` in the mono, without stylistic sets.
- Weights come from three tokens: `--w-normal` 450, `--w-strong` 600, `--w-heavy` 700. Hierarchy uses weight and
  tone, not extra sizes. Plex Sans is variable (100–700). Plex Mono ships Medium and SemiBold only, so mono text
  renders at 500 up to `--w-normal` and at 600 above it.

## Space, grid and radii

- Spacing uses a 4 px grid: `--space-1` to `--space-6` = 4, 8, 12, 16, 24, 32 px. Groups sit 24 px apart, and a title
  sits 6 px above its plate.
- `--control-h` is 32 px. Coarse pointers grow targets to `--hit` (44 px) in place.
- Radii rise with elevation and are concentric (inner = outer − padding): `--r-well` 6 px for inner parts and chips,
  `--r-chrome` 8 px for controls and grouped lists, `--r-surface` 12 px for cards, housings, panels and floats,
  `--r-full` for dots and switches. `--r-pill` (12 px) is only for the run control.

## Elevation and material

| Level | Primitive | Material |
| --- | --- | --- |
| Base | app shell (`--bg`) | Matte grain (a cached 160 px SVG tile at about 3.5 % alpha), with a soft light from the top. |
| Recessed | `.well`, `.kv`, `.surface-inset`, plot | `--surface-inset`, an inner shadow (`--elev-inset` or `--elev-recess`), and a hairline. |
| Raised | `.surface`, `.btn` | `--surface-1` or `--surface-2`, a lit top edge (`--edge-light`), and `--elev-raised`. |
| Floating | `.float`, `.popover`, `.tooltip`, `.inspect-card` | Glass: `--glass` with `--glass-blur`, a hairline `--border-strong`, and `--elev-float` or `--elev-tooltip`. |
| Dialog | `dialog.float` | Opaque `--surface-1` over `--scrim`. A dialog holds reading text. |

- **Hairlines.** `--hairline` is 1 px, and 0.5 px (one device pixel) from 2 dppx. Surface, header and decorative edges
  use it. Field, switch and checkbox edges stay 1 px for the 3:1 floor.
- **Glass** is only for small floating chrome. It is never used for tiles, panels or dialogs.
- **Shadows** are tinted `--shade`, denser in dark mode, and never black.

## Iconography

Icons are 24-unit line drawings with a 1.9 stroke and round caps and joins, in `currentColor`
(`presentation/icons.ts`). They render at `--icon` 16 px, or `--icon-sm` 13 px inside a 22 px `.tone-icon` chip, which
takes the phase wash and a tone hairline. Use one glyph per concept everywhere: download, upload, bidirectional and
ping are the phase marks.

## Motion

- Tokens: `--dur-hover` 120 ms (press, hover, tooltip), `--dur-slide` 180 ms (panels, popovers), `--dur-graph` 280 ms
  (chart and scale changes), and `--dur-pulse` 1.1 s (a live indicator only). Easing is `--ease-out` for anything the
  user triggered, and `--ease-snap` for switches.
- Live values glide on the single frame clock in `presentation/motion.svelte.ts`. A glide smooths only the
  rendering; the number shown is the measured one.
- What may move: a phase change, a value settling, a panel or popover opening, and a result being revealed. Popovers
  and dialogs enter with a 3 px offset and a fade; nothing grows from scale 0.
- Reduced motion keeps opacity and colour changes and drops translation, scale and glides; values jump to their final
  state.

## Components

- **Surface or plate** (`.surface`): a raised card for an object on the canvas, such as a result.
  **Housing** (`.well`): the gauge, lanes and chart. Use one level only; never box inside a box inside a box.
- **Grouped list** (`.group` > `h3` + `dl.kv`): a recessed well ruled by hairlines. Labels are `--text-soft` at 400
  and values are `--text` at 450, with one label column per list (`--kv-label`). The title sits on the row text edge.
- **Segmented control** (`.segmented`): a `--field-edge` well. The selected segment takes the brand wash and
  `--brand-strong` ink. Inner radius = `--r-chrome` − padding.
- **Field and select**: 36 px, `--r-chrome`, a 1 px `--field-edge`, and a brand halo on focus-visible.
- **Switch**: a 36 × 20 track with a `--field-edge` edge and a brand wash when on. The label sits on the leading edge
  and the switch on the trailing edge.
- **Button** (`.btn`): `--surface-2`, a hairline inset edge, and a lit top. Variants: `-accent`, `-quiet`, `-danger`,
  `-icon`. The run control is the one solid brand button.
- **Badge and status dot**: tone wash with tone ink. A dot shows only when its tone is not neutral.
- **Tooltip and hover card** (`.tooltip`, `.inspect-card`): glass. A multi-line tip's first line is its title; a line
  with a tab is a label/value pair (`.inspect-row`). A tip opens on hover intent, keyboard focus or long press, never
  after a click on a control.
- **Result chip**: one per stage, under its stage tile, at a fixed height in every state. The stage name is in phase
  ink, then the value and unit, then one quiet line: the wire rate, else added latency per loaded stage (the phase
  glyph, whole ms from 10 ms), jitter, each bidirectional lane, or bytes so far. Status is a dot and a word. The hover
  lists every fact as pairs. Latency lanes and chart stage labels use the same name treatment.
- **Tile or selectable row** (`.tile`): `--hover-wash` on hover. Selected tiles take `--brand-line`, `--brand-soft`
  and `--brand-strong`.

## Density

| | Desktop (fine pointer) | Touch (coarse pointer) |
| --- | --- | --- |
| Control height | 32 px | 44 px hit area, same visual size |
| List rows | 13 px text, 6 px vertical padding | Same, with 44 px rows where a row is a control |
| Panels | Docked from 1200 px | Sheets over the stage below 1200 px |
| Tooltips | Hover intent (0.5 s, 1 s for explainers) | Long press (0.5 s), closing after 4 s |

## Do and don't

| Do | Don't |
| --- | --- |
| Colour the download trace, lane, stage tile and card icon with `--phase-download`. | Colour a fast result green. |
| Put facts in an aligned `dl.kv`. | Join facts with " · " in grey prose. |
| Use `var(--hairline) solid var(--border)` for decorative edges. | Hard-code `1px solid rgb(...)`. |
| Add a recipe to `app.css` when a second component needs it. | Restyle `.btn` inside a component. |
| Review idle, live, complete, partial and stopped screenshots side by side with the last approved look, in both themes, at phone width and at 2×. | Judge a change from metrics alone. |
