// Feeds screen readers one polite region, read in order, with the last three messages kept.
import { For } from "../../lib/dom";
import { effect, signal, untrack } from "../../lib/signal";

const KEPT = 3;
let next = 0;
const messages = signal<{ id: number; text: string }[]>([]);

export function announce(text: string): void {
  if (text) messages.set([...messages.peek(), { id: next++, text }].slice(-KEPT));
}

/** Announces each later change of `text`. What it says at first is already on screen. */
export function announceChanges(text: () => string): void {
  let previous: string | undefined;
  effect(() => {
    const current = text();
    if (previous !== undefined && current !== previous) untrack(() => announce(current));
    previous = current;
  });
}

/** Renders the page's region. A modal dialog renders its own, since a modal hides the page's from the reader. */
export const Announcements = () => (
  <div class="sr-only" aria-live="polite">
    <For each={messages} by={(message) => message.id}>
      {(message) => <p>{message().text}</p>}
    </For>
  </div>
);
