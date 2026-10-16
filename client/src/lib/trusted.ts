// The page's one Trusted Types policy. The shell's marker names it, and the servers then require Trusted Types for
// every script sink, so a worker starts only from a URL this policy made: one of the build's own worker bundles.

/** The policy name the shell's marker carries; build.ts checks the two agree. */
export const TRUSTED_TYPES_POLICY = "graphite-meter";

type ScriptURLPolicy = { createScriptURL(url: string): string };
const { trustedTypes } = globalThis as {
  trustedTypes?: { createPolicy(name: string, rules: ScriptURLPolicy): ScriptURLPolicy };
};
let policy: ScriptURLPolicy | undefined;

/** `url` as the Worker constructor takes it: a trusted script URL where the browser has Trusted Types. The policy is
    made with the first worker, so a module that only imports this creates none. */
export function workerScript(url: string): string {
  policy ??= trustedTypes?.createPolicy(TRUSTED_TYPES_POLICY, {
    createScriptURL(url) {
      // The build defines each WORKERS member, never WORKERS itself.
      if ([WORKERS.clock, WORKERS.lane, WORKERS.session, WORKERS.probe].includes(url)) return url;
      throw new TypeError(`${url} is not a worker of this app`);
    },
  });
  return policy?.createScriptURL(url) ?? url;
}
