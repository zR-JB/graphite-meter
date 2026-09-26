/* Owner → worker message validation: a worker acts only on the exact shapes and requests its owner builds. */

import { ROUTES } from "../paths";
import type { WtMint } from "./wtToken";

type Route = (typeof ROUTES)[keyof typeof ROUTES];
type Fields = Record<string, unknown>;

export const HTTP_SCHEMES = ["http:", "https:"] as const;
const CREDENTIALS: readonly RequestCredentials[] = [
  "omit",
  "same-origin",
  "include",
];

const refused = (name: string): TypeError =>
  new TypeError(`worker message has an invalid ${name}`);

export function fields(value: unknown): Fields {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    throw refused("body");
  return value as Fields;
}

export function oneOf<const T extends string>(
  value: unknown,
  options: readonly T[],
  name: string,
): T {
  if (
    typeof value === "string" &&
    (options as readonly string[]).includes(value)
  )
    return value as T;
  throw refused(name);
}

export function integer(
  value: unknown,
  min: number,
  max: number,
  name: string,
): number {
  if (
    typeof value === "number" &&
    Number.isSafeInteger(value) &&
    value >= min &&
    value <= max
  )
    return value;
  throw refused(name);
}

/** A finite number of milliseconds or a count that may be fractional, at least `min`. */
export function finite(value: unknown, min: number, name: string): number {
  if (typeof value === "number" && Number.isFinite(value) && value >= min)
    return value;
  throw refused(name);
}

export function flag(value: unknown, name: string): boolean {
  if (typeof value === "boolean") return value;
  throw refused(name);
}

export const optional = <T>(
  value: unknown,
  parse: (value: unknown) => T,
): T | undefined => (value === undefined ? undefined : parse(value));

/** An absolute URL with an allowed scheme and a server route, carrying no credentials or fragment. */
export function requestUrl(
  value: unknown,
  schemes: readonly string[],
  routes: readonly Route[],
): string {
  if (typeof value === "string" && URL.canParse(value)) {
    const url = new URL(value);
    if (
      schemes.includes(url.protocol) &&
      (routes as readonly string[]).includes(url.pathname) &&
      url.username === "" &&
      url.password === "" &&
      url.hash === ""
    )
      return url.href;
  }
  throw refused("request URL");
}

export const requestCredentials = (
  value: unknown,
): RequestCredentials | undefined =>
  optional(value, (item) => oneOf(item, CREDENTIALS, "credentials mode"));

export function requestHeaders(
  value: unknown,
): Record<string, string> | undefined {
  if (value === undefined) return undefined;
  const headers: Record<string, string> = {};
  for (const [name, item] of Object.entries(fields(value))) {
    if (typeof item !== "string") throw refused("header");
    headers[name] = item;
  }
  return headers;
}

/** A CONNECT token mint posts to its server's session route. */
export const tokenMint = (value: unknown): WtMint | undefined =>
  optional(value, (item) => {
    const mint = fields(item);
    return {
      url: requestUrl(mint.url, HTTP_SCHEMES, [
        ROUTES.wsSession,
        ROUTES.wtSession,
      ]),
      headers: requestHeaders(mint.headers),
      credentials: requestCredentials(mint.credentials),
    };
  });
