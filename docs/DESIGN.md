# Design system

The browser client's look is owned by the tokens and primitives in `client/src/app.css`. This page states the rules
they encode. A component uses tokens and primitives; it never restates a colour, radius, shadow or type recipe.

[Project overview](../README.md) · [Development](DEVELOPMENT.md) · [Measurement definitions](MEASUREMENTS.md)

## Concept: a precision instrument with a living readout

Graphite Meter is a calibrated bench instrument. The housing is quiet, the readout is alive, and the only colour
is the measurement's own: each stage has one hue, and that hue is the same on every surface.

- **Material is machined.** Readouts sit in recessed wells (`.well`): the dial, the latency lanes, a strip. Results
  are raised plates with a lit top edge (`.surface`, the result cards, the stage chips, controls). Only floating
  chrome (sheets, popovers, menus, tooltips) casts the deeper shadow. Edges are hairlines; corners are 4, 6 and
  8 px, concentric. One level only: never a box inside a box inside a box.
- **Colour is data.** Stage hues name stages, status tones name states, and everything a person operates is ink:
  the run button, selections, checks, switches, focus.
- **Type is engineered.** IBM Plex Sans for words, records and readouts; IBM Plex Mono for instrument chrome whose
  digits change in place: units, axes, ticks, the status strip, key caps, and the few engraved captions
  (`.caps`): the stages legend and a chip's status tag. All figures are tabular.
- **Motion comes from the measurement.** Live values glide on one frame clock, the running stage's bar and strip
  grow on it, sheets glide with the column they open, and changed times roll. Nothing decorates, and nothing above
  the result cards moves from Start to the result.
- **Density is that of a working tool.** Every value is on screen; the boldness is spent in one place, the dial's
  readout; a result card is compact and its strip is a strip, not a chart.

The instrument's structure is the one the meter grew into before 0.9 (wells, chips, compact result cards, one
solid control) carried on the 0.9 tokens and type, with what 0.9 added: each stage's graph inside its own card, the
packed latency table with added latency and timeouts, one lens over several servers, and explained values. Not
chosen: an editorial page whose areas wash into the canvas (nothing to hold on to), a hero-number screen
(undersells the tool), a run sheet in place of chips (a control the size of a result), glass, grain, glow and 3D
light (effects over data, and compositor work on a slow GPU).

## Principles

1. **Values, not grades.** Show measured values with units. Never rate them, and never colour a value by how good
   it is. Interpretation belongs to the user.
2. **Calm is not grey.** Calm means no clutter; stage hues stay on every stage's chip, card, strip, bead, lane and
   History column.
3. **Precise.** Hairlines, a 4 px grid, concentric radii, tabular figures and aligned columns. Every number can be
   explained on hover or focus: a title line, then short lines.
4. **One owner.** A second component that needs a recipe means the recipe belongs in `app.css`.

## Banned

- Grey cards with soft grey shadows for everything, wells inside cards, a plate for each value. A plate is a
  result or a control; a well is a readout; spacing and hairlines do the rest.
- Middle-dot metadata strings ("373.8 MB transferred · 1034 Mbit/s peak"). Lay facts out as label/value pairs.
- Tracked capitals as labels. `.caps` is chrome: the stages legend, a status tag, an axis name, a unit caption.
- Glass, grain, glow, animated backgrounds, 3D, and a fade-and-slide-up on every block.
- Animating a number's weight or width. A value may roll in once when it changes; live values update in place.
- Judgement colours or grades, dotted underlines outside curated jargon, help cursors, focus rings after clicks.
- A hue on a control. Colour on screen always names a stage or a state.
- Black shadows, mixed radius systems, pill buttons, and labels right-aligned against right-aligned controls.

## Colour

All colours are OKLCH `light-dark()` pairs, so a theme switch changes only `color-scheme`. Neutrals use hue 258.

| Role     | Tokens                                                                           | Rule                                                                                                          |
| -------- | -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| Page     | `--canvas`                                                                       | Flat.                                                                                                         |
| Surfaces | `--surface-inset` < `--canvas` < `--surface-1` < `--surface-2`                   | Wells sit below the page and plates above it; `--surface-2` is for controls and floating chrome.              |
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

A tone gets its variants from one hue. Set `data-tone` (or use `.badge`, `.notice`, `.status-dot`, `.tone-icon`),
and then use `--tone` for the line, trace, bead or glyph, `--tone-wash` for fills, `--tone-line` for edges and
`--tone-ink` for small text. The ink is the hue mixed into `--text`, 80 % on a dark page and 70 % on a light one, so
small type keeps 4.5:1 on a card's wash.

**Dark and OLED.** The page is near-black, never black (`--canvas` L 0.17, `--surface-inset` L 0.15), above the
levels where OLED pixels switch off and smear. Plates lift in lightness steps a dim panel still separates.
**Flat shading.** Fills are flat tokens; the only gradients are a card's wash from its middle down and a strip's
area fade. Marks are flat in their hue, and 1 px lines sit on whole pixels. **Light.** A cool grey page, white
plates, ink controls. **Gamut.** Base values fit sRGB; `@media (color-gamut: p3)` raises stage and status chroma
only, so contrast holds on both. **Contrast modes.** `prefers-contrast: more` strengthens subtle edges and
`--text-soft`.

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
| Dial readout                                            | Plex Sans | fluid, 22–64 px          | 600, tracking −0.02 em             |
| Card and latency headline                               | Plex Sans | 24 px                    | 600, tracking −0.02 em             |
| Sheet and dialog title (`--role-panel-title`)           | Plex Sans | `--type-lg`              | 600                                |
| Card, lane and group title (`--role-title`)             | Plex Sans | 12–13 px                 | 600, a stage's in `--tone-ink`     |
| Row (`--role-row`): lists, settings, facts              | Plex Sans | `--type-md` 14 px / 1.35 | 450                                |
| Second line (`small` in a choice, a fact label, a note) | Plex Sans | `--type-sm` 12 px        | 450, `--text-soft` or `--tone-ink` |
| Control (`--role-control`)                              | Plex Sans | `--type-sm` 12 px        | 600                                |
| Units, ticks, axes, status strip, `kbd`                 | Plex Mono | 10–12 px                 | 500–600                            |
| Engraved caption (`.caps`)                              | Plex Mono | `--type-2xs` 10 px       | 700, capitals, `--track-caps`      |

10 px is the floor for any text. Figures are tabular everywhere. Weights: 450 text, 500 emphasised values, 600
titles, readouts and controls, 700 only for `.caps`.

## Space, grid and radii

- A 4 px grid: `--space-1` to `--space-6` = 4, 8, 12, 16, 24, 32 px. The instrument sits on a 12 px gutter with 16 px
  on a phone's sides; its parts sit 12 px apart and bind to each other's widths.
- A hairline (`--hairline`) is one device pixel: 1 px, 0.5 px from 2x and a third of a pixel from 3x screens.
- Rows are `--row-h` 42 px, controls `--control-h` 32 px, checks `--check` 18 px; coarse pointers grow targets to
  `--hit` 44 px.
- Radii rise with elevation and are concentric: `--r-well` 4 px (inner parts, tags, check boxes, box plots),
  `--r-chrome` 6 px (controls, plates, chips, result cards), `--r-surface` 8 px (wells, the run button, sheets,
  dialogs, popovers), `--r-full` for dots and switches.
- **Panels** are 420 px by default (360–720, resizable). They dock from 1200 px, two side by side from 1520 px, and
  below that the last one opened stays. Docked, a sheet floats 12 px inside its column; below 1200 px it is a
  flyout of the same width, and on a portrait phone a bottom sheet. A docked sheet's inner edge is a handle
  (`.resize-handle`): drag it or step it 16 px with the arrows (48 with Shift), Home and End reach its limits, Enter
  or a double-click resets it, and a 2 px ink line lights the edge on hover or focus.
- **One text edge per sheet.** Plates sit on `--panel-pad` (16 px); text sits `--row-inset` (12 px) inside a plate,
  and every free line starts on that same edge.

## Layers

| Layer    | What                                                                      | Material                                                                                                             |
| -------- | ------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| Page     | `--canvas`; History's list                                                | Flat.                                                                                                                |
| Well     | The dial, the latency lanes (`.well`), a lane's track, a chip's bar track | `--surface-inset` in a `--border` hairline with `--elev-inset`.                                                      |
| Plate    | Result cards, stage chips, `.btn`, `.kv` plates, docked sheets            | `--surface-1` or `--surface-2` in a hairline with a lit top edge (`--elev-tile`); a card is washed in its hue below. |
| Floating | Flyout sheets, dialogs, popovers, menus, tooltips, readouts               | `--sheet` or `--surface-2`, opaque, a hairline, `--elev-float` or `--elev-tooltip`.                                  |

Hairlines mark structure only: a well's and a plate's edge, row separators inside plates and History's list, a head
once content scrolls under it, the axis under a strip, the latency table's gridlines. Spacing separates everything
else. A sheet, and a dialog built as one (About & legal, opaque `--sheet-solid`), takes the `--border` edge; what
floats over the instrument (menus, popovers, toasts, tips) and a confirm dialog's opaque `--surface-1` take
`--border-strong`.

## Icons

Line drawings on a 24-unit grid with a 1.9 stroke and round caps and joins, in `currentColor`
(`presentation/icons.ts`), at `--icon` 16 px or `--icon-sm` 13 px, or 11 px inside a `.tone-icon`: a 20–22 px plate
washed in the stage's hue with a tone hairline, which marks a stage on its card, its lane, the dial's result and
History's column heads. One glyph per concept: download, upload, bidirectional and ping are the stage marks.

## Motion

- Tokens: `--dur-hover` 120 ms, `--dur-slide` 180 ms (popovers, dialogs, tips), `--dur-sheet` 420 ms (sheets and
  their column), `--dur-graph` 280 ms (bars, chips, scale changes), `--dur-pulse` 1.1 s (a live indicator only).
  Easing is `--ease-out` for anything the user triggered.
- Live values and the running strip's leading edge move on the single frame clock in
  `presentation/motion.svelte.ts`; a glide smooths only the rendering.
- A docked sheet hugs its column's inner edge, so the column's glide (`--dock-left`, `--dock-right`) is its slide:
  sheet and page move in one layout pass, in and out, and a dragged handle moves them without the glide. A changed
  time rolls like a counter (`Roll`, 320 ms): up as it grows, down as it shrinks.
- The dial's head is a bead in its hue, a little wider than the arc. While the latency stage runs, it beats on
  each idle reply, one `--dur-pulse` at a time: it swells a little and settles while a hairline ring in its hue
  spreads from it and fades, and the footer counts the replies so far; without replies it holds still.
- A radio's ring closes in and a check draws in (180 ms); a row that appears in a sheet unfolds from its own height.
- Reduced motion keeps colour and opacity changes; sheets, rolls and glides jump to their end state.

## Components

- **Instrument** (`GaugePanel`): two wells share the top as equal halves, the dial's and the latency lanes'; under
  them the run button, then the stage chips, then the result cards, each centred on the instrument and bound to
  the row above (the chips take 540 px, 700 with four stages; the cards 300 px each). The dial's well is as tall as
  the screen allows (`--gauge-well-height`): it yields to the rest of the instrument before the page would scroll,
  so a complete run fits 1024 × 768. On a phone the dial keeps about a third of the screen, the chips sit two to
  a row, the cards two to a row with the running card first, and the lanes come last. The dial is a 270° ring
  with an arc an eighth of its radius wide, short ticks and five labels; the headline result fills the arc over
  the others' and every stage's result is a bead at its arc's end, cut from its neighbours by a ring of the
  well; the stage's mark and name sit over the number. Every "—" that waits for a value is `--text-soft`; a
  measured value is full ink. The note under the dial holds the phase's note or a failure, and while no data or
  no reply arrives, for how long; on a landscape screen it hangs just under the ring. Every rate on the page reads
  in the dial's unit, zero included. Nothing above the cards moves from Start to the result.
- **Server lens** (`ServerLens`, `ServerScope quiet`): with several servers, one quiet field in the dial's corner
  (All servers or one), as wide as the choice it shows, drives the cards and which server's latency is shown once
  the run finishes. History's detail has its own.
- **Run button**: the one solid control, ink with a lit edge, 46 px and up to 320 px wide, sentence case, the
  estimate as a small mono chip at its side; Stop steps back to an outline with a square.
- **Stage chips** (`StageTrack`): under the "Test stages" caption, one 46 px plate per stage, each a switch:
  the stage's glyph and name, and along the chip's top a bar on a well track that fills in the stage's hue as it
  runs and stays full once measured (hatched for a partial stage, failed in `--err`); a check once complete, a mono
  tag while a stage is running, recovering or skipped. A chip in the run takes the ink edge and wash a selected
  tile takes; one off stays operable, so it reads soft, and only a locked chip dims. Nothing on the row moves
  between states.
- **Result card** (`ResultSummary`): one plate per stage, washed in its hue from the middle down: the stage's mark
  and name in its ink with a status dot and word at the line's end when not complete; the readout, one line tall
  (bidirectional: ↓ and ↑ in their own hues, on the same baseline); one quiet line (the wire rate with its overhead,
  the latency card's jitter, a failure's reason named by server when several ran, and after a stall No data from
  0.5 s); the strip; then the facts on one line, each a quiet label and its figure: Peak, Stability, Transferred
  (bidirectional: Stability, Down + up, Transferred), "—" until known, so the card keeps its height from Start to
  the result. The running card takes its hue as its edge. A card with no data yet keeps its strip's room but draws
  nothing in it; a saved result has no strip; a card that has not run, or is done while the run goes on, folds on
  a phone to its name and value. The card's hover lists every fact as pairs.
- **Strip** (`StageGraph`, `LatencyTrace`): 64 px in the card's wash. A transfer's strip is the rate from zero to
  the shared ceiling (`store.scales.chartBytesPerSec`), a dashed second lane for bidirectional upload, and a 20 px
  latency track below: one bar per reply bucket from the dashed idle median, its length being time over the
  median. The latency card's strip is that track alone at the strip's height: the idle replies over the stage,
  kept as the record (History has no series). A mouse, a tap, a sideways drag or arrow keys show a readout on a
  transfer's strip at once: time into the stage, the rate, and the latency replies measured then. A vertical swipe
  scrolls past; a drag's readout leaves with the finger.
- **Latency lanes** (`LatencyProfileView`): in the well beside the dial, the idle median as the headline with
  Jitter, Range, Stability and Timeouts beside it (above the table on a narrow well); then one 32 px ruled row per
  population, each led by its mark: name, median, jitter, timeouts (the share of resolved probes that got no reply,
  which is not packet loss), box plot (P10–P90 box over its min–max whisker, median tick, latest reply as a dot
  while live) and the added latency in its ink, from the medians until the run saves it, "—" without evidence.
  Figures are as wide as their longest value from Start, and a row whose probes timed out or were lost shows a note
  in its mark's place, so no column moves mid-run. The rows share one ms axis on the gauge's ladder over their
  P90s, so the boxes fill it; a whisker past it runs on to the edge, ends in an arrowhead and names its value. The
  axis sits under the last row and its ticks run up through the rows as gridlines behind the plots; the idle median
  is one line from its tick through the loaded rows, and each loaded row's added-latency span starts from it. A
  pointer anywhere on a row's plot reads the marker nearest it and follows the pointer from marker to marker. A
  narrow well drops jitter, never timeouts; a phone gives each population its figures, then its plot. A failed
  stage names its reason under the headline.
- **Sheet** (`SidePanel`, `.sheet`): the title, quiet head actions, grouped plates. **Choice list** (`.choices`): rows
  with a name and a second line saying what the choice does (`PATH_NOTE`) or why it is unavailable, cut with an
  ellipsis; the ring or check alone marks the choice. Unavailable choices fold into one row. While a run locks a list,
  every row but the chosen one dims.
- **Duration** (`DurationStrip`): presets over a bar of the enabled stages, each segment as wide as its time but never
  narrower than its words, with the time and name under it; Custom adds a − time + stepper (`TimeStepper`) per stage
  and for warmup. Steps grow with the time (0.5 s, 1 s, 10 s, 1 min, 5 min) and land on their grid; a click edits the
  time as text (`90`, `2h`, `1 h 30 min`, `1:30:00`), which rounds to the time shown, and Escape drops the edit. The
  field takes the keyboard like a spin button; − and + serve pointers and stay put at a limit. The servers' stage
  limit bounds every time, and its notice names each stage over it.
- **Switch**: a plate row with the link row's wash and ring; off is an empty track with the check box's edge
  (`--check-edge`), on an ink track with an inverse knob. **Check**: 18 px, ink when checked.
- **History**: the list is page, not plate: a day's rows sit under its heading between two rules, with hairlines
  between them, and the column heads sit over their units. A row shows the time over its server and recency (a long
  name is cut), then per column a value over a note on the same two baselines: added latency under each rate in its
  hue, jitter under idle, the stage under loaded; bars share a zero end per column and run on a track that shows the
  column's scale. The time takes the row's slack and each value column is as wide as its content, so the figures
  sit together at the right; the table stops at 1120 px, and the column heads' rule comes in as rows scroll under
  them. Columns never shrink below their content: once they no longer fit, each row folds, its time on one line and
  its values under it. Sort by lists only the shown columns. The detail repeats the result cards (three across or
  one to a row, never two and an orphan) and the latency lanes on the same 12 px text edge as its head, then each
  server's facts. From 821 px it sits beside the list, and the hairline between them is a handle like a docked
  sheet's edge: the list keeps its share of the width (40 % by default), never under 360 px, and the detail never
  under 460 px.
- **Facts** (`dl.kv`): label/value pairs; a qualifier that belongs to a value is an `.aside`, never joined with a dot.
- **Tooltip and readout** share one opaque shell (`.inspect-card`). A tip opens when the pointer rests on its word:
  within 8 px of where it settled for 0.4 s (jargon 0.25 s, 0.12 s while another tip shows or for 0.6 s after one
  closes), so a pointer passing by, sweeping across or dragging opens none. It also opens on keyboard focus, on a
  click or tap on jargon or an explained fact, or on a long press on a control; never after a click on a control. It
  stays while the pointer is on its word, closes a moment after it leaves, and one tip shows at a time. A scroll or a
  tap elsewhere closes a pointer's tip; a tap on the tip closes it without reaching what lies beneath. A tip the
  viewport would cut flips below its word, then aligns to the word's edge.

| Primitive                | Height         | Radius             | Type                | States                                                |
| ------------------------ | -------------- | ------------------ | ------------------- | ----------------------------------------------------- |
| `.well`                  | content        | 8                  | —                   | recessed, `--elev-inset`                              |
| `.surface`, result card  | content        | 8, card 6          | title 12 px 600 ink | lit edge; running: hue edge and halo; pending: subtle |
| `.tone-icon`             | 20–22          | 4                  | 11–13 px glyph      | tone wash and line                                    |
| `.kv` row                | 42             | plate 6            | row                 | separators `--border-subtle`                          |
| Choice row               | 42 (two lines) | 3, concentric      | row + `small`       | hover `--hover-wash`; chosen by its mark              |
| `.btn`                   | 32             | 6                  | control             | lit edge; quiet: hover, press and open washes         |
| Run button               | 46             | 8                  | 14 px 600           | ink fill, `--text-inverse`; running: outline          |
| Stage chip               | 46             | 6                  | 12 px 700, tag caps | on: ink edge and wash; bar in hue; done: check        |
| `.segmented`             | 32             | 6 track, 4 segment | control             | selected `--selected-wash`                            |
| Switch                   | 22 × 38        | full               | row label           | off: check edge; on: ink track                        |
| Check, radio             | 18             | 4, full            | —                   | ink fill or ring                                      |
| `.inspect-card`, tooltip | content        | 6                  | 12 px / 1.4         | `--surface-2`, `--elev-tooltip`                       |
| Sheet, dialog            | content        | 8                  | panel title         | `--sheet`; flyout `--elev-float`; docked flat         |

## Do and don't

| Do                                                                                                                  | Don't                                          |
| ------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------- |
| Colour the download strip, lane, chip bar, bead and History note with `--phase-download`.                           | Colour a fast result green, or a control blue. |
| Put facts in an aligned `dl.kv`.                                                                                    | Join facts with " · " in grey prose.           |
| Set a readout in a well and a result on a plate; keep a card's wash inside the card.                                | Wash the page around a card, or box a well.    |
| Add a recipe to `app.css` when a second component needs it.                                                         | Restyle `.btn` inside a component.             |
| Review idle, live, complete, partial and stopped frames in both themes, at 1024, 1440, 1920 and phone width, at 2×. | Judge a change from metrics alone.             |
