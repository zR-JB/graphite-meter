// A worker's socket dial and its single-use ticket; a lapsed dial's ticket may be unspent, so close dials share it.
import { DecodeError } from "../../lib/decode";
import { readJSON, release, socketTicket } from "../../protocol/documents";
import type { EndReport } from "../../protocol/messages";
import type { TicketRecipe } from "../types";
import { ESTABLISH_MS, refused, RESTART_MS } from "./endings";

const REUSE_MS = 2 * ESTABLISH_MS + RESTART_MS;

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
        return {
          type: "ended",
          ending: refused(response, `socket ticket refused with HTTP ${response.status}`),
        };
      }
      const { token, expires } = socketTicket(await readJSON(response));
      this.#held = { url, token, until: Math.min(Date.now() + REUSE_MS, expires) };
      return { type: "token", token };
    } catch (error) {
      if (!(error instanceof DecodeError || error instanceof SyntaxError))
        return { type: "broken", detail: `socket ticket: ${String(error)}` };
      return {
        type: "ended",
        ending: {
          reason: "protocol-error",
          retry: false,
          detail: `socket ticket: ${error.message}`,
        },
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
