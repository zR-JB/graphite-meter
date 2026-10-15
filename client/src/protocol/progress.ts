// Upload receiver records (api/upload.md): one JSON object per NDJSON line; blank lines are heartbeats.
import { integer, object, string, variant, type Decoded } from "../lib/decode";

const counters = object({ bytes: integer(), nanos: integer() });

export const progressRecord = variant({
  ready: object({}),
  progress: counters,
  complete: counters,
  error: object({}, { code: string(), message: string() }),
});

export type ProgressRecord = Decoded<typeof progressRecord>;
