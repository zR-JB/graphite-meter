/** Minimal Worker seam shared by channel tests that drive the real handlers. */
export class TestWorker {
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  readonly sent: unknown[] = [];
  terminated = 0;
  static last: TestWorker | null = null;

  constructor() {
    TestWorker.last = this;
  }

  postMessage(message: unknown): void {
    this.sent.push(message);
  }

  terminate(): void {
    this.terminated++;
  }

  emit(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent);
  }
}
