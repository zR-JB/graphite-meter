// The server checks as connection.ts imports them, each preflight timed as the scenario says rather than the network.
import { discover as measured, type CheckContext } from "../../src/engine/net/checks";
import { rttOf } from "./fakes";

export * from "../../src/engine/net/checks";

export async function discover(context: CheckContext, signal: AbortSignal) {
  const discovery = await measured(context, signal);
  return { ...discovery, requestMs: rttOf(new URL(context.entry.url).origin) };
}
