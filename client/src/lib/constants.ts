import { BUILD } from "./buildenv";

const A =
  'fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"';

export const ICON = {
  bolt: `<svg viewBox="0 0 24 24" ${A}><path d="M13 2 4 14h6l-1 8 9-12h-6z"/></svg>`,
  download: `<svg viewBox="0 0 24 24" ${A}><path d="M12 4v12"/><path d="m7 11 5 5 5-5"/><path d="M5 20h14"/></svg>`,
  upload: `<svg viewBox="0 0 24 24" ${A}><path d="M12 20V8"/><path d="m7 13 5-5 5 5"/><path d="M5 4h14"/></svg>`,
  ping: `<svg viewBox="0 0 24 24" ${A}><path d="M3 12h4l2-6 4 12 2-6h6"/></svg>`,
  bidirectional: `<svg viewBox="0 0 24 24" ${A}><path d="M7 7h13"/><path d="m16 3 4 4-4 4"/><path d="M17 17H4"/><path d="m8 21-4-4 4-4"/></svg>`,
  server: `<svg viewBox="0 0 24 24" ${A}><rect x="4" y="4" width="16" height="6" rx="1"/><rect x="4" y="14" width="16" height="6" rx="1"/><path d="M8 7h.01M8 17h.01"/></svg>`,
  globe: `<svg viewBox="0 0 24 24" ${A}><circle cx="12" cy="12" r="8"/><path d="M2 12h20M12 4c2.5 2.5 2.5 13 0 16M12 4c-2.5 2.5-2.5 13 0 16"/></svg>`,
  activity: `<svg viewBox="0 0 24 24" ${A}><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>`,
  gauge: `<svg viewBox="0 0 24 24" ${A}><path d="M12 14 15.5 9"/><path d="M4 18a8 8 0 1 1 16 0"/><circle cx="12" cy="14" r="1.4" fill="currentColor" stroke="none"/></svg>`,
  settings: `<svg viewBox="0 0 24 24" ${A}><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>`,
  sun: `<svg viewBox="0 0 24 24" ${A}><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>`,
  moon: `<svg viewBox="0 0 24 24" ${A}><path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z"/></svg>`,
  check: `<svg viewBox="0 0 24 24" ${A}><path d="M20 6 9 17l-5-5"/></svg>`,
  inventory: `<svg viewBox="0 0 24 24" ${A}><path d="M3 7h18M3 7l1.5 12.5a1 1 0 0 0 1 .9h11a1 1 0 0 0 1-.9L19 7M8 7V5a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>`,
  route: `<svg viewBox="0 0 24 24" ${A}><circle cx="6" cy="19" r="2"/><circle cx="18" cy="5" r="2"/><path d="M6 17V9a4 4 0 0 1 4-4h4"/></svg>`,
  flask: `<svg viewBox="0 0 24 24" ${A}><path d="M9 3h6M10 3v6L4.5 18a2 2 0 0 0 1.8 3h11.4a2 2 0 0 0 1.8-3L14 9V3"/><path d="M7 14h10"/></svg>`,
  close: `<svg viewBox="0 0 24 24" ${A}><path d="M18 6 6 18M6 6l12 12"/></svg>`,
  info: `<svg viewBox="0 0 24 24" ${A}><circle cx="12" cy="12" r="9"/><path d="M12 11v5"/><path d="M12 8h.01"/></svg>`,
} as const;

export type IconName = keyof typeof ICON;

export const BUILD_HASH = BUILD.buildLabel;
