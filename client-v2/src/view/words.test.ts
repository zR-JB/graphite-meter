import { expect, test } from "bun:test";
import { rows } from "../protocol/pins.testutil";
import { allServersLabel, reasonLabel, serverLabel, stageState, type StageFacts, type StageState } from "./words";

test("failure labels match the pin Go and Rust load, and any other reason reads as a measurement issue", async () => {
  for (const [reason, label] of await rows("failurereasons.txt")) expect(reasonLabel(reason!)).toBe(label!);
  for (const reason of ["", "dns-failure", "toString", "__proto__"])
    expect(reasonLabel(reason)).toBe("Measurement issue");
});

test("a stage's state follows what the runner said about it", () => {
  const facts: StageFacts = { planned: true, settled: null, phase: null, stalled: false, ended: null };
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
    [{ settled: "not-run" }, "skipped"],
    [{ phase: "upload", ended: "stopped" }, "stopped"],
    [{ phase: "warmup", stalled: true, ended: "stopped" }, "stopped"],
    [{ phase: "upload", ended: "error" }, "failed"],
    [{ phase: "connecting", ended: "stopped" }, "unreached"],
    [{ phase: "upload", ended: "complete" }, "unreached"],
    [{ ended: "complete" }, "unreached"],
    [{ phase: "upload", settled: "complete", ended: "complete" }, "complete"],
    [{ settled: "complete", ended: "stopped" }, "complete"],
  ];
  for (const [change, state] of cases) expect(stageState({ ...facts, ...change }), JSON.stringify(change)).toBe(state);
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
  for (const [server, label] of servers) expect(serverLabel(server), JSON.stringify(server)).toBe(label);
  const counts: [number, number, number, string][] = [
    [3, 3, 3, "All 3 servers"],
    [3, 2, 3, "2 of 3 servers"],
    [3, 0, 1, "1 of 3 servers"],
    [3, 0, 0, "0 of 3 servers"],
  ];
  for (const [total, stayed, measured, label] of counts) expect(allServersLabel(total, stayed, measured)).toBe(label);
});
