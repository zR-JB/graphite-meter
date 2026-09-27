import "../state/runes.testutil";
import { expect, test } from "bun:test";
import type { LiveSample } from "../runner/contract";

const { LiveReadout, STAGE_GLIDE_MS, STALL_FADE_MS } =
  await import("./liveReadout.svelte");

const live = (overrides: Partial<LiveSample> = {}): LiveSample => ({
  t: 0,
  phase: "download",
  continuityId: 1,
  bytes: 0,
  down: 1_000,
  up: null,
  bridgedUp: null,
  stalled: false,
  ...overrides,
});
const shown = (readout: InstanceType<typeof LiveReadout>, now: number) =>
  readout.phase ? readout.down.at(now) + readout.up.at(now) : null;

test("a stage change holds the last rate until its first evidence, then glides at one pace", () => {
  const readout = new LiveReadout();
  readout.update(live(), 1, 0);
  expect(shown(readout, 0)).toBe(1_000);
  readout.update(live({ down: 2_000 }), 1, 100);
  // The sample glides in over the sample interval instead of stepping.
  expect(shown(readout, 150)).toBe(1_500);
  for (const gap of [null, live({ phase: "upload", down: null })])
    readout.update(gap, 1, 300);
  expect(shown(readout, 300)).toBe(2_000);
  const upload = (up: number) =>
    live({ phase: "upload", down: null, up, bridgedUp: up });
  readout.update(upload(450), 1, 400);
  expect(readout.phase).toBe("upload");
  expect(shown(readout, 400)).toBe(2_000);
  readout.update(upload(650), 1, 400 + STAGE_GLIDE_MS / 2);
  expect(shown(readout, 400 + STAGE_GLIDE_MS / 2)).toBe(1_225);
  expect(shown(readout, 400 + STAGE_GLIDE_MS)).toBe(650);
  readout.update(null, 2, 900);
  expect(shown(readout, 900)).toBeNull();
});

test("a stall fades to zero once, over its own time", () => {
  const readout = new LiveReadout();
  readout.update(live(), 1, 0);
  const stalled = live({ down: 0, stalled: true });
  readout.update(stalled, 1, 100);
  readout.update(stalled, 1, 160);
  expect(shown(readout, 100 + STALL_FADE_MS / 2)).toBe(500);
  expect(shown(readout, 100 + STALL_FADE_MS)).toBe(0);
});
