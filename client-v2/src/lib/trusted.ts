// The page's one Trusted Types policy. The shell's marker names it, and the servers then require Trusted Types for
// every script sink, so a worker starts only from a URL this policy made: one of the build's own worker bundles.

/** The policy name the shell's marker carries; build.ts checks the two agree. */
export const TRUSTED_TYPES_POLICY = "graphite-meter";

type ScriptURLPolicy = { createScriptURL(url: string): string };
const { trustedTypes } = globalThis as {
  trustedTypes?: { createPolicy(name: string, rules: ScriptURLPolicy): ScriptURLPolicy };
};
const policy = trustedTypes?.createPolicy(TRUSTED_TYPES_POLICY, {
  createScriptURL(url) {
    if ([WORKERS.clock, WORKERS.lane, WORKERS.session, WORKERS.probe].includes(url)) return url;
    throw new TypeError(`${url} is not a worker of this app`);
  },
});

/** `url` as the Worker constructor takes it: a trusted script URL where the browser has Trusted Types. */
export const workerScript = (url: string): string => policy?.createScriptURL(url) ?? url;
