import type { Attachment } from "svelte/attachments";
import { browserEnvironment } from "../canvas/presentation";

/** Reports whether the element is on screen in a visible tab. */
export function inView(report: (seen: boolean) => void): Attachment {
  return (node) => {
    const environment = browserEnvironment();
    let intersecting = false;
    const update = () => report(intersecting && !environment.hidden());
    const unobserve = environment.observe(node, (visible) => {
      intersecting = visible;
      update();
    });
    const unlisten = environment.onVisibilityChange(update);
    return () => {
      unobserve();
      unlisten();
    };
  };
}
