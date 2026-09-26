import { untrack } from "svelte";

// Screen readers read appended nodes in order, so one region is the queue.
const KEPT = 3;
let next = 0;
export const announcements = $state<{ id: number; text: string }[]>([]);

export function announce(text: string) {
  if (!text) return;
  untrack(() => {
    announcements.push({ id: next++, text });
    if (announcements.length > KEPT) announcements.shift();
  });
}

/** Announces each later change of `text`; the value at mount is already on screen. */
export function announceChanges(text: () => string) {
  let previous: string | undefined;
  $effect(() => {
    const current = text();
    if (previous !== undefined && current !== previous) announce(current);
    previous = current;
  });
}
