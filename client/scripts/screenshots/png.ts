// Writes deterministic 256-color PNGs from Chrome's: median cut, dithered alpha, maximum deflate.
import { deflateSync, inflateSync } from "node:zlib";

export interface Image {
  width: number;
  height: number;
  /** RGBA, row by row. */
  data: Uint8Array;
}

const SIGNATURE = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

export function decode(png: Uint8Array): Image {
  const bytes = Buffer.from(png);
  if (!bytes.subarray(0, 8).equals(SIGNATURE)) throw new Error("not a PNG");
  let width = 0;
  let height = 0;
  let channels = 0;
  const chunks: Buffer[] = [];
  for (let at = 8; at < bytes.length; ) {
    const length = bytes.readUInt32BE(at);
    const type = bytes.toString("latin1", at + 4, at + 8);
    const body = bytes.subarray(at + 8, at + 8 + length);
    if (type === "IHDR") {
      [width, height] = [body.readUInt32BE(0), body.readUInt32BE(4)];
      channels = { 2: 3, 6: 4 }[body[9]!] ?? 0;
      if (body[8] !== 8 || !channels || body[12] !== 0)
        throw new Error("only 8-bit RGB(A) without interlace");
    } else if (type === "IDAT") chunks.push(body);
    at += 12 + length;
  }
  const raw = inflateSync(Buffer.concat(chunks));
  const stride = width * channels;
  const rows = new Uint8Array(height * stride);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)]!;
    const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let x = 0; x < stride; x++) {
      const a = x >= channels ? rows[y * stride + x - channels]! : 0;
      const b = y ? rows[(y - 1) * stride + x]! : 0;
      const c = y && x >= channels ? rows[(y - 1) * stride + x - channels]! : 0;
      const p = a + b - c;
      const [pa, pb, pc] = [Math.abs(p - a), Math.abs(p - b), Math.abs(p - c)];
      const predicted = [0, a, b, (a + b) >> 1, pa <= pb && pa <= pc ? a : pb <= pc ? b : c][
        filter
      ]!;
      rows[y * stride + x] = (line[x]! + predicted) & 255;
    }
  }
  if (channels === 4) return { width, height, data: rows };
  const data = new Uint8Array(width * height * 4).fill(255);
  for (let i = 0; i < width * height; i++) data.set(rows.subarray(i * 3, i * 3 + 3), i * 4);
  return { width, height, data };
}

/** Up to 256 RGBA colors by median cut over the distinct colors, each weighted by its pixels. */
function palette(image: Image): number[][] {
  const counts = new Map<number, number>();
  const view = new DataView(image.data.buffer, image.data.byteOffset);
  for (let i = 0; i < image.data.length; i += 4) {
    const color = view.getUint32(i);
    counts.set(color, (counts.get(color) ?? 0) + 1);
  }
  const colors = [...counts].map(([color, count]) => [
    color >>> 24,
    (color >>> 16) & 255,
    (color >>> 8) & 255,
    color & 255,
    count,
  ]);
  const boxes = [colors];
  const extent = (box: number[][]) => {
    let widest = 0;
    let span = -1;
    for (let channel = 0; channel < 4; channel++) {
      let [low, high] = [255, 0];
      for (const color of box)
        [low, high] = [Math.min(low, color[channel]!), Math.max(high, color[channel]!)];
      if (high - low > span) [widest, span] = [channel, high - low];
    }
    return { widest, span, weight: box.reduce((sum, color) => sum + color[4]!, 0) };
  };
  while (boxes.length < 256) {
    let best = -1;
    let score = 0;
    boxes.forEach((box, index) => {
      const { span, weight } = extent(box);
      if (box.length > 1 && span > 0 && Math.sqrt(weight) * span > score)
        [best, score] = [index, Math.sqrt(weight) * span];
    });
    if (best < 0) break;
    const box = boxes[best]!;
    const { widest, weight } = extent(box);
    box.sort(
      (a, b) =>
        a[widest]! - b[widest]! || a[0]! - b[0]! || a[1]! - b[1]! || a[2]! - b[2]! || a[3]! - b[3]!,
    );
    let half = 0;
    let cut = 1;
    for (; cut < box.length - 1 && (half += box[cut - 1]![4]!) < weight / 2; cut++);
    boxes.splice(best, 1, box.slice(0, cut), box.slice(cut));
  }
  return boxes.map((box) => {
    const weight = box.reduce((sum, color) => sum + color[4]!, 0);
    return [0, 1, 2, 3].map((channel) =>
      Math.round(box.reduce((sum, color) => sum + color[channel]! * color[4]!, 0) / weight),
    );
  });
}

/** The image in at most 256 colors, as a PNG with a palette and, when any color is translucent, its alphas. */
export function encode(image: Image): Uint8Array {
  const { width, height } = image;
  const colors = palette(image);
  const nearest = new Map<number, number>();
  const pick = (r: number, g: number, b: number, a: number) => {
    const key = ((r << 24) | (g << 16) | (b << 8) | a) >>> 0;
    let index = nearest.get(key);
    if (index === undefined) {
      let best = Infinity;
      colors.forEach(([pr, pg, pb, pa], at) => {
        const distance = (pr! - r) ** 2 + (pg! - g) ** 2 + (pb! - b) ** 2 + 2 * (pa! - a) ** 2;
        if (distance < best) [best, index] = [distance, at];
      });
      nearest.set(key, index!);
    }
    return index!;
  };
  // Only alpha is dithered: colors map to their nearest, which keeps flat UI flat, and a fade keeps its steps fine.
  const error = new Float32Array((width + 2) * 2);
  const indices = new Uint8Array((width + 1) * height);
  for (let y = 0; y < height; y++) {
    const [current, next] = [(y & 1) * (width + 2), ((y + 1) & 1) * (width + 2)];
    error.fill(0, next, next + width + 2);
    for (let x = 0; x < width; x++) {
      const at = (y * width + x) * 4;
      const [r, g, b, a] = image.data.subarray(at, at + 4) as unknown as [
        number,
        number,
        number,
        number,
      ];
      const alpha = Math.min(255, Math.max(0, Math.round(a + error[current + x + 1]!)));
      const index = pick(r, g, b, alpha);
      indices[y * (width + 1) + 1 + x] = index;
      const spill = alpha - colors[index]![3]!;
      error[current + x + 2]! += (spill * 7) / 16;
      error[next + x]! += (spill * 3) / 16;
      error[next + x + 1]! += (spill * 5) / 16;
      error[next + x + 2]! += spill / 16;
    }
  }
  const chunk = (type: string, body: Uint8Array) => {
    const out = Buffer.alloc(12 + body.length);
    out.writeUInt32BE(body.length, 0);
    out.write(type, 4, "latin1");
    out.set(body, 8);
    out.writeUInt32BE(Bun.hash.crc32(out.subarray(4, 8 + body.length)), 8 + body.length);
    return out;
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header.set([8, 3, 0, 0, 0], 8);
  const translucent = colors.some((color) => color[3] !== 255);
  return Buffer.concat([
    SIGNATURE,
    chunk("IHDR", header),
    chunk("PLTE", Uint8Array.from(colors.flatMap((color) => color.slice(0, 3)))),
    ...(translucent ? [chunk("tRNS", Uint8Array.from(colors.map((color) => color[3]!)))] : []),
    chunk("IDAT", deflateSync(indices, { level: 9, memLevel: 9 })),
    chunk("IEND", new Uint8Array()),
  ]);
}
