import type { Attachment } from "svelte/attachments";

/** Reports whether the node is on screen in a visible page. */
export function inView(report: (seen: boolean) => void): Attachment {
  return (node) => {
    let intersecting = false;
    const update = () => report(intersecting && !document.hidden);
    const observer = new IntersectionObserver(([entry]) => {
      intersecting = entry.isIntersecting;
      update();
    });
    observer.observe(node);
    document.addEventListener("visibilitychange", update);
    return () => {
      observer.disconnect();
      document.removeEventListener("visibilitychange", update);
    };
  };
}
