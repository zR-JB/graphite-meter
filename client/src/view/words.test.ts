import { expect, test } from "bun:test";
import { FAILURE_REASONS } from "../protocol/reasons";
import { DEFAULT_CONFIG } from "../app/preferences";
import { STATUS_WORD } from "../ui/history/model";
import { liveCard } from "../ui/measure/cards-model";
import {
  allServersLabel,
  chipTag,
  STAGE_STATE,
  listed,
  reasonLabel,
  serverLabel,
  stageLimitError,
  stageState,
  type StageFacts,
  type StageState,
} from "./words";

test("a known failure reads as its label, a renamed one as its new label, and any other reason as a measurement issue", () => {
  expect(reasonLabel("timeout")).toBe(FAILURE_REASONS.timeout);
  expect(reasonLabel("preparation-failed")).toBe(FAILURE_REASONS["path-failed"]);
  for (const reason of ["", "dns-failure", "toString", "__proto__"])
    expect(reasonLabel(reason)).toBe("Measurement issue");
});

test("a stage's state follows what the runner said about it", () => {
  const facts: StageFacts = {
    planned: true,
    settled: null,
    phase: null,
    stalled: false,
    ended: null,
  };
  const cases: [Partial<StageFacts>, StageState][] = [
    [{}, "upcoming"],
    [{ phase: "connecting" }, "upcoming"],
    [{ phase: "warmup" }, "warmup"],
    [{ phase: "warmup", stalled: true }, "recovering"],
    [{ phase: "upload" }, "running"],
    [{ phase: "upload", stalled: true }, "recovering"],
    [{ phase: "upload", settled: "partial" }, "partial"],
    [{ planned: false }, "skipped"],
    [{ planned: false, settled: "complete" }, "skipped"],
    [{ settled: "skipped" }, "skipped"],
    [{ phase: "upload", settled: "unreached", ended: "complete" }, "unreached"],
    [{ phase: "upload", ended: "stopped" }, "stopped"],
    [{ phase: "warmup", stalled: true, ended: "stopped" }, "stopped"],
    [{ phase: "connecting", ended: "stopped" }, "unreached"],
    [{ phase: "upload", ended: "complete" }, "unreached"],
    [{ ended: "complete" }, "unreached"],
    [{ phase: "upload", settled: "complete", ended: "complete" }, "complete"],
    [{ settled: "complete", ended: "stopped" }, "complete"],
  ];
  for (const [change, state] of cases)
    expect(stageState({ ...facts, ...change }), JSON.stringify(change)).toBe(state);
});

test("a server reads its name and location once, and all servers count who stayed", () => {
  const servers: [{ name: string; location?: string }, string][] = [
    [{ name: "Nuremberg", location: "Nuremberg, DE" }, "Nuremberg, DE"],
    [{ name: "nuremberg", location: "Nuremberg, DE" }, "Nuremberg, DE"],
    [{ name: "Hetzner Nuremberg", location: "nuremberg" }, "Hetzner Nuremberg"],
    [{ name: "Home lab", location: "Berlin, DE" }, "Home lab, Berlin, DE"],
    [{ name: "Frankfurt" }, "Frankfurt"],
    [{ name: "Edge", location: "" }, "Edge"],
  ];
  for (const [server, label] of servers)
    expect(serverLabel(server), JSON.stringify(server)).toBe(label);
  const counts: [number, number, number, string][] = [
    [3, 3, 3, "All 3 servers"],
    [3, 2, 3, "2 of 3 servers"],
    [3, 0, 1, "1 of 3 servers"],
    [3, 0, 0, "0 of 3 servers"],
  ];
  for (const [total, stayed, measured, label] of counts)
    expect(allServersLabel(total, stayed, measured)).toBe(label);
});

test("the stage limit names the server and each planned stage past it", () => {
  const oslo = { ms: 2_000, server: "Oslo" };
  const only = (download: boolean, upload: boolean) => ({
    ...DEFAULT_CONFIG,
    stages: { latency: false, download, upload, bidirectional: false },
  });
  const one = stageLimitError(only(true, false), oslo);
  expect(one).toContain("Oslo");
  expect([one.includes("Download"), one.includes("Upload")]).toEqual([true, false]);
  expect(stageLimitError(DEFAULT_CONFIG, oslo)).toContain(
    listed(["Latency", "Download", "Upload"]),
  );
  expect(stageLimitError(DEFAULT_CONFIG, { ms: 300_000, server: "Oslo" })).toBe("");
  expect(stageLimitError(DEFAULT_CONFIG, { ms: 86_400_000, server: null })).toBe("");
});

test("every stage state reads one word, the same on a chip, a card and a History row", () => {
  const unseen = {
    own: false,
    lost: false,
    spoken: null,
    held: null,
    idle: null,
    bytes: 0,
    shape: null,
  };
  const units = { base: "base10", kind: "bits", tier: 2 } as const;
  for (const state of Object.keys(STAGE_STATE) as StageState[]) {
    const { label } = STAGE_STATE[state];
    const card = liveCard("upload", state, unseen, units).word?.label;
    expect(card, state).toBe(state === "upcoming" || state === "complete" ? undefined : label);
    const tag = chipTag(state, true, false);
    if (tag !== null) expect(tag, state).toBe(label);
  }
  for (const state of ["partial", "failed", "stopped", "unreached"] as const)
    expect(chipTag(state, true, false), state).toBe(STAGE_STATE[state].label);
  expect(chipTag("upcoming", false, false)).toBe(STAGE_STATE.skipped.label);
  // History names the stages a result never measured as the console does.
  for (const status of ["partial", "failed", "unreached", "skipped"] as const)
    expect(STATUS_WORD[status], status).toBe(STAGE_STATE[status].label);
});
