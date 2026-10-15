// Rows that unfold from their own height as they arrive and fold away before they leave. Content shown when the
// panel is built, or while it is out of sight, lands at once.
import { clear, insert, type Child } from "../../lib/jsx-runtime";
import { reducedMotion } from "../../lib/motion";
import { computed, effect, onCleanup, scope, untrack } from "../../lib/signal";

const UNFOLD_MS = 180;

/** A row from nothing to its own height, padding and edge; it shows by halfway. */
function frames(row: HTMLElement): Keyframe[] {
  const style = getComputedStyle(row);
  const folded = { height: "0px", minHeight: "0px", paddingTop: "0px", paddingBottom: "0px", borderTopWidth: "0px" };
  return [
    { ...folded, opacity: 0, overflow: "hidden" },
    { opacity: 1, offset: 0.5 },
    {
      height: style.height,
      minHeight: style.minHeight,
      paddingTop: style.paddingTop,
      paddingBottom: style.paddingBottom,
      borderTopWidth: style.borderTopWidth,
      opacity: 1,
      overflow: "hidden",
    },
  ];
}

export function Unfold(props: { when: () => boolean; children: () => Child }): Node {
  const fragment = document.createDocumentFragment();
  const start = fragment.appendChild(document.createComment(""));
  const end = fragment.appendChild(document.createComment(""));
  let content: { dispose: () => void; motion: Animation[] } | null = null;
  let open = false;
  let mounted = false;

  const rows = () => {
    const found: HTMLElement[] = [];
    for (let node = start.nextSibling; node && node !== end; node = node.nextSibling)
      if (node instanceof HTMLElement) found.push(node);
    return found;
  };
  const remove = () => {
    content?.dispose();
    content = null;
    clear(start, end);
  };
  // A cancelled animation lingers with the timeline until its next frame; without its effect it holds no row.
  const stop = () => {
    for (const animation of content?.motion ?? []) {
      animation.cancel();
      animation.effect = null;
    }
    if (content) content.motion = [];
  };
  // The motion ends as one; what is wanted by then stays.
  const settle = () => {
    stop();
    if (!open) remove();
  };
  const play = (direction: PlaybackDirection) => {
    const easing = getComputedStyle(start.parentElement!).getPropertyValue("--ease-out").trim() || "ease-out";
    content!.motion = rows().map((row) =>
      row.animate(frames(row), { duration: UNFOLD_MS, easing, fill: "both", direction }),
    );
    if (content!.motion[0]) content!.motion[0].onfinish = settle;
    else settle();
  };

  const wanted = computed(() => !!props.when());
  effect(() => {
    open = wanted();
    untrack(() => {
      const moving = mounted && !reducedMotion.peek() && start.parentElement?.checkVisibility() === true;
      mounted = true;
      if (content?.motion.length) {
        for (const animation of content.motion) animation.reverse();
        return;
      }
      if (!open) {
        if (content && moving) play("reverse");
        else remove();
        return;
      }
      if (content) return;
      const built = document.createDocumentFragment();
      const [, dispose] = scope(() => insert(built, props.children()));
      content = { dispose, motion: [] };
      end.before(built);
      if (moving) play("normal");
    });
  });
  onCleanup(() => {
    stop();
    remove();
  });
  return fragment;
}
