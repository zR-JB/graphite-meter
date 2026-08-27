import { fmtSpeed } from "../format";

/* A live announcement is a measurement statement, so it receives only the authoritative presented transfer rate. */
export function authoritativeTransferAnnouncement(input: {
  authoritativeBytesPerSec: number;
  /** Provided to make the presentation boundary explicit; never announced. */
  visualBytesPerSec: number;
  toUnit: (bytesPerSec: number) => number;
  unit: string;
}): { value: string; unit: string } {
  void input.visualBytesPerSec;
  return {
    value: fmtSpeed(input.toUnit(input.authoritativeBytesPerSec)),
    unit: input.unit,
  };
}
