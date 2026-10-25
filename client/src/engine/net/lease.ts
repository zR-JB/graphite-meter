// A server's lease (api/discovery.md): admission taken once at its discovery origin, which its measurements present.
import { lease as leaseDocument, readJSON, release } from "../../protocol/documents";
import { ROUTES } from "../../protocol/routes";
import { refused } from "../lanes/endings";
import { FAILURE_REASONS, type FailureReason } from "../../protocol/reasons";
import type { ServerAuth } from "../types";
import { CheckError } from "./checks";

const LEASE_HEADER = "Graphite-Meter-Lease";
/** The bus the lease holds whenever it is held. */
const STANDING = { measurements: 1, sessions: 0 };

export interface Need {
  measurements: number;
  sessions: number;
}

/** A refusal of the lease by this client's share or the server's capacity, which a check or Start shows. */
export class LeaseRefused extends CheckError {
  constructor(reason: FailureReason) {
    super(FAILURE_REASONS[reason], "retryable", { reason });
  }
}

/** The server's credentials with the lease presented: as a field on HTTP requests, as `lease=` on sockets. */
export interface Lease extends ServerAuth {
  /** Holds `need` beside the bus until the release, minting `uploads` ids. */
  hold(need: Need, signal: AbortSignal, uploads?: number): Promise<() => void>;
  /** Takes the lease for the bus alone, or proves the one held still stands. */
  renew(signal?: AbortSignal): Promise<void>;
  /** An upload id minted with the lease. */
  upload(signal: AbortSignal): Promise<string>;
  /** Ends the lease once nothing holds it beyond the bus. */
  end(): void;
}

export function createLease(url: string, auth: ServerAuth): Lease {
  const held = new Set<Need>();
  const minted: string[] = [];
  let token: string | null = null;
  let ending = false;
  // Requests go one at a time, so a later grant is never overtaken by an earlier one.
  let queue: Promise<unknown> = Promise.resolve();
  const field = (): Record<string, string> => (token ? { [LEASE_HEADER]: token } : {});
  const total = () => {
    const sum = { ...STANDING };
    for (const need of held) {
      sum.measurements += need.measurements;
      sum.sessions += need.sessions;
    }
    return sum;
  };
  const serially = <T>(send: () => Promise<T>): Promise<T> => {
    const sent = queue.then(send);
    queue = sent.catch(() => {});
    return sent;
  };
  /** Asks for what is held now, answering whether the lease holds all of it; a lowering asks nothing once it ended. */
  const post = (uploads: number, signal?: AbortSignal, lowering = false) =>
    serially(async () => {
      if (lowering && !token) return true;
      const asked = total();
      const response = await auth.fetch(`${url}${ROUTES.lease}`, {
        method: "POST",
        headers: { "Content-Type": "application/json", ...field() },
        body: JSON.stringify({ ...asked, uploads }),
        signal,
      });
      if (!response.ok) {
        const { reason } = refused(response);
        release(response);
        if (reason === "client-limit" || reason === "server-capacity")
          throw new LeaseRefused(reason);
        throw new CheckError(
          `The server did not admit the test (HTTP ${response.status})`,
          "retryable",
        );
      }
      const answer = leaseDocument(await readJSON(response));
      token = answer.token;
      minted.push(...answer.uploads);
      return answer.measurements >= asked.measurements && answer.sessions >= asked.sessions;
    });
  const lowered = () => {
    if (ending && !held.size) return lease.end();
    void post(0, undefined, true).catch(() => {});
  };
  const lease: Lease = {
    ...auth,
    request(target, method) {
      const recipe = auth.request(target, method);
      return { ...recipe, headers: { ...recipe.headers, ...field() } };
    },
    fetch(target, init = {}) {
      const headers = new Headers(init.headers);
      for (const [name, value] of Object.entries(field())) headers.set(name, value);
      return auth.fetch(target, { ...init, headers });
    },
    socket(target) {
      if (!token) return target;
      return `${target}${target.includes("?") ? "&" : "?"}lease=${encodeURIComponent(token)}`;
    },
    async hold(need, signal, uploads = 0) {
      ending = false;
      // A run's ids are its own; one an earlier run left would have expired.
      if (uploads) minted.length = 0;
      held.add(need);
      let fits: boolean;
      try {
        fits = await post(uploads, signal);
      } catch (error) {
        held.delete(need);
        // A refusal changed nothing; a request cut off after it left may have.
        if (!(error instanceof LeaseRefused)) lowered();
        throw error;
      }
      // The server cuts an ask past the client's share, which the run would then meet mid-stage.
      if (!fits) {
        held.delete(need);
        lowered();
        throw new LeaseRefused("client-limit");
      }
      let released = false;
      return () => {
        if (released) return;
        released = true;
        held.delete(need);
        lowered();
      };
    },
    async renew(signal) {
      ending = false;
      await post(0, signal);
    },
    async upload(signal) {
      if (!minted.length) await post(1, signal);
      return minted.shift()!;
    },
    end() {
      ending = held.size > 0;
      if (ending) return;
      // After what is queued, so a lease still being taken is ended too.
      void serially(async () => {
        if (!token || held.size) return;
        const ended = token;
        token = null;
        minted.length = 0;
        release(
          await auth.fetch(`${url}${ROUTES.lease}`, {
            method: "DELETE",
            headers: { [LEASE_HEADER]: ended },
            keepalive: true,
          }),
        );
      }).catch(() => {});
    },
  };
  return lease;
}
