// The envelope every preflight and probe carries (api/discovery.md, The envelope): the part no protocol changes, read
// before the rest, so a server this page cannot speak to is named.
import { isRecord } from "../lib/decode";
import { displayText } from "./documents";

/** The API protocol this build speaks. A server raises it with each change that older pages cannot read. */
export const PROTOCOL = 2;
/** The first release of each protocol (api/protocols.txt). */
export const PROTOCOLS: readonly (readonly [number, string])[] = [
  [1, "0.1.0"],
  [2, "0.13.5"],
];

export interface Envelope {
  /** Null when missing or no safe text. */
  engineVersion: string | null;
  protocol: number;
}

/** A document's envelope, whatever else it holds; null without one. A preflight without `protocol` is from before the
 *  envelope: protocol 1, or 2 with a lease. A probe without `protocol` has none. */
export function envelope(value: unknown, preflight: boolean): Envelope | null {
  if (!isRecord(value)) return null;
  const { protocol, engineVersion: version } = value;
  if (protocol === undefined && (!preflight || typeof version !== "string")) return null;
  if (protocol !== undefined && !(Number.isSafeInteger(protocol) && (protocol as number) >= 1))
    return null;
  let engineVersion: string | null = null;
  try {
    engineVersion = displayText(256, 1)(version);
  } catch {}
  return {
    engineVersion,
    protocol: (protocol as number | undefined) ?? ("lease" in value ? 2 : 1),
  };
}

/** Why this page, of build `own`, cannot use `name` at `server`'s envelope; null when it can. */
export function mismatch(name: string, server: Envelope, own: string): string | null {
  const { engineVersion, protocol } = server;
  if (protocol === PROTOCOL) return null;
  const release = engineVersion
    ? `Graphite Meter ${engineVersion}`
    : "an unknown Graphite Meter release";
  if (protocol > PROTOCOL)
    return `${name} runs ${release}, newer than this page (${own}); reload the page or update the server this page comes from.`;
  const first = PROTOCOLS.at(-1)![1];
  return `${name} runs ${release} (protocol ${protocol}); this page needs protocol ${PROTOCOL}, from ${first}. Update ${name}.`;
}
