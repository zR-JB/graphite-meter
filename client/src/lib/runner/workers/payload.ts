// The incompressible source block every upload path writes, so a compressing hop cannot flatter the measurement.

/** crypto.getRandomValues' hard per-call byte quota. */
const RNG_CHUNK_BYTES = 65536;
/** Large enough that the fill cost never lands on a write, small enough to hold on a low-memory device. */
const FILL_BLOCK_BYTES = 4 * 1024 * 1024;

const filled = new Map<number, Uint8Array<ArrayBuffer>>();

/* The reusable block, filled once with CSPRNG bytes in quota-sized chunks. */
export function incompressibleBlock(
  bytes = FILL_BLOCK_BYTES,
): Uint8Array<ArrayBuffer> {
  const cached = filled.get(bytes);
  if (cached) return cached;
  const block = new Uint8Array(new ArrayBuffer(bytes));
  for (let off = 0; off < block.length; off += RNG_CHUNK_BYTES) {
    crypto.getRandomValues(
      block.subarray(off, Math.min(off + RNG_CHUNK_BYTES, block.length)),
    );
  }
  filled.set(bytes, block);
  return block;
}
