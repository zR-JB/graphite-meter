# Design system

The browser client's look is owned by the tokens and primitives in `client/src/app.css`. This page states the rules
they encode. A component uses tokens and primitives; it never restates a colour, radius, shadow or type recipe.

[Project overview](../README.md) · [Development](DEVELOPMENT.md) · [Measurement definitions](MEASUREMENTS.md)

## Concept: a bench instrument in graphite

The instrument is graphite: ink controls on a matte page, in square-cornered frames. The only colour is the
measurement's own. Each stage has one hue, and that hue lights the stage's frame while it runs and marks it once
measured.

- **Frames.** Every area of the instrument is a frame (`.panel`): a hairline, one fill step above the page, a 4 px
  corner, an engraved caption. The dial, the latency table, the control strip and the stage cards each have one.
  A stage's frame takes its hue as its top rule and holds its light inside, like an indicator. What floats over
  the instrument (flyout sheets, dialogs, popovers, menus, tooltips) is the one raised layer, opaque, with the only
  shadow; a docked sheet is a frame beside the instrument, flat.
- **Colour is data.** Stage hues name stages, status tones name states, and everything a person operates is ink:
  the run button, selections, checks, switches, focus.
- **Two voices.** Prose, names and controls are IBM Plex Sans. Everything measured is IBM Plex Mono at one figure
  weight: readouts, facts, axes, counters, the status strip. A frame's caption and a table's head are small mono
  capitals, tracked. Figures are tabular everywhere, so nothing moves as a value changes.
- **Motion** follows the measurement: the running frame's wash comes up, its graph grows on the frame clock, sheets
  glide with the column they open, and changed times roll. The page itself never repaints for motion.
- **Density** is that of a working tool: every value is on screen, nothing is cut down to a hero number, and the
  whole instrument sits on one 12 px gutter.

The frames, captions and mono figures are what the meter had before 0.9 and lost to an editorial layout whose
areas bled into the page; the stage hues, the per-stage graphs with their latency track and the box-plot latency
table are what 0.9 added and keeps. Not chosen: a 360° dial (hard to read), a hero-number screen (undersells the
tool), glass, grain, glow and 3D light (effects over data, and compositor work on a slow GPU).

## Principles

1. **Values, not grades.** Show measured values with units. Never rate them, and never colour a value by how good
   it is. Interpretation belongs to the user.
2. **Calm is not grey.** Calm means no clutter; stage hues stay on every stage's cards, graphs, lanes, chips and
   History columns.
3. **Precise.** Hairlines, a 4 px grid, tight concentric radii, tabular figures and aligned columns. Every number can
   be explained on hover or focus: a title line, then short lines.
4. **One owner.** A second component that needs a recipe means the recipe belongs in `app.css`.

## Banned

- Frames inside frames: wells inside cards, a plate for each value, a shadow on anything that does not float. One
  frame per area; inside it, hairlines and spacing.
- Middle-dot metadata strings ("373.8 MB transferred · 1034 Mbit/s peak"). Lay facts out as label/value pairs.
- Capitals outside the kicker: a button, a choice, a notice or a sentence is sentence case. The kicker names
  frames, table heads and fact labels only.
- Glass, grain, glow, animated backgrounds, 3D, and a fade-and-slide-up on every block.
- Animating a number's weight or width. A value may roll in once when it changes; live values update in place.
- Judgement colours or grades, dotted underlines outside curated jargon, help cursors, focus rings after clicks.
- A hue on a control. Colour on screen always names a stage or a state.
- Black shadows, mixed radius systems, pill buttons, and labels right-aligned against right-aligned controls.

## Colour

All colours are OKLCH `light-dark()` pairs, so a theme switch changes only `color-scheme`. Neutrals use hue 258.

| Role     | Tokens                                                                           | Rule                                                                                                          |
| -------- | -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| Page     | `--canvas`, `--panel`                                                            | A flat page; every frame of the instrument and every docked sheet is `--panel`, one step above it.            |
| Floating | `--sheet`, `--surface-1`, `--surface-2`                                          | Flyout sheets are `--sheet` (opaque); grouped lists are `--surface-1` plates; controls use `--surface-2`.     |
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
The ink is the hue mixed into `--text`, 80 % on a dark page and 70 % on a light one, so small type keeps 4.5:1 on a
running card's wash.

**Dark and OLED.** The page is near-black, never black (`--canvas` L 0.17, `--panel` L 0.19), above the levels where
OLED pixels switch off and smear. Frames, sheets and plates lift in lightness steps a dim panel still separates.
**Flat shading.** Fills are flat tokens and the only gradient is a stage frame's wash, short enough that a 6-bit
panel shows no steps. Marks are flat in their hue: no gloss, no knockout rings in the page colour, and 1 px lines
sit on whole pixels.
**Light.** A cool grey page, near-white frames, white plates, ink controls. **Gamut.** Base values fit sRGB;
`@media (color-gamut: p3)` raises stage and status chroma only, so contrast holds on both. **Contrast modes.**
`prefers-contrast: more` strengthens subtle edges and `--text-soft`.

The auth pages keep a pinned copy of the page, ink and text tokens and of the Plex Sans and Plex Mono 600 faces
(`go/internal/auth/assets/auth.css`; those two font files are the only ones served before sign-in), notices are
app.css's `.notice`, and every page's card starts at one height so a notice grows it downward. `client/index.html`
repeats `--canvas` and `--text` for the first paint. The terminal client repeats the text, ink, stage and status
tokens in sRGB (`go/cmd/graphite-meter-client/theme.go`). Its light stage text colours are each hue mixed 80 % into
`--text`; graph strokes use the unmixed stage tokens. A few values sit a unit or three off their token so that
256-colour terminals still map ink and selection to grey and keep latency apart from ok.

## Type

| Role                                                        | Family    | Size                     | Weight                                  |
| ----------------------------------------------------------- | --------- | ------------------------ | --------------------------------------- |
| Readout (dial, card and latency headline)                   | Plex Mono | fluid, 22–56 px          | 500, tracking `--track-figure` −0.02 em |
| Figure (`--role-figure`, `-sm`): facts, table cells, status | Plex Mono | 14 px, 12 px             | 500; 600 for an added-latency figure    |
| Kicker (`--role-kicker`): frame captions, heads, labels     | Plex Mono | `--type-2xs` 10 px, 11   | 500, capitals, tracking `--track-wide`  |
| Sheet and dialog title (`--role-panel-title`)               | Plex Sans | `--type-lg`              | 600                                     |
| Group title (`--role-title`)                                | Plex Sans | 13–14 px                 | 600                                     |
| Row (`--role-row`): lists, settings, prose                  | Plex Sans | `--type-md` 14 px / 1.35 | 450                                     |
| Second line (`small` in a choice, a note)                   | Plex Sans | `--type-sm` 12 px        | 450, `--text-soft` or `--tone-ink`      |
| Control (`--role-control`)                                  | Plex Sans | `--type-sm` 12 px        | 600                                     |

10 px is the floor for any text. Figures are tabular everywhere. A frame's caption is the kicker at 11 px in
`--text`; a fact's label or a table head is the kicker at 10 px in `--text-soft`. Weights: 500 for every figure,
450 text, 600 titles, controls and the one emphasised figure.

## Space, grid and radii

- A 4 px grid: `--space-1` to `--space-6` = 4, 8, 12, 16, 24, 32 px.
- A hairline (`--hairline`) is one device pixel: 1 px, 0.5 px from 2x and a third of a pixel from 3x screens.
- Rows are `--row-h` 42 px, controls `--control-h` 32 px, checks `--check` 18 px; coarse pointers grow targets to
  `--hit` 44 px.
- Radii are the smallest that still read as a cut corner: `--r-well` 2 px (tags, check boxes, box plots),
  `--r-chrome` 3 px (controls, plates, the run button, chips), `--r-surface` 4 px (frames, sheets, dialogs,
  popovers), `--r-full` for status dots and switches. A stage's key is a square swatch, not a dot.
- **Panels** are 420 px by default (360–720, resizable). They dock from 1200 px, two side by side from 1520 px, and
  below that the last one opened stays. Docked, a sheet is a frame 12 px inside its column; below 1200 px it is a
  flyout of the same width, and on a portrait phone a bottom sheet. A docked sheet's inner edge is a handle
  (`.resize-handle`): drag it or step it 16 px with the arrows (48 with Shift), Home and End reach its limits, Enter
  or a double-click resets it, and a 2 px ink line lights the edge on hover or focus.
- **One text edge per sheet.** Plates sit on `--panel-pad` (16 px); text sits `--row-inset` (12 px) inside a plate,
  and every free line starts on that same edge.

## Layers

| Layer    | What                                                                                      | Material                                                                                                                                                                                                       |
| -------- | ----------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Page     | `--canvas`; History's list                                                                | Flat.                                                                                                                                                                                                          |
| Frame    | The dial, the latency table, the control strip, the stage cards, docked sheets (`.panel`) | `--panel` inside a `--border` hairline, no shadow. A stage's frame (`.stage-area`) has a 2 px top rule in its hue and a wash from it, inside the frame: `--wash` 7 % once measured, 14 % while running, none while pending. |
| Floating | Flyout sheets, dialogs, popovers, menus, tooltips, readouts                               | `--sheet` or `--surface-2`, opaque, a hairline, `--elev-float` or `--elev-tooltip`. Grouped lists inside are `--surface-1` plates without shadow.                                                              |

Hairlines mark structure only: a frame's and a plate's edge, row separators inside plates, the latency table and
History's list, a head once content scrolls under it, the axis under a graph, the latency table's gridlines and the
facts' top edge in a card. Spacing separates everything else. A frame, a sheet, and a dialog built as one (About &
legal, opaque `--sheet-solid`) take the `--border` edge; what floats over the instrument (menus, popovers, toasts,
tips) and a confirm dialog's opaque `--surface-1` take `--border-strong`.

## Icons

Line drawings on a 24-unit grid with a 1.9 stroke and round caps and joins, in `currentColor`
(`presentation/icons.ts`), at `--icon` 16 px or `--icon-sm` 13 px. One glyph per concept: download, upload,
bidirectional and ping mark their stage in History's column heads. On the instrument a stage is named by a dot in its
hue, never by a boxed icon.

## Motion

- Tokens: `--dur-hover` 120 ms, `--dur-slide` 180 ms (popovers, dialogs, tips), `--dur-sheet` 420 ms (sheets and
  their column), `--dur-graph` 280 ms (washes, chips, scale changes), `--dur-pulse` 1.1 s (a live indicator only).
  Easing is `--ease-out` for anything the user triggered.
- Live values and the running graph's leading edge move on the single frame clock in
  `presentation/motion.svelte.ts`; a glide smooths only the rendering.
- A docked sheet hugs its column's inner edge, so the column's glide (`--dock-left`, `--dock-right`) is its slide:
  sheet and page move in one layout pass, in and out, and a dragged handle moves them without the glide. A changed
  time rolls like a counter (`Roll`, 320 ms): up as it grows, down as it shrinks.
- The dial's head is a flat bead in its hue, a little wider than the arc. While the latency stage runs, it beats on
  each idle reply, one `--dur-pulse` at a time: it swells a little and settles while a hairline ring in its hue
  spreads from it and fades, and the footer counts the replies so far; without replies it holds still.
- A radio's ring closes in and a check draws in (180 ms); a row that appears in a sheet unfolds from its own height.
- Reduced motion keeps colour and opacity changes; sheets, rolls and glides jump to their end state.

## Components

- **Instrument** (`GaugePanel`): the dial's frame and the latency frame share the top row at one height and take the
  height left over; the control strip and three stage cards keep theirs. The dial's column is one stage card wide,
  so the latency frame starts on the second card's edge. On a portrait screen, where the dial is bound by its
  width, the two share the width evenly. Narrow, it stacks: dial, strip, stage cards, latency. A tight screen
  scrolls rather than overlapping rows. On a phone the dial takes about a third of the screen, so the running
  stage's card, its value and its graph share the first screen with it: the running card is the first under the
  strip, and while the latency stage runs its card is. The dial's frame is captioned Throughput, or Latency while
  the ring reads in milliseconds, and with several servers the server lens sits at its caption's end. The readout
  keeps one place: "—" stands where the value arrives, and the result lands on it, with the stage's key and name
  engraved over it.
  Every "—" that waits for a value, on the dial, the latency card and the stage cards, is `--text-soft`; a measured
  value is full ink. The footer under the dial holds the phase's note or a failure, and while no data or no reply
  arrives, for how long; on a landscape screen it hangs just under the ring, and the ring and the latency card share
  one axis. Every rate on the page reads in the dial's unit, zero included. Nothing above the run bar moves from Start
  to the result.
- **Server lens** (`ServerLens`, `ServerScope quiet`): with several servers, one quiet field over the instrument
  (All servers or one), as wide as the choice it shows, drives the stage cards and which server's latency is shown
  once the run finishes. History's detail has its own.
- **Stage card** (`ResultSummary`): a stage frame; its key and name as the caption and a status word when not
  complete (a stalled stage is Recovering); the readout, one line tall (bidirectional: ↓ and ↑ in their own hues,
  on the same baseline); the wire rate or a failure's reason, named by server when several ran, and at the line's
  end, after a stall, No data (from 0.5 s); the graph; then facts under a hairline, each a kicker over its figure:
  Peak, Stability, Down + up, Transferred, in columns of at least 84 px, so a phone's card holds three to a row. A
  card holds the same facts in every state (`cardFacts`): unseen
  until one is known, "—" while one is not, so it keeps its height from Start to the result; a taller neighbour
  leaves its rows in place. Under the dial on a landscape page, its rows sit 4 px apart rather than 6, so the page
  fits one screen down to 1024 × 768. A card with no data yet keeps its graph's room but draws nothing in it. A
  saved result has no graph row. On a phone the cards stack, and a card that has not run, or is done while the run
  goes on, folds to its name and value; the running card and every card of a finished run are whole.
- **Stage graph** (`StageGraph`): the rate from zero to the shared ceiling (`store.scales.chartBytesPerSec`), a dashed
  second lane for bidirectional upload, and a 20 px latency track below: one dot per reply bucket, height being time
  over the idle median (dashed baseline). A mouse, a tap, a sideways drag or arrow keys show a readout at once: time
  into the stage, the rate, and the latency replies measured then. A vertical swipe scrolls past; a drag's readout
  leaves with the finger.
- **Latency card** (`LatencyProfileView`): a stage frame as tall as the dial's, its content centred under its
  caption. The idle median as the headline; under it the idle replies over the stage, drawn like a stage
  graph's latency track (one dot per reply bucket over the dashed median), growing through the stage and kept as the
  record (History has no series); then Jitter, Range, Stability and Timeouts, each held from Start. A failed stage
  names its reason under the headline. Then a table, one 40 px row per population: name, median, jitter, timeouts
  (the share of resolved probes that got no reply, which is not packet loss), box plot (P10–P90 box over its min–max
  whisker, median tick, latest reply as a dot while live) and the added latency in its hue, from the medians until the
  run saves it, "—" without evidence. Figures are as wide as their longest value from Start, and a row whose probes
  timed out or were lost shows a note in its key's place, so no column moves mid-run. The rows are ruled and share
  one ms axis on the gauge's ladder over their P90s, so the boxes fill it; a whisker past it runs on to the edge,
  ends in an arrowhead and names its value. The axis sits under the last row and its ticks run up through the rows
  as gridlines behind the plots, so the table reads as a grid; the idle median is one line from its tick through
  the loaded rows, and each loaded row's added-latency span
  starts from it. A pointer anywhere on a row's plot reads the marker nearest it, as a stage graph's readout does, and
  follows the pointer from marker to marker. Narrow cards put the idle facts above and drop jitter, never timeouts; a phone gives each
  population its figures, then its plot.
- **Control strip**: a frame with the Stages caption, the stage chips, and the run button at the line's end; on a
  phone the chips share one row in equal columns and the run button spans the row under them at 44 px. A chip whose
  stage can still change is a switch drawn as an ink control (`.btn`): a plate and a filled square bead when on,
  its edge alone and an outlined bead when off; hover strengthens the
  edge and adds a wash, a press deepens the wash. A stage the run has reached locks its chip, which drops the plate
  (one locked only while the test starts keeps it) and shows progress instead: a line and a wash while its stage runs;
  once complete, a check in the stage's ink takes the bead's place. A chip has one glyph and no status word (its tip
  says why it is locked or skipped), so it keeps its width in every state: from Start to the result neither the chips
  nor the run button move. The run button is the one ink button, flat, sentence case, with the estimate as a quiet
  suffix; Stop steps back to an outline.
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
- **History**: the list is page, not plate: a day's rows sit under its kicker between two rules, with hairlines
  between them, and the column heads are kickers over their units. A row
  shows the time over its server and recency (a long name is cut), then per column a figure over a note on the same two
  baselines: added latency under each rate in its hue, jitter under idle, the stage under loaded; bars share a zero
  end per column and run on a track that shows the column's scale. The time takes the row's slack and each value
  column is as wide as its content, so the figures sit together at the right; the table stops at 1120 px, and the
  column heads' rule comes in as rows scroll under them. Columns never shrink below their content: once they no
  longer fit, each row folds, its time on one line and its values under it. Sort by lists only the shown columns. The detail repeats the stage cards (three across
  or one to a row, never two and an orphan) and the latency card on the same 12 px text edge as its head, then each
  server's facts. From 821 px it sits beside the list, and the hairline between them is a handle like a docked sheet's
  edge: the list keeps its share of the width (40 % by default), never under 360 px, and the detail never under
  460 px.
- **Facts** (`dl.kv`): label/value pairs; a qualifier that belongs to a value is an `.aside`, never joined with a dot.
- **Tooltip and readout** share one opaque shell (`.inspect-card`). A tip opens when the pointer rests on its word:
  within 8 px of where it settled for 0.4 s (jargon 0.25 s, 0.12 s while another tip shows or for 0.6 s after one
  closes), so a pointer passing by, sweeping across or dragging opens none. It also opens on keyboard focus, on a
  click or tap on jargon or an explained fact, or on a long press on a control; never after a click on a control. It
  stays while the pointer is on its word, closes a moment after it leaves, and one tip shows at a time. A scroll or a
  tap elsewhere closes a pointer's tip; a tap on the tip closes it without reaching what lies beneath. A tip the
  viewport would cut flips below its word, then aligns to the word's edge.

| Primitive                | Height         | Radius             | Type            | States                                                |
| ------------------------ | -------------- | ------------------ | --------------- | ----------------------------------------------------- |
| `.panel`, `.stage-area`  | content        | 4                  | kicker caption  | stage: top rule in hue; wash while running            |
| `.kv` row                | 42             | plate 3            | row             | separators `--border-subtle`                          |
| Choice row               | 42 (two lines) | 2                  | row + `small`   | hover `--hover-wash`; chosen by its mark              |
| `.btn`                   | 32             | 3                  | control         | quiet: hover, press and open washes; disabled 0.5     |
| Run button               | 36 (44 phone)  | 3                  | 13 px 600       | ink fill, `--text-inverse`; running: outline          |
| Stage chip               | 32             | 3                  | 12 px, name 600 | on: plate, bead; off: outline; running: wash; done: ✓ |
| `.segmented`             | 32             | 3 track, 1 segment | control         | selected `--selected-wash`                            |
| Switch                   | 22 × 38        | full               | row label       | off: check edge; on: ink track                        |
| Check, radio             | 18             | 2, full            | —               | ink fill or ring                                      |
| `.inspect-card`, tooltip | content        | 3                  | 12 px / 1.4     | `--surface-2`, `--elev-tooltip`                       |
| Sheet, dialog            | content        | 4                  | panel title     | `--sheet`; flyout `--elev-float`; docked flat         |

## Do and don't

| Do                                                                                                                  | Don't                                          |
| ------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------- |
| Colour the download graph, lane, chip bead and History note with `--phase-download`.                                | Colour a fast result green, or a control blue. |
| Put facts in an aligned `dl.kv`.                                                                                    | Join facts with " · " in grey prose.           |
| Frame an area once, caption it, and keep its light inside the frame.                                                | Shadow a card, or wash the page around it.     |
| Add a recipe to `app.css` when a second component needs it.                                                         | Restyle `.btn` inside a component.             |
| Review idle, live, complete, partial and stopped frames in both themes, at 1280, 1440, 1920 and phone width, at 2×. | Judge a change from metrics alone.             |
