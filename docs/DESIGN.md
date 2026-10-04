# Design system

The browser client's look is owned by the tokens and primitives in `client/src/app.css`. This page states the rules
they encode. A component uses tokens and primitives; it never restates a colour, radius, shadow or type recipe.

[Project overview](../README.md) · [Development](DEVELOPMENT.md) · [Measurement definitions](MEASUREMENTS.md)

## Concept: a bench console

Graphite Meter is a bench instrument on screen: a few flat panels on a canvas, a transport row of keys, a thin
ring in a quiet field. Separation comes from tone and space, the only colour is the measurement's own, and each
stage has one hue that is the same on every surface.

- **Material is flat.** A panel is one fill step above the canvas in a hairline (`.panel`): the dial, the latency
  lanes, a result card. A key is the same plate at a key's height. Only floating chrome (sheets, popovers, menus,
  tooltips) casts a shadow. Corners are 2, 3 and 4 px, concentric. One level only: never a box inside a box
  inside a box.
- **Colour is data.** Stage hues name stages, status tones name states, and everything a person operates is ink:
  the run key, selections, checks, switches, focus.
- **Type is engineered.** IBM Plex Sans for words and labels; IBM Plex Mono at one weight for every figure
  (`--role-readout`, `--role-figure-sm`): the readout, a card's or lane's figure, a fact, the status strip, units,
  ticks and key caps. All figures are tabular.
- **Motion comes from the measurement.** Live values glide on one frame clock, the running key and strip grow on
  it, sheets glide with the column they open, and changed times roll. Nothing decorates, and nothing above the
  result cards moves from Start to the result.
- **Density is that of a working tool.** Every value is on screen; the boldness is spent in one place, the dial's
  readout; a result card is compact and its strip is a strip, not a chart.

The console's structure is the 0.9 instrument's (each stage's graph in its own card, the packed latency table with
added latency and timeouts, one lens over several servers, explained values) on flat panels with a transport row.
Not chosen: an editorial page whose areas wash into the canvas (nothing to hold on to), recessed wells and lit edges
(a box in a box, and cold), a hero-number screen (undersells the tool), a run sheet in place of keys (a control the
size of a result), glass, grain, glow and 3D light (effects over data, and compositor work on a slow GPU).

## Principles

1. **Values, not grades.** Show measured values with units. Never rate them, and never colour a value by how good
   it is. Interpretation belongs to the user.
2. **Calm is not grey.** Calm means no clutter; stage hues stay on every stage's key, card rule, strip, bead, lane
   and History column.
3. **Precise.** Hairlines, a 4 px grid, concentric radii, tabular figures and aligned columns. Every number can be
   explained on hover or focus: a title line, then short lines.
4. **One owner.** A second component that needs a recipe means the recipe belongs in `app.css`.

## Banned

- Grey cards with soft grey shadows for everything, wells inside cards, a plate for each value. A panel is a
  readout, a result or a control; spacing and hairlines do the rest.
- Middle-dot metadata strings ("373.8 MB transferred · 1034 Mbit/s peak"). Lay facts out as label/value pairs.
- Tracked capitals as labels. `.caps` is chrome: an axis name or a unit caption.
- Glass, grain, glow, inner shadows, animated backgrounds, 3D, and a fade-and-slide-up on every block.
- Animating a number's weight or width. A value may roll in once when it changes; live values update in place.
- Judgement colours or grades, dotted underlines outside curated jargon, a pointer cursor on anything that is
  not pressed (an explained word takes the help cursor), focus rings after clicks.
- A hue on a control. Colour on screen always names a stage or a state.
- Black shadows, mixed radius systems, pill buttons, and labels right-aligned against right-aligned controls.

## Colour

All colours are OKLCH `light-dark()` pairs, so a theme switch changes only `color-scheme`. Neutrals use hue 258.

| Role     | Tokens                                                                           | Rule                                                                                                       |
| -------- | -------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| Page     | `--canvas`                                                                       | Flat.                                                                                                      |
| Surfaces | `--surface-inset` < `--canvas` < `--surface-1` < `--surface-2`                   | Panels and keys are `--surface-1`; `--surface-2` is for buttons and floating chrome.                       |
| Washes   | `--track`, `--hover-wash`, `--selected-wash`                                     | Translucent ink, so they read on any layer.                                                                |
| Text     | `--text`, `--text-muted`, `--text-soft`                                          | ≥ 4.5:1 on every layer in both themes. `--text-soft` is the floor for any text.                            |
| Edges    | `--border-subtle`, `--border`, `--border-strong`, `--field-edge`, `--check-edge` | Hairlines for structure; a field's identifying edge uses `--field-edge` (3:1).                             |
| Ink      | `--brand`, `--brand-strong`, `--brand-soft`                                      | Graphite: near-white in dark, near-black in light. The run key, checked marks, switches, selection, focus. |
| Stages   | `--phase-latency`, `--phase-download`, `--phase-upload`, `--phase-bidirectional` | Teal, blue, amber, magenta: four families a quarter turn apart.                                            |
| Status   | `--ok`, `--warn`, `--err`, each with `-soft`                                     | Only for states (failed, reachable, stale), never for how good a value is.                                 |

**Stage hues are chosen to stay apart.** Every pair differs by at least ΔE 19 (OKLab × 100) for normal vision and
at least 10 under simulated protanopia, deuteranopia and tritanopia, in both themes; latency and bidirectional also
differ in lightness, which colour-vision deficiency keeps. Each hue clears 3:1 on the page as a line; small text in a
hue uses `--tone-ink`.

A tone gets its variants from one hue. Set `data-tone` (or use `.badge`, `.notice`, `.status-dot`, `.tone-icon`),
and then use `--tone` for the line, trace, bead or glyph, `--tone-wash` for fills, `--tone-line` for edges and
`--tone-ink` for small text. The ink is the hue mixed into `--text`, 80 % on a dark page and 70 % on a light one, so
small type keeps 4.5:1 on a tinted field.

**Dark and OLED.** The page is near-black, never black (`--canvas` L 0.17, `--surface-1` L 0.215), above the levels
where OLED pixels switch off and smear. Panels lift in lightness steps a dim panel still separates. **Flat
shading.** Fills are flat tokens; the only gradient is a strip's area fade, and a strip's field is its hue mixed a
few percent into the panel. Marks are flat in their hue, and 1 px lines sit on whole pixels. **Light.** A cool grey
page, white panels, ink controls. **Gamut.** Base values fit sRGB; `@media (color-gamut: p3)` raises stage and
status chroma only, so contrast holds on both. **Contrast modes.** `prefers-contrast: more` strengthens subtle
edges and `--text-soft`.

The auth pages keep a pinned copy of the page, ink and text tokens and of the Plex Sans and Plex Mono 600 faces
(`go/internal/auth/assets/auth.css`; those two font files are the only ones served before sign-in), notices are
app.css's `.notice`, and every page's card starts at one height so a notice grows it downward. `client/index.html`
repeats `--canvas` and `--text` for the first paint. The terminal client repeats the text, ink, stage and status
tokens in sRGB (`go/cmd/graphite-meter-client/theme.go`). Its light stage text colours are each hue mixed 80 % into
`--text`; graph strokes use the unmixed stage tokens. A few values sit a unit or three off their token so that
256-colour terminals still map ink and selection to grey and keep latency apart from ok.

## Type

| Role                                                    | Family    | Size                     | Weight                             |
| ------------------------------------------------------- | --------- | ------------------------ | ---------------------------------- |
| Dial readout                                            | Plex Mono | fluid, 24–76 px          | 500                                |
| Card and latency headline (`--role-readout`)            | Plex Mono | 26–28 px                 | 500                                |
| Figure (`--role-figure-sm`)                             | Plex Mono | 12 px                    | 500                                |
| Sheet and dialog title (`--role-panel-title`)           | Plex Sans | `--type-lg`              | 600                                |
| Card, lane and group title (`--role-title`)             | Plex Sans | 13 px                    | 600                                |
| Key name                                                | Plex Sans | 13 px                    | 500                                |
| Row (`--role-row`): lists, settings                     | Plex Sans | `--type-md` 14 px / 1.35 | 450                                |
| Second line (`small` in a choice, a fact label, a note) | Plex Sans | 11–12 px                 | 450, `--text-soft` or `--tone-ink` |
| Control (`--role-control`)                              | Plex Sans | `--type-sm` 12 px        | 600                                |
| Status strip, hints                                     | Plex Sans | 12 px, figures mono      | 450                                |
| Engraved caption (`.caps`)                              | Plex Mono | `--type-2xs` 10 px       | 700, capitals, `--track-caps`      |

10 px is the floor for any text. Figures are tabular everywhere. Weights: 450 text, 500 figures and key names, 600
titles and controls, 700 only for `.caps`.

## Space, grid and radii

- A 4 px grid: `--space-1` to `--space-6` = 4, 8, 12, 16, 24, 32 px. The console sits on a 24 px gutter (16 under
  1024 px and on a phone); its panels sit 16 px apart and the keys 8 px, and everything binds to the console's
  width.
- A hairline (`--hairline`) is one device pixel: 1 px, 0.5 px from 2x and a third of a pixel from 3x screens.
- Rows are `--row-h` 42 px, controls `--control-h` 32 px (28 px on the bar), keys 46 px, checks `--check` 18 px;
  coarse pointers grow targets to `--hit` 44 px.
- Radii rise with elevation and are concentric: `--r-well` 2 px (inner parts, check boxes, box plots, a strip's
  field), `--r-chrome` 3 px (buttons, keys), `--r-surface` 4 px (panels, cards, sheets, dialogs, popovers),
  `--r-full` for dots and switches.
- **Panels** are 420 px by default (360–720, resizable). They dock from 1200 px, two side by side from 1520 px, and
  below that the last one opened stays. Docked, a sheet is its column: flush with the bars, cut from the stage
  by one hairline, no corner; below 1200 px it is a flyout of the same width, and on a portrait phone a bottom
  sheet. A docked sheet's inner edge is a handle
  (`.resize-handle`): drag it or step it 16 px with the arrows (48 with Shift), Home and End reach its limits, Enter
  or a double-click resets it, and a 2 px ink line lights the edge on hover or focus.
- **One text edge per sheet.** Plates sit on `--panel-pad` (16 px); text sits `--row-inset` (12 px) inside a plate,
  and every free line starts on that same edge.

## Layers

| Layer    | What                                                           | Material                                                                            |
| -------- | -------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| Page     | `--canvas`; History's list                                     | Flat.                                                                               |
| Panel    | The dial, the latency lanes, result cards, keys, docked sheets | `--surface-1` in a `--border` hairline (`.panel`); a card is ruled in its hue.      |
| Field    | A strip's field, a key's live tint, a lane's track             | The stage's hue mixed a few percent into the panel; no edge.                        |
| Bar      | The top bar and the status strip                               | `--surface-1` with one hairline against the page.                                   |
| Floating | Flyout sheets, dialogs, popovers, menus, tooltips, readouts    | `--sheet` or `--surface-2`, opaque, a hairline, `--elev-float` or `--elev-tooltip`. |

Hairlines mark structure only: a panel's and a key's edge, the bars' edges, row separators inside plates and
History's list, a head once content scrolls under it, the axis under a strip, the latency table's gridlines.
Spacing separates everything else. A sheet, and a dialog built as one (About & legal, opaque `--sheet-solid`),
takes the `--border` edge; what floats over the console (menus, popovers, toasts, tips) and a confirm dialog's
opaque `--surface-1` take `--border-strong`.

## Icons

Line drawings on a 24-unit grid with a 1.9 stroke and round caps and joins, in `currentColor`
(`presentation/icons.ts`), at `--icon` 16 px or `--icon-sm` 13 px, or 10 px inside a `.tone-icon`: an 18 px plate
washed in the stage's hue with a tone hairline, which marks a stage on its card, its lane, the dial's result and
History's column heads. One glyph per concept: download, upload, bidirectional and ping are the stage marks.

## Motion

- Tokens: `--dur-hover` 120 ms, `--dur-slide` 180 ms (popovers, dialogs, tips), `--dur-sheet` 420 ms (sheets and
  their column), `--dur-graph` 280 ms (bars, keys, scale changes), `--dur-pulse` 1.1 s (a live indicator only).
  Easing is `--ease-out` for anything the user triggered.
- Live values and the running strip's leading edge move on the single frame clock in
  `presentation/motion.svelte.ts`; a glide smooths only the rendering.
- A docked sheet hugs its column's inner edge, so the column's glide (`--dock-left`, `--dock-right`) is its slide:
  sheet and page move in one layout pass, in and out, and a dragged handle moves them without the glide. A changed
  time rolls like a counter (`Roll`, 320 ms): up as it grows, down as it shrinks.
- The dial's head is a bead in its hue, a little wider than the arc. While the latency stage runs, each idle
  reply rings out from it, one `--dur-pulse` at a time: a faint hairline ring in its hue widens to twice the
  head and fades, eased out, while the head itself holds still; without replies nothing rings.
- A radio's ring closes in and a check draws in (180 ms); a row that appears in a sheet unfolds from its own height.
- Reduced motion keeps colour and opacity changes; sheets, rolls and glides jump to their end state.

## Components

- **Console** (`GaugePanel`): the dial on the page, in no panel, 300 px wide and up to 560 with the console,
  beside the latency lanes with the controls under them, the run key over the stage chips, centred in the room
  the lanes leave; the dial is as tall as its ring wants or as the lanes and the controls together; under both
  one card per stage across the console. What is measured live lies on the page (dial, lanes, keys); what is
  kept is a card. Without the latency stage the dial stands centred and wider with the controls under it. A complete run fits 1024 × 768 without scrolling. On a phone the dial keeps about two fifths
  of the screen with the controls under it, up to three chips to a row (four as two and two, a narrow chip
  without its glyph), the cards one to a row in stage order, each whole from Start so nothing moves as the
  stages run, and the lanes come last.
  The dial is a 270° ring with an arc 0.13 of its radius wide, ticks and five labels; every result's
  arc lies on the ring, the longest underneath, so each shows from where the next shorter one ends, and ends in
  a bead in its hue; a bead moved inward off a close neighbour hangs on a stalk; the stage's mark and name sit
  over the number, which is Plex Mono 500 at 0.17 of the face's smaller side (24–76 px), every size of the
  face's type taken from its measured box, never from a container query, which a flex item answers late. The ring and its readout grow with the screen: on a landscape screen the
  console's rows are as tall as their content, the dial as tall as its ring wants (0.86 of its width) or as the
  lanes and the controls together, and a tall screen's spare height goes into even air above, between and below
  the sections, never under the cards alone; the lanes start a step (32 px) under the dial's top, nearer the
  ring's crown than its box. Every "—" that waits for a value is `--text-soft`; a measured value is full ink. The note under the
  dial holds the phase's note or a failure, and while no data or no reply arrives, for how long; on a landscape
  screen it hangs just under the ring. Every rate on the page reads in the dial's unit, zero included. Nothing
  above the cards moves from Start to the result.
- **Top bar and status strip**: 48 px on the canvas and 28 px in `--surface-1`, each with a hairline, their
  text on the console's gutter (`--gutter`, 24 px, 16 under 1024 px), so the brand, the panels' edges and the
  status word share one line. The bar carries the brand, whose hexagon is the latency hue as the favicon's
  is, a Settings key, the connection dot, and at the right History, Details and the theme; a key is a 32 px
  square glyph plate with a hairline. The brand's hover dims its word, no plate. The strip reads from the left
  the status word, the elapsed time, the bytes moved and the time left while a stage runs, each figure in a cell
  as wide as its longest value, and the build alone at the right.
- **Server lens** (`ServerLens`, `ServerScope quiet`): with several servers, one quiet field in the dial's corner
  (All servers or one), as wide as the choice it shows, drives the cards and which server's latency is shown once
  the run finishes. History's detail has its own.
- **Stage chips** (`StageTrack`): under the run key and the engraved caption "Test stages", one 172 × 46 px chip
  per stage in a row. A chip is a switch: along its top a 3 px bar in the stage's hue that fills as the stage runs
  and stays full once measured (hatched for a partial stage, failed in `--err`, a sweep while warming up); under
  it the stage's glyph and name and, at the end, its time: the stage's length while it waits ("4 s"), the time
  into it while it runs ("1.9 / 4 s", counting in place; the time alone on a phone), a check once complete, or
  its state as a small engraved tag (Skipped, Recovering, Partial, Failed). As many chips stand in a row as fit,
  140–172 px each. The running chip takes its hue as its
  edge, like the running card. One off stays operable, so it reads soft, and only a locked chip dims. The chip's
  hover names its result. Nothing on the row moves between states.
- **Run key** (`RunButton`): the one solid control, ink, over the stage chips at their row's width, 48 px (44 on
  a phone), its label and estimate centred; Stop steps back to a quiet plate with a square.
- **Result card** (`ResultSummary`): one panel per stage, ruled 2 px in its hue along the top: the stage's mark
  and name with a status word at the line's end when not complete; the readout, one line tall (bidirectional: ↓
  and ↑ as a pair on one baseline); one quiet line (the wire rate with its overhead, the latency card's jitter, a
  failure's reason named by server when several ran, and after a stall No data from 0.5 s); the strip on a field
  tinted in the hue; then the facts as ruled rows, a quiet label and its figure on one line: Peak, Stability,
  Transferred (bidirectional: Stability, Down + up, Transferred; latency: the idle stage's Stability, Range,
  Replies and Timeouts), "—" until known, so the card keeps its height from Start to
  the result; a card that has measured nothing yet shows them quietly. The running card's edge takes its hue. A card with no data yet keeps its strip's room
  but draws nothing in it; a saved result has no strip; a card that has not run, or is done while the run goes
  on, folds on a phone to its name and value. The card's hover lists every fact as pairs.
- **Strip** (`StageGraph`): 68 px on the card's field, up to 120 with the console's height. A transfer's strip is the rate from zero to
  the shared ceiling (`store.scales.chartBytesPerSec`), a dashed second lane for bidirectional upload, and a 20 px
  latency track below: one bar per 4 px column of the width, spanning its reply buckets' fastest to slowest
  reply, so a bucket's spread shows at a glance and every stage's track has bars of one pitch whatever its
  length, over the dashed idle median. The track's top is the ladder tier above 2.5× the p75 of the run's
  slowest replies, so the body of the replies keeps its shape; a reply past it is clamped at the edge and ends in
  an arrowhead, and the readout names a bucket's median and, when its replies spread, their range. The latency card's strip is the same track alone at the strip's height, with the same readout: the
  idle replies over the stage, kept as the record (History has no series). A mouse, a tap, a sideways drag or arrow keys show a readout on a
  transfer's strip at once: time into the stage, the rate, and the latency replies measured then. A vertical swipe
  scrolls past; a drag's readout leaves with the finger.
- **Latency lanes** (`LatencyProfileView`): the latency stage's area beside the dial (`.stage-area`: a 2 px rule
  in its hue along the top and a wash of it that fades out, no box), under the head (mark, Latency, "Idle and
  under load"): the idle median as the headline over "Idle median" with Jitter, Range, Stability and Timeouts as
  label-over-figure pairs under it (beside it on a narrow panel); then one 32 px ruled row per population, each
  led by its mark: name, median, jitter, timeouts (the share of resolved probes that got no reply, which is not
  packet loss), box plot (P10–P90 box over its min–max whisker, median tick, latest reply as a dot while live) and
  the added latency in its ink, from the medians until the run saves it, "—" without evidence. Figures are as
  wide as their longest value from Start, and a row whose probes timed out or were lost shows a note in its
  mark's place, so no column moves mid-run. The rows share one ms axis on the gauge's ladder over their P90s, so
  the boxes fill it; a whisker past it runs on to the edge, ends in an arrowhead and names its value. The axis
  sits under the last row and its ticks run up through the rows as gridlines behind the plots; the idle median
  is one line from its tick through the loaded rows, and each loaded row's added-latency span starts from it. A
  pointer anywhere on a row's plot reads the marker nearest it and follows the pointer from marker to marker; the
  reading (marker, value, meaning) stands in the row beside the marker, above the box's band, on the side with
  room for it, as the strips' readouts do. A narrow panel drops jitter, never timeouts; a phone gives each population its figures, then its plot. A failed
  stage names its reason under the headline.
- **Sheet** (`SidePanel`, `.sheet`): the title, quiet head actions, grouped plates. **Choice list** (`.choices`): rows
  with a name and a second line saying what the choice does (`PATH_NOTE`) or why it is unavailable, cut with an
  ellipsis; the ring or check alone marks the choice. Unavailable choices fold into one row. While a run locks a list,
  every row but the chosen one dims.
- **Duration** (`DurationStrip`): presets over a bar of the enabled stages, each segment as wide as its time but never
  narrower than its words, with the time and name under it; Custom adds a − time + stepper (`Stepper`) per stage
  and for warmup. Steps grow with the time (0.5 s, 1 s, 10 s, 1 min, 5 min) and land on their grid; a click edits the
  time as text (`90`, `2h`, `1 h 30 min`, `1:30:00`), which rounds to the time shown, and Escape drops the edit. The
  field takes the keyboard like a spin button; − and + serve pointers, repeat while held (after 0.4 s, every
  70 ms) and stay put at a limit. The stream limit is
  the same `Stepper` over a whole number. In a settings row a control stands at its label's end while the row
  holds both and against the right edge under it when it wraps; a cadence's segments then take the row's width.
  The servers' stage
  limit bounds every time, and its notice names each stage over it.
- **Select**: only for a list of servers (`ServerScope`): a native `select`, so a phone opens its own picker;
  where the browser allows it (`appearance: base-select`) the field and its list take the console's own field,
  plate and rows. A choice among a few words is a segmented control (units, presets, probe cadences).
- **Switch**: a plate row with the link row's wash and ring; off is an empty track with the check box's edge
  (`--check-edge`), on an ink track with an inverse knob. **Check**: 18 px, ink when checked.
- **History**: the list is page, not plate: a day's rows sit under its heading between two rules, with hairlines
  between them, and the column heads sit over their units. A row shows the time over its server and recency (a long
  name is cut), then per column a value over a note on the same two baselines: added latency under each rate in its
  hue, jitter under idle, the stage under loaded; bars share a zero end per column and run on a track that shows the
  column's scale. The time takes the row's slack and each value column is as wide as its content, so the figures
  sit together at the right; the value columns share the slack after the time, so the table fills the list at any
  width, and the column heads' rule comes in as rows scroll under
  them. Columns never shrink below their content: once they no longer fit, each row folds, its time on one line and
  its values under it. Sort by lists only the shown columns. A notice over the list counts the records it cannot
  read, with Remove them, which deletes only those, or Dismiss. The detail repeats the result cards (three across or
  one to a row, never two and an orphan) and the latency lanes on the same 12 px text edge as its head, then each
  server's facts. From 821 px it sits beside the list, and the hairline between them is a handle like a docked
  sheet's edge: the list keeps its share of the width (50 % by default), never under 360 px, and the detail never
  under 460 px.
- **Facts** (`dl.kv`): label/value pairs; a qualifier that belongs to a value is an `.aside`, never joined with a dot.
- **Tooltip and readout**: a readout is a light plate (`.inspect-card`, a hairline, 3 px corners); a tip is
  ink (`--brand`, inverse text, 3 px corners, no arrow), so it never reads as part of the instrument, and it
  fades in over 120 ms. A tip opens once the pointer has rested on its word for 0.3 s (jargon 0.2 s, 60 ms just
  after another closed); a hand moving faster than 0.2 px/ms starts the rest over, a reading hand's drift does
  not, so a pointer passing by or dragging opens none. A dotted underline marks jargon inside a line of text
  (a card's "wire", "no data"); a row's or a control's label carries its tip on the help cursor alone, so rows
  and sheets read clean. No tip restates what its control already shows: a Close key and a connection path's
  row carry none. It also opens on keyboard focus, on a
  click or tap on jargon or an explained fact, or on a long press on a control; never after a click on a control. It
  stays while the pointer is on its word, closes a moment after it leaves, and one tip shows at a time. A scroll or a
  tap elsewhere closes a pointer's tip; a tap on the tip closes it without reaching what lies beneath. A tip the
  viewport would cut flips below its word, then aligns to the word's edge.

| Primitive                | Height         | Radius             | Type              | States                                              |
| ------------------------ | -------------- | ------------------ | ----------------- | --------------------------------------------------- |
| `.panel`                 | content        | 4                  | —                 | flat, hairline                                      |
| Result card              | content        | 4                  | title 13 px 600   | hue rule; running: hue edge; pending: subtle        |
| `.tone-icon`             | 18             | 2                  | 10 px glyph       | tone wash and line                                  |
| `.kv` row                | 42             | plate 4            | row               | separators `--border-subtle`                        |
| Choice row               | 42 (two lines) | 2, concentric      | row + `small`     | hover `--hover-wash`; chosen by its mark            |
| `.btn`                   | 32 (bar 28)    | 3                  | control           | hairline; quiet: hover, press and open washes       |
| Run key                  | 48             | 3                  | 14 px 600         | ink fill, `--text-inverse`; running: quiet plate    |
| Stage chip               | 172 × 46       | 3                  | 12 px 700, figure | bar in hue; live: time and hue edge; done: check    |
| `.segmented`             | 32             | 3 track, 2 segment | control           | equal segments; the chosen one a plate with a shadow |
| Switch                   | 22 × 38        | full               | row label         | off: check edge; on: ink track                      |
| Check, radio             | 18             | 2, full            | —                 | ink fill or ring                                    |
| `.inspect-card`, tooltip | content        | 3                  | 12 px / 1.4       | `--surface-2`, `--elev-tooltip`                     |
| Sheet, dialog            | content        | 4                  | panel title       | `--sheet`; flyout `--elev-float`; docked flat       |

## Do and don't

| Do                                                                                                                  | Don't                                          |
| ------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------- |
| Colour the download strip, lane, key bar, bead, card rule and History note with `--phase-download`.                 | Colour a fast result green, or a control blue. |
| Put facts in an aligned `dl.kv`.                                                                                    | Join facts with " · " in grey prose.           |
| Set a readout, a result and a key on a flat panel; tint only a strip's field.                                       | Recess a well, wash a card, or box a panel.    |
| Add a recipe to `app.css` when a second component needs it.                                                         | Restyle `.btn` inside a component.             |
| Review idle, live, complete, partial and stopped frames in both themes, at 1024, 1440, 1920 and phone width, at 2×. | Judge a change from metrics alone.             |
