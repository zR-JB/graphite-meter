import type { ConnectionRole } from "../contract";

/* A transport a role needs is unavailable: not advertised, blocked by the browser, or it never established. */
export class TransportUnavailableError extends Error {
  readonly role?: ConnectionRole;

  constructor(
    message: string,
    options?: ErrorOptions & { role?: ConnectionRole },
  ) {
    super(message, options);
    this.role = options?.role;
  }
}

/** The shared discovery request failed before either role could be checked. */
export class PreflightUnavailableError extends Error {}
