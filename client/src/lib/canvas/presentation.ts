const PRESENTATION_MAX_FPS = 30;
const FRAME_MS = 1000 / PRESENTATION_MAX_FPS;
/** Draws one frame; true keeps the clock running without another invalidation. */
type Render = (now: number) => boolean;
interface Task {
  render: Render;
  dirty: boolean;
  active: boolean;
  visible: boolean;
  unobserve(): void;
}
export interface PresentationEnvironment {
  hidden(): boolean;
  now(): number;
  requestFrame(callback: FrameRequestCallback): number;
  cancelFrame(id: number): void;
  setTimer(callback: () => void, delayMs: number): number;
  clearTimer(id: number): void;
  observe(element: Element, visible: (value: boolean) => void): () => void;
  onVisibilityChange(callback: () => void): () => void;
}
export interface PresentationHandle {
  invalidate(): void;
  destroy(): void;
}
/** One visibility-aware frame clock shared by instrument renderers. */
export class PresentationScheduler {
  #tasks = new Set<Task>();
  #raf = 0;
  #timer = 0;
  #lastFrame = -Infinity;
  #environment: PresentationEnvironment;
  constructor(environment = browserEnvironment()) {
    this.#environment = environment;
    // The scheduler outlives every task, so the unsubscribe is never needed.
    environment.onVisibilityChange(this.#onVisibility);
  }
  register(element: Element, render: Render): PresentationHandle {
    const task: Task = {
      render,
      dirty: true,
      active: false,
      visible: true,
      unobserve: () => {},
    };
    task.unobserve = this.#environment.observe(element, (visible) => {
      task.visible = visible;
      if (visible) task.dirty = true;
      else this.#cancelIfIdle();
      this.#request();
    });
    this.#tasks.add(task);
    this.#request();
    return {
      invalidate: () => {
        task.dirty = true;
        this.#request();
      },
      destroy: () => {
        task.unobserve();
        this.#tasks.delete(task);
        if (!this.#tasks.size) this.#cancel();
      },
    };
  }
  #onVisibility = (): void => {
    if (this.#environment.hidden()) {
      this.#cancel();
      return;
    }
    for (const task of this.#tasks) task.dirty = true;
    this.#request();
  };
  #request(): void {
    if (this.#raf || this.#timer || this.#environment.hidden()) return;
    if (!this.#hasPending()) return;
    const delay = FRAME_MS - (this.#environment.now() - this.#lastFrame);
    if (delay > 1) {
      // Wake half a frame early so the requested frame lands on the budgeted slot.
      this.#timer = this.#environment.setTimer(
        () => {
          this.#timer = 0;
          this.#raf = this.#environment.requestFrame(this.#frame);
        },
        Math.max(1, delay - FRAME_MS / 2),
      );
    } else {
      this.#raf = this.#environment.requestFrame(this.#frame);
    }
  }
  #frame = (now: number): void => {
    this.#raf = 0;
    const cappedFrameDue = now - this.#lastFrame >= FRAME_MS - 1;
    if (!cappedFrameDue) {
      this.#request();
      return;
    }
    this.#lastFrame = now;
    for (const task of this.#tasks) {
      if (!task.visible || (!task.dirty && !task.active)) continue;
      task.dirty = false;
      task.active = task.render(now);
    }
    this.#request();
  };
  #hasPending(): boolean {
    for (const task of this.#tasks)
      if (task.visible && (task.dirty || task.active)) return true;
    return false;
  }
  #cancelIfIdle(): void {
    if (this.#environment.hidden()) {
      this.#cancel();
      return;
    }
    if (!this.#hasPending()) this.#cancel();
  }
  #cancel(): void {
    if (this.#raf) this.#environment.cancelFrame(this.#raf);
    if (this.#timer) this.#environment.clearTimer(this.#timer);
    this.#raf = 0;
    this.#timer = 0;
  }
}
function browserEnvironment(): PresentationEnvironment {
  return {
    hidden: () => typeof document !== "undefined" && document.hidden,
    now: () => performance.now(),
    requestFrame: (callback) =>
      typeof requestAnimationFrame === "undefined"
        ? 0
        : requestAnimationFrame(callback),
    cancelFrame: (id) => cancelAnimationFrame(id),
    setTimer: (callback, delayMs) => window.setTimeout(callback, delayMs),
    clearTimer: (id) => clearTimeout(id),
    observe: (element, visible) => {
      if (typeof IntersectionObserver === "undefined") return () => {};
      const observer = new IntersectionObserver(([entry]) =>
        visible(entry.isIntersecting),
      );
      observer.observe(element);
      return () => observer.disconnect();
    },
    onVisibilityChange: (callback) => {
      if (typeof document === "undefined") return () => {};
      document.addEventListener("visibilitychange", callback);
      return () => document.removeEventListener("visibilitychange", callback);
    },
  };
}
export const presentation = new PresentationScheduler();
