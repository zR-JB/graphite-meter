// The brand mark, "lattice needle": the hexagon is graphite's carbon lattice and a pencil's cross-section, drawn in the
// latency hue; the needle makes it a meter. One drawing for the favicon and the mark beside the name.

const glyph = (hexagon: number, needle: number, hub: number) =>
  `<path class="hex" d="M12 2.6 3.9 7.3v9.4l8.1 4.7 8.1-4.7V7.3Z" fill="none" stroke-width="${hexagon}" ` +
  `stroke-linejoin="round"/><path d="M12 12 18.6 8.2" stroke="currentColor" stroke-width="${needle}" ` +
  `stroke-linecap="round"/><circle cx="12" cy="12" r="${hub}" fill="currentColor"/>`;

/** The auth pages' brand line; their stylesheet strokes the hexagon, and the needle takes the text colour. */
export const BRAND_LINE = `<p class="brand"><svg class="mark" viewBox="0 0 24 24" aria-hidden="true">${glyph(2, 2.2, 2.1)}</svg>Graphite Meter</p>`;

/** The favicon from the `--phase-latency` and `--text` tokens' `light-dark()` pairs. The browser draws it apart from
    any page, so each scheme's colours are baked in as sRGB, and its strokes are heavier to hold up at 16 px. */
export function favicon(latency: string, text: string): string {
  const [hexagon, ink] = [schemes(latency), schemes(text)];
  const paint = (scheme: 0 | 1) => `svg{color:${ink[scheme]}}.hex{stroke:${hexagon[scheme]}}`;
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><style>${paint(0)}` +
    `@media (prefers-color-scheme:dark){${paint(1)}}</style>${glyph(2.4, 2.6, 2.4)}</svg>`
  );
}

/** `light-dark(oklch(…), oklch(…))` as its two sRGB hex colours, clipped to the gamut. */
function schemes(token: string): [string, string] {
  const pair = [...token.matchAll(/oklch\(([\d.]+) ([\d.]+) ([\d.]+)\)/g)].map((lch) =>
    hex(Number(lch[1]), Number(lch[2]), Number(lch[3])),
  );
  if (!token.startsWith("light-dark(") || pair.length !== 2) throw new Error(`not a light-dark() oklch pair: ${token}`);
  return pair as [string, string];
}

// OKLab to linear sRGB as in CSS Color 4.
function hex(lightness: number, chroma: number, hue: number): string {
  const a = chroma * Math.cos((hue * Math.PI) / 180);
  const b = chroma * Math.sin((hue * Math.PI) / 180);
  const l = (lightness + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (lightness - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (lightness - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const linear = [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
  const encode = (value: number) => {
    const clipped = Math.min(1, Math.max(0, value));
    const gamma = clipped <= 0.0031308 ? 12.92 * clipped : 1.055 * clipped ** (1 / 2.4) - 0.055;
    return Math.round(gamma * 255);
  };
  return `#${Uint8Array.from(linear, encode).toHex()}`;
}
