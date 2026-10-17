// Upload receiver records (api/upload.md): one JSON object per NDJSON line; blank lines are heartbeats.
import { integer, object, string, variant, type Decoded } from "../lib/decode";

/** A receiver's counters, from a feed record or a checkpoint. */
export const checkpoint = object({ bytes: integer(), nanos: integer() });

export const progressRecord = variant({
  ready: object({}),
  progress: checkpoint,
  complete: checkpoint,
  error: object({}, { code: string(), message: string() }),
});

export type ProgressRecord = Decoded<typeof progressRecord>;
