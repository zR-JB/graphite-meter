let locks = 0;
let saved:
  { scrollY: number; bodyCss: string; rootOverscroll: string } | undefined;

// Sheets and dialogs can overlap or change layout while another layer is open.
export function acquirePageScrollLock(): () => void {
  if (++locks === 1) {
    const body = document.body;
    saved = {
      scrollY: window.scrollY,
      bodyCss: body.style.cssText,
      rootOverscroll: document.documentElement.style.overscrollBehavior,
    };
    document.documentElement.style.overscrollBehavior = "none";
    // iOS can scroll behind an overlay with overflow:hidden alone.
    Object.assign(body.style, {
      position: "fixed",
      top: `-${saved.scrollY}px`,
      left: "0",
      right: "0",
      width: "100%",
      overflow: "hidden",
      overscrollBehavior: "none",
    });
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    if (--locks || !saved) return;
    const restore = saved;
    saved = undefined;
    document.body.style.cssText = restore.bodyCss;
    document.documentElement.style.overscrollBehavior = restore.rootOverscroll;
    window.scrollTo(0, restore.scrollY);
  };
}
