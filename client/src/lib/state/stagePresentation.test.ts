import { expect, test } from "bun:test";
import { deriveStagePresentation } from "./stagePresentation";

const base = {
  configured: true,
  phase: "connecting" as const,
  phaseStage: null,
  phaseFraction: 0,
  measuring: true,
  hasUsableResult: false,
  hasFailure: false,
};

test("stage presentation has one truthful result/failure classification", () => {
  expect(
    deriveStagePresentation("download", { ...base, configured: false }),
  ).toMatchObject({ status: "disabled", fill: 0 });
  expect(deriveStagePresentation("download", base)).toMatchObject({
    status: "pending",
  });
  expect(
    deriveStagePresentation("download", {
      ...base,
      hasUsableResult: true,
    }),
  ).toMatchObject({ status: "complete", fill: 100 });
  expect(
    deriveStagePresentation("download", {
      ...base,
      hasFailure: true,
    }),
  ).toMatchObject({ status: "failed", fill: 0 });
  expect(
    deriveStagePresentation("download", {
      ...base,
      hasUsableResult: true,
      hasFailure: true,
    }),
  ).toMatchObject({ status: "partial", fill: 100 });
});

test("active and recovering stages retain their exact display progress", () => {
  expect(
    deriveStagePresentation("upload", {
      ...base,
      phase: "upload",
      phaseStage: "upload",
      phaseFraction: 0.243,
    }),
  ).toMatchObject({ status: "active", fill: 24.5, warming: false });
  expect(
    deriveStagePresentation("upload", {
      ...base,
      phase: "warmup",
      phaseStage: "upload",
    }),
  ).toMatchObject({ status: "active", fill: 0, warming: true });
  expect(
    deriveStagePresentation("upload", {
      ...base,
      phase: "upload",
      phaseStage: "upload",
      measuring: false,
    }),
  ).toMatchObject({ status: "recovering", fill: 0 });
});
