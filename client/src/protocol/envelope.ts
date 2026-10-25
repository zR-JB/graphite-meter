// The envelope every preflight and probe carries (api/discovery.md, The envelope): the part no API version changes,
// read before the rest, so a server this page cannot speak to is named.
import { isRecord } from "../lib/decode";
import { displayText } from "./documents";

/** The API version this build speaks. A server raises it with each change that older pages cannot read. */
export const API_VERSION = 2;
/** The first release of each API version (api/apiversions.txt). */
export const API_VERSIONS: readonly (readonly [number, string])[] = [
  [1, "0.1.0"],
  [2, "0.13.5"],
];

export interface Envelope {
  /** Null when missing or no safe text. */
  engineVersion: string | null;
  apiVersion: number;
}

/** A document's envelope, whatever else it holds; null without one. A preflight without `apiVersion` is from before
 *  the envelope: API version 1, or 2 with a lease. A probe without `apiVersion` has none. */
export function envelope(value: unknown, preflight: boolean): Envelope | null {
  if (!isRecord(value)) return null;
  const { apiVersion, engineVersion: version } = value;
  if (apiVersion === undefined && (!preflight || typeof version !== "string")) return null;
  if (
    apiVersion !== undefined &&
    !(Number.isSafeInteger(apiVersion) && (apiVersion as number) >= 1)
  )
    return null;
  let engineVersion: string | null = null;
  try {
    engineVersion = displayText(256, 1)(version);
  } catch {}
  return {
    engineVersion,
    apiVersion: (apiVersion as number | undefined) ?? ("lease" in value ? 2 : 1),
  };
}

/** Why this page, of build `own`, cannot use `name` at `server`'s envelope; null when it can. */
export function mismatch(name: string, server: Envelope, own: string): string | null {
  const { engineVersion, apiVersion } = server;
  if (apiVersion === API_VERSION) return null;
  const release = engineVersion
    ? `Graphite Meter ${engineVersion}`
    : "an unknown Graphite Meter release";
  if (apiVersion > API_VERSION)
    return `${name} runs ${release}, newer than this page (${own}); reload the page or update the server this page comes from.`;
  const first = API_VERSIONS.at(-1)![1];
  return `${name} runs ${release} (API version ${apiVersion}); this page needs API version ${API_VERSION}, from ${first}. Update ${name}.`;
}
