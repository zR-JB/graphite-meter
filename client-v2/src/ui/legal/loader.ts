// The server's legal notices (legal/about.json beside the page): the project's licence and the third-party
// software it ships. A load that succeeded is kept for the page's life; a failed one is tried again when asked.
import { array, boolean, DecodeError, object, string, type Decoded, type Decoder } from "../../lib/decode";
import { displayText, readJSON } from "../../protocol/documents";

/** A link the page may follow: http(s), absolute or relative to the page. */
const href: Decoder<string> = (value, path = "") => {
  const text = string(1, 2048)(value, path);
  const resolved = URL.parse(text, document.baseURI);
  if (resolved?.protocol !== "https:" && resolved?.protocol !== "http:")
    throw new DecodeError(`${path} must be a web link`);
  return text;
};

const about = object({
  project: object({
    name: displayText(256, 1),
    copyrightHolder: displayText(256),
    copyrightYears: displayText(64),
    licenseExpression: displayText(256),
  }),
  sourceURL: href,
  licenseURL: href,
  noticesURL: href,
  components: array(
    object({
      name: displayText(256, 1),
      version: displayText(256),
      ecosystem: displayText(64, 1),
      selectedLicenseExpression: displayText(256),
      modified: boolean,
      links: array(object({ label: displayText(64), url: href }), 16),
    }),
    8_192,
  ),
});

export type About = Decoded<typeof about>;
export type LegalComponent = About["components"][number];

/** A server that never answers fails the load, so Retry is offered rather than "Loading" for good. */
const LOAD_MS = 10_000;
/** The Rust server lists every crate it links, about 50 KiB of notices; this leaves room for many more. */
const MAX_ABOUT_BYTES = 1024 * 1024;

async function fetchAbout(): Promise<About> {
  const response = await fetch(new URL("legal/about.json", document.baseURI), {
    credentials: "same-origin",
    headers: { Accept: "application/json" },
    signal: AbortSignal.timeout(LOAD_MS),
  });
  if (!response.ok) {
    response.body?.cancel().catch(() => {});
    throw new Error(`legal notices returned ${response.status}`);
  }
  return about(await readJSON(response, MAX_ABOUT_BYTES));
}

let loaded: Promise<About> | null = null;

export function loadLegal(): Promise<About> {
  loaded ??= fetchAbout().catch((error: unknown) => {
    loaded = null;
    throw error;
  });
  return loaded;
}
