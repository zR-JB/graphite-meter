/// <reference types="svelte" />
/// <reference types="vite/client" />



// Build-time constants injected by Vite `define` (see vite.config.ts). These
// are literal-substituted at build time; declared here so `svelte-check` (which
// runs before `vite build` and never sees `define`) type-checks references.

declare const __GM_DEFAULT_ENGINE__: "real" | "dummy";
declare const __GM_ALLOW_DUMMY__: boolean;
declare const __GM_DEV_TOOLS__: boolean;
declare const __GM_BUILD_LABEL__: string;
declare const __GM_CLIENT_VERSION__: string;
