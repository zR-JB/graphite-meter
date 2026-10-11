import { expect, test } from "bun:test";
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
  const positive = computed(() => a() > 0);
  let runs = 0;
  effect(() => void (positive(), runs++));
  a.set(-1);
  expect(runs).toBe(1);
  batch(() => {
    a.set(1);
    a.set(2);
  });
  expect(runs).toBe(2);
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
