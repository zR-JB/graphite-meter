// Decoders type untrusted JSON; unknown members are ignored so newer servers stay readable, a mismatch throws.

export class DecodeError extends Error {}

export type Decoder<T> = (value: unknown, path?: string) => T;
export type Decoded<D> = D extends Decoder<infer T> ? T : never;

const fail = (path: string, expected: string): never => {
  throw new DecodeError(`${path || "the response"} must be ${expected}`);
};

const encoder = new TextEncoder();

/** A string of `min` to `max` UTF-8 bytes. */
export const string =
  (min = 0, max = Infinity): Decoder<string> =>
  (value, path = "") => {
    if (typeof value !== "string") return fail(path, "a string");
    // UTF-16 length bounds the UTF-8 length from below and three times it from above.
    const bytes =
      value.length >= min && value.length * 3 <= max ? value.length : encoder.encode(value).length;
    return bytes >= min && bytes <= max ? value : fail(path, `${min} to ${max} UTF-8 bytes`);
  };

export const integer =
  (min = 0, max = Number.MAX_SAFE_INTEGER): Decoder<number> =>
  (value, path = "") =>
    Number.isSafeInteger(value) && (value as number) >= min && (value as number) <= max
      ? (value as number)
      : fail(path, `an integer from ${min} to ${max}`);

export const boolean: Decoder<boolean> = (value, path = "") =>
  typeof value === "boolean" ? value : fail(path, "true or false");

export const oneOf =
  <const T extends readonly (string | number | boolean)[]>(...values: T): Decoder<T[number]> =>
  (value, path = "") =>
    values.includes(value as T[number])
      ? (value as T[number])
      : fail(path, `one of ${values.join(", ")}`);

export const array =
  <T>(item: Decoder<T>, max: number, min = 0): Decoder<T[]> =>
  (value, path = "") => {
    if (!Array.isArray(value)) return fail(path, "an array");
    if (value.length < min || value.length > max) fail(path, `${min} to ${max} items`);
    return value.map((each, index) => item(each, `${path}[${index}]`));
  };

type Shape = Record<string, Decoder<unknown>>;
type Fields<R extends Shape, O extends Shape> = { [K in keyof R]: Decoded<R[K]> } & {
  [K in keyof O]?: Decoded<O[K]>;
};

export const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const record = (value: unknown, path: string): Record<string, unknown> =>
  isRecord(value) ? value : fail(path, "an object");

/** An object with `required` members and `optional` ones, which may be absent but never null. */
export const object =
  <R extends Shape, O extends Shape = Record<never, never>>(
    required: R,
    optional?: O,
  ): Decoder<Fields<R, O>> =>
  (value, path = "") => {
    const fields = record(value, path);
    const out: Record<string, unknown> = {};
    for (const key in required) out[key] = required[key]!(fields[key], `${path}.${key}`);
    for (const key in optional)
      if (fields[key] !== undefined) out[key] = optional[key]!(fields[key], `${path}.${key}`);
    return out as Fields<R, O>;
  };

/** Objects told apart by their `type` member. */
export const variant =
  <V extends Record<string, Decoder<object>>>(
    variants: V,
  ): Decoder<{ [K in keyof V]: { type: K } & Decoded<V[K]> }[keyof V]> =>
  (value, path = "") => {
    const type = record(value, path).type;
    const decode =
      typeof type === "string" && Object.hasOwn(variants, type) ? variants[type] : undefined;
    if (!decode) return fail(`${path}.type`, `one of ${Object.keys(variants).join(", ")}`);
    return { ...decode(value, path), type } as never;
  };
