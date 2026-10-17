// Dialing a socket from a worker: its single-use ticket (api/wire.md#socket-credentials), minted right before a dial.
// The server spends one on any answer, a refusal too; one whose dial lapsed unanswered may never have reached it, so
// close dials share it, keeping a worker's retries inside the server's outstanding tickets per sign-in.
import { DecodeError } from "../../lib/decode";
import { readJSON, release, socketTicket } from "../../protocol/documents";
import type { EndReport } from "../../protocol/messages";
import type { TicketRecipe } from "../types";
import { refused } from "./endings";

/** Two dial budgets and a restart pause. */
const REUSE_MS = 6_300;

type Ticket = { type: "token"; token: string } | EndReport;

/** One worker's held ticket. Its lifetime is wall-clock time, as the server states `expires`. */
export class Tickets {
  #held: { url: string; token: string; until: number } | null = null;

  async take({ url, request }: TicketRecipe, signal: AbortSignal): Promise<Ticket> {
    const held = this.#held;
    if (held?.url === url && Date.now() < held.until) return { type: "token", token: held.token };
    this.#held = null;
    try {
      const response = await fetch(url, { ...request, method: "POST", cache: "no-store", signal });
      if (!response.ok) {
        release(response);
        return { type: "ended", ending: refused(response, `socket ticket refused with HTTP ${response.status}`) };
      }
      const { token, expires } = socketTicket(await readJSON(response));
      this.#held = { url, token, until: Math.min(Date.now() + REUSE_MS, expires) };
      return { type: "token", token };
    } catch (error) {
      if (!(error instanceof DecodeError || error instanceof SyntaxError))
        return { type: "broken", detail: `socket ticket: ${String(error)}` };
      return {
        type: "ended",
        ending: { reason: "protocol-error", retry: false, detail: `socket ticket: ${error.message}` },
      };
    }
  }

  /** A dial carrying `token` was answered, which spent it. */
  spent(token: string): void {
    if (this.#held?.token === token) this.#held = null;
  }
}

/** The dial URL with its ticket; the empty ticket of a server without authentication adds nothing. */
export const withToken = (url: string, token: string) =>
  token ? `${url}${url.includes("?") ? "&" : "?"}token=${encodeURIComponent(token)}` : url;
