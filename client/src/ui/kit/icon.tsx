// Draws the interface's glyphs as strokes in the current color on a 24-unit grid, sized by their context.
const GLYPHS = {
  server: () => [
    <rect x="3" y="3" width="18" height="6" rx="2" />,
    <rect x="3" y="15" width="18" height="6" rx="2" />,
    <path d="M7 6h.01M7 18h.01M12 9v6" />,
  ],
  bolt: () => [<path d="M13 2 4 14h6l-1 8 9-12h-6z" />],
  download: () => [<path d="M12 4v12" />, <path d="m7 11 5 5 5-5" />, <path d="M5 20h14" />],
  upload: () => [<path d="M12 20V8" />, <path d="m7 13 5-5 5 5" />, <path d="M5 4h14" />],
  ping: () => [<path d="M3 12h4l2-6 4 12 2-6h6" />],
  bidirectional: () => [
    <path d="M7 7h13" />,
    <path d="m16 3 4 4-4 4" />,
    <path d="M17 17H4" />,
    <path d="m8 21-4-4 4-4" />,
  ],
  settings: () => [
    <circle cx="12" cy="12" r="3" />,
    <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />,
  ],
  sun: () => [
    <circle cx="12" cy="12" r="4" />,
    <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />,
  ],
  moon: () => [<path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z" />],
  contrast: () => [
    <circle cx="12" cy="12" r="8" />,
    <path d="M12 4a8 8 0 0 1 0 16z" fill="currentColor" stroke="none" />,
  ],
  check: () => [<path d="M20 6 9 17l-5-5" />],
  chevron: () => [<path d="m9 5 7 7-7 7" />],
  chevrons: () => [<path d="m6 6 6 6-6 6" />, <path d="m13 6 6 6-6 6" />],
  external: () => [<path d="M8 16 16 8M9 8h7v7" />],
  copy: () => [
    <rect x="9" y="9" width="12" height="12" rx="2" />,
    <path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1" />,
  ],
  close: () => [<path d="M18 6 6 18M6 6l12 12" />],
  info: () => [<circle cx="12" cy="12" r="9" />, <path d="M12 11v5" />, <path d="M12 8h.01" />],
  history: () => [<path d="M3 7h18v14H3z" />, <path d="M7 3.5h10" />, <path d="M7 11h10M7 15h6" />],
  columns: () => [<rect x="3" y="4" width="18" height="16" rx="2" />, <path d="M9 4v16M9 10h12" />],
  more: () => [
    <circle cx="5" cy="12" r="1" fill="currentColor" />,
    <circle cx="12" cy="12" r="1" fill="currentColor" />,
    <circle cx="19" cy="12" r="1" fill="currentColor" />,
  ],
  person: () => [<circle cx="12" cy="8.4" r="3.6" />, <path d="M5.4 19.2c.6-3.6 2.8-5.4 6.6-5.4s6 1.8 6.6 5.4" />],
  power: () => [<path d="M12 3.6v8.4" />, <path d="M7.2 6.6a7.2 7.2 0 1 0 9.6 0" />],
  exit: () => [<path d="M9 4H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h4M16 7l5 5-5 5M9 12h12" />],
  trash: () => [<path d="M4 7h16M9 4h6l1 3H8zM6.5 7l.8 13h9.4l.8-13M10 11v5M14 11v5" />],
  eye: () => [
    <path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12Z" />,
    <circle cx="12" cy="12" r="3" />,
    <path class="strike" d="M4 4l16 16" />,
  ],
};

export type IconName = keyof typeof GLYPHS;

export const Icon = (props: { name: IconName }) => (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.9"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    {GLYPHS[props.name]()}
  </svg>
);
