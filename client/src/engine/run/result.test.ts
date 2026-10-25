import { expect, test } from "bun:test";
import { FAILURE_REASONS, type FailureReason } from "../../protocol/reasons";
import { failureSentence } from "./result";
import { STAGES, type Stage } from "./schedule";

test("every failure reads as one sentence naming its server, never its reason's label again", () => {
  const places: { stage: Stage | "start"; scope: "throughput" | "latency" }[] = [
    { stage: "start", scope: "throughput" },
    ...STAGES.map(
      (stage) => ({ stage, scope: stage === "latency" ? "latency" : "throughput" }) as const,
    ),
    ...STAGES.filter((stage) => stage !== "latency").map(
      (stage) => ({ stage, scope: "latency" }) as const,
    ),
  ];
  for (const reason of Object.keys(FAILURE_REASONS) as FailureReason[])
    for (const place of places) {
      const said = failureSentence({ reason, ...place }, "Oslo");
      const name = `${reason} ${place.scope} in ${place.stage}`;
      expect(said, name).toMatch(/^[A-Z][^.]*\.$/);
      expect(said, name).toContain("Oslo");
      expect(said.toLowerCase(), name).not.toContain(FAILURE_REASONS[reason].toLowerCase());
      // Where it happened: at Start, or the stage it happened in.
      expect(said.toLowerCase(), name).toContain(place.stage === "start" ? "started" : place.stage);
    }
  // A reason an earlier build named otherwise, or one a newer build adds, still reads as a sentence.
  expect(
    failureSentence({ reason: "server-busy", stage: "start", scope: "throughput" }, "Oslo"),
  ).toBe("Oslo had no room for another test when this one started.");
  expect(
    failureSentence({ reason: "dns-failure", stage: "upload", scope: "throughput" }, "Oslo"),
  ).toBe("During the upload, Oslo ran into a problem this page does not name.");
  expect(
    failureSentence({ reason: "connection-lost", stage: "download", scope: "throughput" }, "Oslo"),
  ).toBe("Download from Oslo lost its connection.");
});
