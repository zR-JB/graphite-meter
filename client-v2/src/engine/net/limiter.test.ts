import { expect, test } from "bun:test";
import { createLimiter } from "./limiter";

test("checks run 2 per origin and 8 at once; background ones only on an idle origin, 4 at most", async () => {
  const limiter = createLimiter();
  const started: string[] = [];
  const releases = new Map<string, () => void>();
  const selected = new Set<string>();
  const take = (label: string, signal = new AbortController().signal) =>
    limiter
      .acquire(label.split("#")[0]!, () => (selected.has(label) ? 0 : 1), signal)
      .then((release) => {
        started.push(label);
        releases.set(label, release);
      })
      .catch((reason) => started.push(`${label} refused: ${reason}`));
  const flush = () => new Promise((resolve) => setImmediate(resolve));

  selected.add("a#3");
  for (const label of ["a#1", "a#2", "a#3", "b#1", "c#1", "d#1", "e#1"]) void take(label);
  const abort = new AbortController();
  void take("f#1", abort.signal);
  await flush();
  // a#2 waits for its origin; e#1 and f#1 for the background cap; the selected a#3 still starts.
  expect(started).toEqual(["a#1", "a#3", "b#1", "c#1", "d#1"]);
  abort.abort("closed");
  await flush();
  expect(started.at(-1)).toBe("f#1 refused: closed");
  // A release counts once, however often it is called.
  releases.get("a#1")!();
  releases.get("a#1")!();
  await flush();
  expect(started.slice(5)).toEqual(["f#1 refused: closed", "e#1"]);
  // Priorities are read again on every drain: a server selected meanwhile no longer waits for an idle origin.
  selected.add("a#2");
  limiter.drain();
  await flush();
  expect(started.at(-1)).toBe("a#2");
  for (const label of ["g#1", "g#2", "h#1", "h#2"]) {
    selected.add(label);
    void take(label);
  }
  await flush();
  // Eight run at once: a#2, a#3, b, c, d, e and two of the new ones.
  expect(started.slice(-2)).toEqual(["g#1", "g#2"]);
});
