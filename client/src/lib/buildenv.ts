// Vite replaces these tokens at build time.
export const BUILD = {
  profile: __GM_BUILD_PROFILE__,
  version: __GM_RELEASE_VERSION__,
  revision: __GM_SOURCE_REVISION__,
  clientVersion: __GM_CLIENT_VERSION__,
  identity: __GM_BUILD_IDENTITY__,
} as const;
