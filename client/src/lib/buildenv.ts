// Vite replaces these tokens at build time. Use raw __GM_*__ constants for
// tree-shaken branches; BUILD is for ordinary runtime reads.
export const BUILD = {
  defaultEngine: __GM_DEFAULT_ENGINE__,
  allowDummy: __GM_ALLOW_DUMMY__,
  devTools: __GM_DEV_TOOLS__,
  profile: __GM_BUILD_PROFILE__,
  version: __GM_RELEASE_VERSION__,
  revision: __GM_SOURCE_REVISION__,
  clientVersion: __GM_CLIENT_VERSION__,
  identity: __GM_BUILD_IDENTITY__,
} as const;
