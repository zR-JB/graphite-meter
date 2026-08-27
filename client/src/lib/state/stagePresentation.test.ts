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
  for (const [state, expected] of [
    [{ configured: false }, { status: "disabled", fill: 0 }],
    [{}, { status: "pending" }],
    [{ hasUsableResult: true }, { status: "complete", fill: 100 }],
    [{ hasFailure: true }, { status: "failed", fill: 0 }],
    [
      { hasUsableResult: true, hasFailure: true },
      { status: "partial", fill: 100 },
    ],
  ] as const)
    expect(
      deriveStagePresentation("download", { ...base, ...state }),
    ).toMatchObject(expected);
});

test("active and recovering stages retain their exact display progress", () => {
  for (const [state, expected] of [
    [
      { phase: "upload", phaseStage: "upload", phaseFraction: 0.243 },
      { status: "active", fill: 24.5, warming: false },
    ],
    [
      { phase: "warmup", phaseStage: "upload" },
      { status: "active", fill: 0, warming: true },
    ],
    [
      { phase: "upload", phaseStage: "upload", measuring: false },
      { status: "recovering", fill: 0 },
    ],
  ] as const)
    expect(
      deriveStagePresentation("upload", { ...base, ...state }),
    ).toMatchObject(expected);
});
