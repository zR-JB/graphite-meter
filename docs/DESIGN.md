# Design system

The browser client's look is owned by the tokens and primitives in `client/src/app.css`. This page states the rules
they encode. A component uses tokens and primitives; it never restates a colour, radius, shadow or type recipe.

[Project overview](../README.md) · [Development](DEVELOPMENT.md) · [Measurement definitions](MEASUREMENTS.md)

## Concept: graphite, lit by what it measures

The instrument is graphite: ink controls on a matte, grained page. The only colour is the measurement's own. Each
stage has one hue, and that hue lights the stage's area while it runs and marks it once measured.

- **Two layers.** The instrument sits on the page: the dial, the latency card and the stage cards have no box and no
  shadow; a rule and a soft wash in the stage's hue mark each area. What floats over the instrument (side sheets,
  dialogs, popovers, menus, tooltips) is the one raised layer, frosted, with the only shadow.
- **Colour is data.** Stage hues name stages, status tones name states, and everything a person operates is ink:
  the run button, selections, checks, switches, focus.
- **Type** is IBM Plex Sans throughout, with light numerals for measured values and tabular figures everywhere.
  IBM Plex Mono is reserved for key caps and unit captions.
- **Motion** follows the measurement: the room takes a little of the running stage's light, the running card's
  graph grows on the frame clock, sheets glide with the column they open, and changed times roll.
- **Density** is that of a working tool: every value is on screen, nothing is cut down to a hero number.

Chosen from four studies: the lab's surfaces, motion and settings sheet (Recorder, Chronograph), Editorial's stage
hues, shallowness and the idle-to-loaded latency figure, and Spatial's stage cards and box-plot latency card, on
Graphite Meter's own layout. Not chosen: a 360° dial (hard to read), a hero-number screen (undersells the tool), a
poster grid without hover (loses detail), glass, glow and 3D light (effects over data).

## Principles

1. **Values, not grades.** Show measured values with units. Never rate them, and never colour a value by how good
   it is. Interpretation belongs to the user.
2. **Calm is not grey.** Calm means no clutter; stage hues stay on every stage's cards, graphs, lanes, chips and
   History columns.
3. **Precise.** Hairlines, a 4 px grid, tight concentric radii, tabular figures and aligned columns. Every number can
   be explained on hover or focus: a title line, then short lines.
4. **One owner.** A second component that needs a recipe means the recipe belongs in `app.css`.

## Banned

- Boxes around everything: grey cards with drop shadows, wells inside cards, a plate for each value. The page holds
  the instrument; only floating layers are surfaces.
- Middle-dot metadata strings ("373.8 MB transferred · 1034 Mbit/s peak"). Lay facts out as label/value pairs.
- Tracked all-caps labels ("LOADED DOWN", "START TEST"). Labels and buttons are sentence case.
- Glass tiles, glow, animated backgrounds, 3D, and a fade-and-slide-up on every block.
- Animating a number's weight or width. A value may roll in once when it changes; live values update in place.
- Judgement colours or grades, dotted underlines outside curated jargon, help cursors, focus rings after clicks.
- A hue on a control. Colour on screen always names a stage or a state.
- Black shadows, mixed radius systems, pill buttons, and labels right-aligned against right-aligned controls.

## Colour

All colours are OKLCH `light-dark()` pairs, so a theme switch changes only `color-scheme`. Neutrals use hue 258.

| Role     | Tokens                                                                           | Rule                                                                                                          |
| -------- | -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| Page     | `--canvas`, `--canvas-deep`, `--grain`                                           | The shell paints a deeper floor over `--canvas`, dithered by grain; the running stage's light falls on it.    |
| Floating | `--sheet`, `--surface-1`, `--surface-2`                                          | Sheets are `--sheet` (frosted); their grouped lists are `--surface-1` plates; controls use `--surface-2`.     |
| Washes   | `--track`, `--hover-wash`, `--selected-wash`                                     | Translucent ink, so they read on any layer.                                                                   |
| Text     | `--text`, `--text-muted`, `--text-soft`                                          | ≥ 4.5:1 on every layer in both themes. `--text-soft` is the floor for any text.                               |
| Edges    | `--border-subtle`, `--border`, `--border-strong`, `--field-edge`, `--check-edge` | Hairlines for structure; a field's identifying edge uses `--field-edge` (3:1).                                |
| Ink      | `--brand`, `--brand-strong`, `--brand-soft`                                      | Graphite: near-white in dark, near-black in light. The run button, checked marks, switches, selection, focus. |
| Stages   | `--phase-latency`, `--phase-download`, `--phase-upload`, `--phase-bidirectional` | Teal, blue, amber, magenta: four families a quarter turn apart.                                               |
| Status   | `--ok`, `--warn`, `--err`, each with `-soft`                                     | Only for states (failed, reachable, stale), never for how good a value is.                                    |

**Stage hues are chosen to stay apart.** Every pair differs by at least ΔE 19 (OKLab × 100) for normal vision and
at least 10 under simulated protanopia, deuteranopia and tritanopia, in both themes; latency and bidirectional also
differ in lightness, which colour-vision deficiency keeps. Each hue clears 3:1 on the page as a line; small text in a
hue uses `--tone-ink`.

A tone gets its variants from one hue. Set `data-tone` (or use `.badge`, `.notice`, `.status-dot`), and then use
`--tone` for the line, trace or dot, `--tone-wash` for fills, `--tone-line` for edges and `--tone-ink` for small text.

**Dark and OLED.** The page is near-black, never black (`--canvas` L 0.17, `--canvas-deep` L 0.15), above the levels
where OLED pixels switch off and smear. Sheets and plates lift in lightness steps a dim panel still separates.
**Smooth shading.** The grain is zero-mean: mid-grey noise blended with `overlay` dithers every gradient by about one
8-bit level without moving the page off its token, so a 6-bit panel shows no steps. Marks are flat in their hue: no
gloss, no knockout rings in the page colour, and 1 px lines sit on whole pixels.
**Light.** A cool grey page, white plates, ink controls. **Gamut.** Base values fit sRGB; `@media (color-gamut: p3)`
raises stage and status chroma only, so contrast holds on both. **Contrast modes.** `prefers-contrast: more`
strengthens subtle edges and `--text-soft`; it and `prefers-reduced-transparency` make glass opaque.

The auth pages keep a pinned copy of the page, ink and text tokens (`go/internal/auth/assets/auth.css`), and
`client/index.html` repeats `--canvas` and `--text` for the first paint. The terminal client repeats the text, ink,
stage and status tokens in sRGB (`go/cmd/graphite-meter-client/theme.go`); a terminal draws everything as text, so
its light stage colours are each hue's `--tone-ink`. A few values sit a unit or three off their token so that
256-colour terminals still map ink and selection to grey and keep latency apart from ok.

## Type

| Role                                                    | Family    | Size                     | Weight                             |
| ------------------------------------------------------- | --------- | ------------------------ | ---------------------------------- |
| Measured value (dial, card, latency headline)           | Plex Sans | fluid, 30–76 px          | 300, tracking −0.025 em            |
| Sheet and dialog title (`--role-panel-title`)           | Plex Sans | `--type-lg`              | 600                                |
| Card and group title (`--role-title`)                   | Plex Sans | 13–14 px                 | 600                                |
| Row (`--role-row`): lists, settings, facts              | Plex Sans | `--type-md` 14 px / 1.35 | 450                                |
| Second line (`small` in a choice, a fact label, a note) | Plex Sans | `--type-sm` 12 px        | 450, `--text-soft` or `--tone-ink` |
| Control (`--role-control`)                              | Plex Sans | `--type-sm` 12 px        | 600                                |
| Key caps (`kbd`), unit captions in History heads        | Plex Mono | 10–11 px                 | 500                                |

10 px is the floor for any text. Figures are tabular everywhere. Weights: 300 for measured values, 450 text, 500
emphasised values, 600 titles and controls.

## Space, grid and radii

- A 4 px grid: `--space-1` to `--space-6` = 4, 8, 12, 16, 24, 32 px.
- A hairline (`--hairline`) is one device pixel: 1 px, 0.5 px from 2x and a third of a pixel from 3x screens.
- Rows are `--row-h` 42 px, controls `--control-h` 32 px, checks `--check` 18 px; coarse pointers grow targets to
  `--hit` 44 px.
- Radii: `--r-well` 4 px (tags, check boxes, box plots), `--r-chrome` 8 px (controls, plates, the run button, chips),
  `--r-surface` 12 px (sheets, dialogs, popovers), `--r-full` for dots and switches.
- **Panels** are 420 px by default (360–720, resizable). They dock from 1200 px, two side by side from 1520 px, and
  below that the last one opened stays. Docked, a sheet floats 12 px inside its column; below 1200 px it is a
  flyout of the same width, and on a portrait phone a bottom sheet. A docked sheet's inner edge is a handle
  (`.resize-handle`): drag it or step it 16 px with the arrows (48 with Shift), Home and End reach its limits, Enter
  or a double-click resets it, and a 2 px ink line lights the edge on hover or focus.
- **One text edge per sheet.** Plates sit on `--panel-pad` (16 px); text sits `--row-inset` (12 px) inside a plate,
  and every free line starts on that same edge.

## Layers

| Layer    | What                                                                                | Material                                                                                                                                                                                   |
| -------- | ----------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Page     | The instrument: dial, latency card, run bar, stage cards; History's list and detail | Grain and the stage light on `--canvas`. Areas are marked by a 2 px rule and a wash in their hue, fading out downward: `--wash` 9 % once measured, 16 % while running, none while pending. |
| Floating | Side sheets, dialogs, popovers, menus, tooltips, readouts                           | `--sheet` or glass with blur, a `--border-subtle` hairline, `--elev-float` or `--elev-tooltip`. Grouped lists inside are `--surface-1` plates without shadow.                              |

Hairlines mark structure only: a plate's edge, row separators inside plates, a head once content scrolls under it,
the axis under a graph, the latency card's gridlines and the facts' top edge in a card. Spacing separates everything
else.

## Icons

Line drawings on a 24-unit grid with a 1.9 stroke and round caps and joins, in `currentColor`
(`presentation/icons.ts`), at `--icon` 16 px or `--icon-sm` 13 px. One glyph per concept: download, upload,
bidirectional and ping mark their stage in History's column heads. On the instrument a stage is named by a dot in its
hue, never by a boxed icon.

## Motion

- Tokens: `--dur-hover` 120 ms, `--dur-slide` 180 ms (popovers), `--dur-sheet` 420 ms (sheets and their column),
  `--dur-graph` 280 ms (washes, chips, scale changes), `--dur-pulse` 1.1 s (a live indicator only). Easing is
  `--ease-out` for anything the user triggered.
- Live values and the running graph's leading edge move on the single frame clock in
  `presentation/motion.svelte.ts`; a glide smooths only the rendering.
- The room's light cross-fades 1.1 s between stages: one layer per stage in the stage column, never a repaint of the
  page; History is read without it. A docked sheet slides from its edge while its column
  (`--dock-left`, `--dock-right`) grows, and back out when closed. A changed time rolls like a counter (`Roll`,
  320 ms): up as it grows, down as it shrinks.
- A radio's ring closes in and a check draws in (180 ms); a row that appears in a sheet unfolds from its own height.
- Reduced motion keeps colour and opacity changes; sheets, rolls and glides jump to their end state.

## Components

- **Instrument** (`GaugePanel`): the dial and the latency card share the top and take the height left over; the run
  bar and three stage cards keep theirs. On a portrait screen, where the dial is bound by its width, the two share the
  width evenly. Narrow, it stacks: dial, run bar, stage cards, latency. A tight screen scrolls rather than
  overlapping rows. On a phone the dial takes about a third of the screen, so the running stage's card, its value and
  its graph share the first screen with it. The footer under the dial holds the phase's note or a failure, and while
  no data arrives, for how long.
- **Server lens** (`ServerLens`, `ServerScope quiet`): with several servers, one quiet field over the instrument
  (All servers or one) drives the stage cards and which server's latency is shown once the run finishes. History's
  detail has its own.
- **Stage card** (`ResultSummary`): a rule and wash in the stage hue; the name and a status word when not complete;
  the value (bidirectional: ↓ and ↑ in their own hues); the wire rate or a failure's reason; the graph; then facts:
  Peak, Stability, No data (from 0.5 s), Down + up, Transferred, in columns of at least 84 px, so a phone's card
  holds three to a row. A saved result has no graph row. On a phone the cards stack, and a card that has not run, or
  is done while the run goes on, folds to its name and value; the running card and every card of a finished run are
  whole.
- **Stage graph** (`StageGraph`): the rate from zero to the shared ceiling (`store.scales.chartBytesPerSec`), a dashed
  second lane for bidirectional upload, and a 20 px latency track below: one dot per reply bucket, height being time
  over the idle median (dashed baseline). A mouse, a tap, a sideways drag or arrow keys show a readout at once: time
  into the stage, the rate, and the latency replies measured then. A vertical swipe scrolls past; a drag's readout
  leaves with the finger.
- **Latency card** (`LatencyProfileView`): as tall as its content and centred beside the dial, its wash reaching its
  bottom edge so the area it marks reads whole. The idle median as the headline with Jitter, Range, Stability and
  Timeouts; then a table, one 40 px row per population on one scale: name, median, jitter, timeouts (the share of
  resolved probes that got no reply, which is not packet loss), box plot (P10–P90 box, min–max whisker, median tick,
  latest reply as a dot while live) and the added latency in its hue. The ms axis sits under the last row and its
  ticks run up through the rows as gridlines behind the plots; the idle median is one line from its tick through the
  loaded rows, and each loaded row's added-latency span starts from it. Narrow cards put the idle facts above and
  drop jitter, never timeouts; a phone gives each population its figures, then its plot.
- **Run bar**: the stage chips and the run button on one line; on a phone the chips share one row in equal columns
  and the run button spans the row under them at 44 px. A chip whose stage can still change is a switch drawn as an
  ink control (`.btn`): a plate and a filled bead when on, its edge alone and a ring bead when off; hover strengthens
  the edge and adds a wash, a press deepens the wash. A stage the run has reached locks its chip, which drops the
  plate and shows progress instead: a line and a wash while its stage runs, and a check once complete (on a phone
  the card says so). The run button is the one ink button, sentence case, with the estimate as a quiet suffix; Stop
  steps back to an outline.
- **Sheet** (`SidePanel`, `.sheet`): the title, quiet head actions, grouped plates. **Choice list** (`.choices`): rows
  with a name and a second line saying what the choice does (`PATH_NOTE`) or why it is unavailable; the ring or check
  alone marks the choice. Unavailable choices fold into one row.
- **Duration** (`DurationStrip`): presets over a bar of the enabled stages, each segment as wide as its time but never
  narrower than its words, with the time and name under it; Custom adds a − time + stepper (`TimeStepper`) per stage
  and for warmup. Steps grow with the time (0.5 s, 1 s, 10 s, 1 min, 5 min) and land on their grid; a click edits the
  time as text (`90`, `2h`, `1 h 30 min`, `1:30:00`), which rounds to the time shown, and Escape drops the edit. The
  field takes the keyboard like a spin button; − and + serve pointers and stay put at a limit. The servers' stage
  limit bounds every time, and its notice names each stage over it.
- **Switch**: a plate row with the link row's wash and ring; off is an empty track with the check box's edge
  (`--check-edge`), on an ink track with an inverse knob. **Check**: 18 px, ink when checked.
- **History**: rows show the time with the server and recency, then per column a value over a note: added latency
  under each rate in its hue, jitter under idle, the stage under loaded. The detail repeats the stage cards and the
  latency card, then each server's facts. From 821 px it sits beside the list, and the hairline between them is a
  handle like a docked sheet's edge: the list keeps its share of the width (40 % by default), never under 360 px,
  and the detail never under 460 px.
- **Facts** (`dl.kv`): label/value pairs; a qualifier that belongs to a value is an `.aside`, never joined with a dot.
- **Tooltip and readout** share one glass shell (`.inspect-card`). A tip opens after a short pause on its word
  (0.4 s, jargon 0.2 s, at once when moving from one tip to the next), on keyboard focus, on a click or tap on jargon
  or an explained fact, or on a long press on a control; never after a click on a control. It stays while the pointer
  is on its word, closes a moment after it leaves, and one tip shows at a time. A scroll or a tap elsewhere closes a
  pointer's tip; a tap on the tip closes it without reaching what lies beneath.

| Primitive                | Height         | Radius             | Type            | States                                               |
| ------------------------ | -------------- | ------------------ | --------------- | ---------------------------------------------------- |
| `.kv` row                | 42             | plate 8            | row             | separators `--border-subtle`                         |
| Choice row               | 42 (two lines) | 5, concentric      | row + `small`   | hover `--hover-wash`; chosen by its mark             |
| `.btn`                   | 32             | 8                  | control         | quiet: no ring, hover wash; disabled 0.5             |
| Run button               | 40 (44 phone)  | 8                  | 14 px 600       | ink fill, `--text-inverse`; running: outline         |
| Stage chip               | 36             | 8                  | 13 px, name 600 | on: plate, filled bead; off: edge; running: hue wash |
| `.segmented`             | 32             | 8 track, 6 segment | control         | selected `--selected-wash`                           |
| Switch                   | 22 × 38        | full               | row label       | off: check edge; on: ink track                       |
| Check, radio             | 18             | 4, full            | —               | ink fill or ring                                     |
| `.inspect-card`, tooltip | content        | 8                  | 12 px / 1.4     | glass, `--elev-tooltip`                              |
| Sheet, dialog            | content        | 12                 | panel title     | `--sheet`, frosted, `--elev-float`                   |

## Do and don't

| Do                                                                                                                  | Don't                                          |
| ------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------- |
| Colour the download graph, lane, chip bead and History note with `--phase-download`.                                | Colour a fast result green, or a control blue. |
| Put facts in an aligned `dl.kv`.                                                                                    | Join facts with " · " in grey prose.           |
| Let a stage's rule and wash mark its area on the page.                                                              | Box a card with a border and a shadow.         |
| Add a recipe to `app.css` when a second component needs it.                                                         | Restyle `.btn` inside a component.             |
| Review idle, live, complete, partial and stopped frames in both themes, at 1280, 1440, 1920 and phone width, at 2×. | Judge a change from metrics alone.             |
