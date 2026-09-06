// Shared inline SVG icons and build identity constants used across the UI.
import { BUILD } from "./buildenv";

// Inline SVGs inherit currentColor so controls can theme them with text color.
const STROKE =
  'fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"';

export const ICON = {
  server: `<svg viewBox="0 0 24 24" ${STROKE}><rect x="3" y="3" width="18" height="6" rx="2"/><rect x="3" y="15" width="18" height="6" rx="2"/><path d="M7 6h.01M7 18h.01M12 9v6"/></svg>`,
  bolt: `<svg viewBox="0 0 24 24" ${STROKE}><path d="M13 2 4 14h6l-1 8 9-12h-6z"/></svg>`,
  download: `<svg viewBox="0 0 24 24" ${STROKE}><path d="M12 4v12"/><path d="m7 11 5 5 5-5"/><path d="M5 20h14"/></svg>`,
  upload: `<svg viewBox="0 0 24 24" ${STROKE}><path d="M12 20V8"/><path d="m7 13 5-5 5 5"/><path d="M5 4h14"/></svg>`,
  ping: `<svg viewBox="0 0 24 24" ${STROKE}><path d="M3 12h4l2-6 4 12 2-6h6"/></svg>`,
  bidirectional: `<svg viewBox="0 0 24 24" ${STROKE}><path d="M7 7h13"/><path d="m16 3 4 4-4 4"/><path d="M17 17H4"/><path d="m8 21-4-4 4-4"/></svg>`,
  settings: `<svg viewBox="0 0 24 24" ${STROKE}><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>`,
  sun: `<svg viewBox="0 0 24 24" ${STROKE}><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>`,
  moon: `<svg viewBox="0 0 24 24" ${STROKE}><path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z"/></svg>`,
  contrast: `<svg viewBox="0 0 24 24" ${STROKE}><circle cx="12" cy="12" r="8"/><path d="M12 4a8 8 0 0 1 0 16z" fill="currentColor" stroke="none"/></svg>`,
  check: `<svg viewBox="0 0 24 24" ${STROKE}><path d="M20 6 9 17l-5-5"/></svg>`,
  close: `<svg viewBox="0 0 24 24" ${STROKE}><path d="M18 6 6 18M6 6l12 12"/></svg>`,
  info: `<svg viewBox="0 0 24 24" ${STROKE}><circle cx="12" cy="12" r="9"/><path d="M12 11v5"/><path d="M12 8h.01"/></svg>`,
  history: `<svg viewBox="0 0 24 24" ${STROKE}><path d="M4 7h16v13H4z"/><path d="M8 4h8"/><path d="M8 11h8M8 15h5"/></svg>`,
  columns: `<svg viewBox="0 0 24 24" ${STROKE}><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16M9 10h12"/></svg>`,
  more: `<svg viewBox="0 0 24 24" ${STROKE}><circle cx="5" cy="12" r="1" fill="currentColor" stroke="none"/><circle cx="12" cy="12" r="1" fill="currentColor" stroke="none"/><circle cx="19" cy="12" r="1" fill="currentColor" stroke="none"/></svg>`,
  trash: `<svg viewBox="0 0 24 24" ${STROKE}><path d="M4 7h16M9 4h6l1 3H8zM6.5 7l.8 13h9.4l.8-13M10 11v5M14 11v5"/></svg>`,
} as const;

export const BUILD_IDENTITY = BUILD.identity;
