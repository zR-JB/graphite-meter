const globals = globalThis as Record<string, unknown>;

export interface WorkerRealm<Out> {
  posted: Out[];
  send(message: unknown): void;
}

export async function bootWorker<Out>(
  modulePath: string,
  realm: number,
): Promise<WorkerRealm<Out>> {
  const posted: Out[] = [];
  globals.postMessage = (message: Out): void => {
    posted.push(message);
  };
  await import(`${modulePath}?realm=${realm}`);
  const handler = globalThis.onmessage as (event: MessageEvent) => void;
  return {
    posted,
    send: (message) => handler({ data: message } as MessageEvent),
  };
}
