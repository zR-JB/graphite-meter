/// <reference types="svelte" />
/// <reference types="vite/client" />

// Build-time constants literal-substituted by Vite `define` (see vite.config.ts).
// `svelte-check` runs without `define`, so these declarations keep references typed.

declare const __GM_DEFAULT_ENGINE__: "real" | "dummy";
declare const __GM_ALLOW_DUMMY__: boolean;
declare const __GM_DEV_TOOLS__: boolean;
declare const __GM_BENCH__: boolean;
declare const __GM_BUILD_LABEL__: string;
declare const __GM_CLIENT_VERSION__: string;
