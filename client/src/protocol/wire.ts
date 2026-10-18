// Speaks the latency bus (api/wire.md), `PING,<uint32 id>` out and `PONG,<uint32 id>,<uint64 handling ns>` back.

export const ping = (id: number): string => `PING,${id}`;

const PONG = /^PONG,(\d{1,10}),(\d{1,20})$/;

/** A reply, or null when malformed; a handling time past a double's exact range drops only the server-timing diagnostic. */
export function pong(message: string): { id: number; handlingNs: number | null } | null {
  const match = PONG.exec(message);
  if (!match) return null;
  const id = Number(match[1]);
  const digits = match[2]!;
  if (id > 0xffff_ffff || (digits.length === 20 && digits > "18446744073709551615")) return null;
  const handlingNs = Number(digits);
  return { id, handlingNs: Number.isSafeInteger(handlingNs) ? handlingNs : null };
}
