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
    }
  expect(
    failureSentence({ reason: "connection-lost", stage: "download", scope: "throughput" }, "Oslo"),
  ).toBe("Download from Oslo lost its connection.");
});
