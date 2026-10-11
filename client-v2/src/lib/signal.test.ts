import { expect, spyOn, test } from "bun:test";
import { batch, computed, effect, onCleanup, scope, signal, untrack } from "./signal";

test("a diamond settles once, without a glitch", () => {
  const a = signal(1);
  const b = computed(() => a() * 2);
  const c = computed(() => a() + 1);
  const seen: string[] = [];
  effect(() => void seen.push(`${b()}/${c()}`));
  a.set(2);
  expect(seen).toEqual(["2/2", "4/3"]);
});

test("an unchanged computed stops the update, and a batch flushes once", () => {
  const a = signal(0);
  const b = signal(0);
  const positive = computed(() => a() > 0);
  let runs = 0;
  effect(() => void (positive(), runs++));
  a.set(-1);
  expect(runs).toBe(1);
  let sums = 0;
  effect(() => void (a() + b(), sums++));
  batch(() => {
    a.set(1);
    a.set(2);
    b.set(1);
  });
  expect([runs, sums]).toEqual([2, 2]);
});

test("computeds are lazy and untracked reads do not subscribe", () => {
  const a = signal(1);
  let computes = 0;
  const doubled = computed(() => (computes++, a() * 2));
  expect(computes).toBe(0);
  let runs = 0;
  effect(() => void (untrack(doubled), runs++));
  a.set(5);
  expect(runs).toBe(1);
  expect(doubled()).toBe(10);
});

test("switching to a computed another computed read keeps it subscribed", () => {
  const s = signal(1);
  const mode = signal("d");
  const c = computed(() => s() * 10);
  const d = computed(() => c() + 1);
  const seen: number[] = [];
  effect(() => void seen.push(mode() === "d" ? d() : c()));
  mode.set("c");
  s.set(2);
  expect(seen).toEqual([11, 10, 20]);
});

test("an owner reruns before the effects it owns, which it disposes", () => {
  const user = signal<{ name: string } | null>({ name: "a" });
  const seen: string[] = [];
  effect(() => {
    if (user()) effect(() => void seen.push(user()!.name));
  });
  user.set({ name: "b" });
  user.set(null);
  expect(seen).toEqual(["a", "b"]);
});

test("an effect that throws is reported, stays alive, and never stops the others", () => {
  const reported = spyOn(globalThis, "reportError").mockImplementation(() => {});
  const a = signal(0);
  const seen: number[] = [];
  effect(() => {
    if (a() === 1) throw new Error("boom");
    seen.push(a());
  });
  effect(() => void seen.push(-a()));
  a.set(1);
  a.set(2);
  expect(seen).toEqual([0, -0, -1, 2, -2]);
  expect(reported).toHaveBeenCalledTimes(1);
  reported.mockRestore();
});

test("a write an effect causes to what it read runs it again with the new value", () => {
  const count = signal(0);
  const seen: number[] = [];
  effect(() => {
    const value = count();
    seen.push(value);
    if (value < 3) count.set(value + 1);
  });
  expect([seen, count.peek()]).toEqual([[0, 1, 2, 3], 3]);
});

test("an effect disposed while it runs lets go of its sources and of what it creates afterwards", () => {
  const a = signal(0);
  const log: string[] = [];
  let stop = () => {};
  stop = effect(() => {
    a();
    if (a() === 1) {
      stop();
      effect(() => void log.push("child"));
      onCleanup(() => log.push("cleanup"));
    }
    log.push(`run ${a()}`);
  });
  a.set(1);
  a.set(2);
  expect(log).toEqual(["run 0", "cleanup", "run 1"]);
});

test("an effect disposes what it created before it reruns, and a scope disposes everything", () => {
  const outer = signal(0);
  const inner = signal(0);
  const log: string[] = [];
  const [, dispose] = scope(() =>
    effect(() => {
      const round = outer();
      effect(() => void log.push(`inner ${round}:${inner()}`));
      onCleanup(() => log.push(`cleanup ${round}`));
    }),
  );
  outer.set(1);
  inner.set(1);
  dispose();
  inner.set(2);
  expect(log).toEqual(["inner 0:0", "cleanup 0", "inner 1:0", "inner 1:1", "cleanup 1"]);
});
