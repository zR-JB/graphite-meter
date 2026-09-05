/** The client owns the uint32 probe ID; handling is retained as exact uint64 digits. */
export interface Pong {
  id: number;
  handlingNanos: string;
}

const MAX_U64_DIGITS = "18446744073709551615";

function digitsOnly(value: string): boolean {
  if (!value.length) return false;
  for (let index = 0; index < value.length; index++) {
    const code = value.charCodeAt(index);
    if (code < 48 || code > 57) return false;
  }
  return true;
}

/** Strictly parse PONG,<uint32 ID>,<uint64 handling ns>; malformed replies are ignored. */
export function decodePong(message: string): Pong | null {
  if (!message.startsWith("PONG,")) return null;
  const comma = message.indexOf(",", 5);
  if (comma === -1) return null;
  const idText = message.slice(5, comma);
  const handlingNanos = message.slice(comma + 1);
  if (
    idText.length > 10 ||
    !digitsOnly(idText) ||
    Number(idText) > 4294967295 ||
    handlingNanos.length > 20 ||
    !digitsOnly(handlingNanos) ||
    (handlingNanos.length === 20 && handlingNanos > MAX_U64_DIGITS)
  )
    return null;
  return { id: Number(idText), handlingNanos };
}

export function encodePing(id: number): string {
  return `PING,${id}`;
}
