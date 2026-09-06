<script lang="ts">
  import { tooltip } from "../actions/tooltip";
  import type { ServerIdentity } from "../servers/catalog";
  import { store } from "../state/store.svelte";
  import { serverAccent, serverLabel } from "../presentation/serverAppearance";
  let {
    servers,
    value,
    onchange,
    label,
    aggregate = false,
    disabled = false,
    disabledIds = [],
    aggregateLabel = "All servers",
    aggregateDescription = "Combined speed",
  }: {
    servers: readonly ServerIdentity[];
    value: string;
    onchange: (id: string) => void;
    label: string;
    aggregate?: boolean;
    disabled?: boolean;
    disabledIds?: readonly string[];
    aggregateLabel?: string;
    aggregateDescription?: string;
  } = $props();
  const items = $derived([
    ...(aggregate
      ? [
          {
            id: "",
            text: aggregateLabel,
            description: aggregateDescription,
            accent: "var(--text-soft)",
          },
        ]
      : []),
    ...servers.map((server) => ({
      id: server.id,
      text: serverLabel(server),
      accent: serverAccent(server, store.serverCatalog?.servers ?? servers),
      description: [server.name, server.location, new URL(server.url).host]
        .filter(Boolean)
        .join("\n"),
    })),
  ]);
  const tabId = $derived(
    items.find((item) => !disabledIds.includes(item.id) && value === item.id)
      ?.id ?? items.find((item) => !disabledIds.includes(item.id))?.id,
  );
  const layoutKey = $derived(
    JSON.stringify(items.map(({ id, text }) => [id, text])),
  );
  let suppressClick = false;
  function positionSelector(element: HTMLDivElement) {
    const buttons = Array.from(element.querySelectorAll("button"));
    const selected = buttons.find((button) => button.dataset.id === value);
    const thumb = element.querySelector<HTMLElement>(".selector-thumb")!;
    const place = (button: HTMLButtonElement | undefined) => {
      thumb.hidden = !button;
      if (!button) return;
      const outer = element.getBoundingClientRect();
      const box = button.getBoundingClientRect();
      thumb.style.transform = `translate(${box.left - outer.left - element.clientLeft}px, ${box.top - outer.top - element.clientTop}px)`;
      thumb.style.width = `${box.width}px`;
      thumb.style.height = `${box.height}px`;
    };
    let drag: {
      pointer: number;
      x: number;
      y: number;
      moved: boolean;
      target: HTMLButtonElement | undefined;
    } | null = null;
    const update = () => {
      if (!drag) place(selected);
    };
    const cancel = () => {
      if (drag && element.hasPointerCapture(drag.pointer))
        element.releasePointerCapture(drag.pointer);
      drag = null;
      element.removeAttribute("data-dragging");
      place(selected);
    };
    const down = (event: PointerEvent) => {
      if (disabled || event.button !== 0 || !event.isPrimary) return;
      const button = (event.target as Element).closest("button");
      if (!button || button.disabled) return;
      drag = {
        pointer: event.pointerId,
        x: event.clientX,
        y: event.clientY,
        moved: false,
        target: button,
      };
    };
    const move = (event: PointerEvent) => {
      if (!drag || drag.pointer !== event.pointerId) return;
      if (event.buttons === 0) {
        cancel();
        return;
      }
      if (
        !drag.moved &&
        Math.hypot(event.clientX - drag.x, event.clientY - drag.y) < 6
      )
        return;
      drag.moved = true;
      element.setPointerCapture(event.pointerId);
      element.dataset.dragging = "true";
      drag.target = buttons.find((button) => {
        const box = button.getBoundingClientRect();
        return (
          !button.disabled &&
          event.clientX >= box.left &&
          event.clientX <= box.right &&
          event.clientY >= box.top &&
          event.clientY <= box.bottom
        );
      });
      place(drag.target ?? selected);
    };
    let clickTimer = 0;
    const up = (event: PointerEvent) => {
      if (!drag || drag.pointer !== event.pointerId) return;
      const { moved, target } = drag;
      cancel();
      if (!moved) return;
      suppressClick = true;
      clickTimer = window.setTimeout(() => {
        suppressClick = false;
      }, 0);
      if (target) {
        target.focus({ preventScroll: true });
        onchange(target.dataset.id!);
      }
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || !drag) return;
      event.preventDefault();
      event.stopPropagation();
      cancel();
    };
    const resize = new ResizeObserver(update);
    resize.observe(element);
    buttons.forEach((button) => resize.observe(button));
    update();
    element.addEventListener("pointerdown", down);
    element.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    element.addEventListener("pointercancel", cancel);
    element.addEventListener("lostpointercapture", cancel);
    element.addEventListener("keydown", escape, true);
    return () => {
      resize.disconnect();
      element.removeEventListener("pointerdown", down);
      element.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      element.removeEventListener("pointercancel", cancel);
      element.removeEventListener("lostpointercapture", cancel);
      element.removeEventListener("keydown", escape, true);
      if (drag) cancel();
      // The click after pointerup may arrive after a selection rerenders.
      if (!suppressClick) window.clearTimeout(clickTimer);
    };
  }
  function navigate(event: KeyboardEvent, index: number) {
    if (disabled) return;
    const delta =
      event.key === "ArrowRight" || event.key === "ArrowDown"
        ? 1
        : event.key === "ArrowLeft" || event.key === "ArrowUp"
          ? -1
          : 0;
    if (!delta && event.key !== "Home" && event.key !== "End") return;
    event.preventDefault();
    event.stopPropagation();
    const enabled = items
      .map((item, index) => ({ item, index }))
      .filter(({ item }) => !disabledIds.includes(item.id));
    if (!enabled.length) return;
    const current = enabled.findIndex((item) => item.index === index);
    const next =
      enabled[
        event.key === "Home"
          ? 0
          : event.key === "End"
            ? enabled.length - 1
            : (current + delta + enabled.length) % enabled.length
      ].index;
    onchange(items[next].id);
    const buttons = (
      event.currentTarget as HTMLElement
    ).parentElement?.querySelectorAll<HTMLButtonElement>("button");
    buttons?.[next].focus();
  }
</script>

<div
  class="server-selector"
  role="radiogroup"
  aria-label={label}
  {@attach (element) => {
    layoutKey;
    return positionSelector(element);
  }}
>
  <span class="selector-thumb" aria-hidden="true"></span>
  {#each items as item, index (item.id)}
    <button
      type="button"
      role="radio"
      aria-checked={value === item.id}
      aria-label={item.id === ""
        ? `${aggregateLabel}, ${aggregateDescription}`
        : item.description.replaceAll("\n", ", ")}
      tabindex={item.id === tabId ? 0 : -1}
      disabled={disabled || disabledIds.includes(item.id)}
      class:chosen={value === item.id}
      data-id={item.id}
      style:--server-accent={item.accent}
      use:tooltip={item.description}
      onclick={() => {
        if (!suppressClick) onchange(item.id);
      }}
      onkeydown={(event) => navigate(event, index)}
    >
      {#if item.id}<span class="server-dot" aria-hidden="true"></span>{/if}
      <span class="server-name">{item.text}</span>
    </button>
  {/each}
</div>

<style>
  .server-selector {
    position: relative;
    isolation: isolate;
    display: inline-flex;
    flex-wrap: nowrap;
    align-items: stretch;
    justify-content: center;
    gap: 2px;
    padding: 3px;
    border: 1px solid var(--border);
    border-radius: calc(var(--r-well) + 4px);
    background: var(--surface-inset);
    width: var(--selector-width, fit-content);
    min-width: 0;
    max-width: 100%;
    touch-action: pan-y;
    user-select: none;
  }
  .selector-thumb {
    position: absolute;
    inset: 0 auto auto 0;
    z-index: -1;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-well);
    background: var(--surface-1);
    pointer-events: none;
    transition:
      transform 220ms var(--ease-out),
      width 220ms var(--ease-out),
      height 220ms var(--ease-out);
  }
  .server-selector:global([data-dragging]) .selector-thumb {
    transition-duration: 100ms;
  }
  button {
    position: relative;
    display: inline-flex;
    flex: 1 1 0;
    align-items: center;
    justify-content: center;
    min-width: 0;
    min-height: 32px;
    gap: 6px;
    padding: 6px 8px;
    border: 0;
    border-radius: var(--r-well);
    background: transparent;
    color: var(--text-muted);
    font: 600 var(--type-xs)/1.3 var(--font-sans);
    text-align: center;
    cursor: pointer;
    transition:
      background 160ms ease,
      border-color 160ms ease,
      color 160ms ease;
  }
  button:not(.chosen):hover:not(:disabled) {
    color: var(--text);
  }
  button:active:not(:disabled) .server-name {
    transform: translateY(1px);
  }
  button.chosen {
    color: var(--text);
  }
  button:focus-visible {
    outline: var(--focus-ring);
    outline-offset: -3px;
  }
  button:disabled {
    color: var(--text-soft);
    cursor: default;
  }
  button:not(.chosen):disabled .server-name {
    opacity: 0.55;
  }
  .server-name {
    min-width: 0;
    max-width: 18ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    transition: transform 120ms ease;
  }
  .server-dot {
    width: 5px;
    height: 5px;
    flex-shrink: 0;
    border-radius: 50%;
    background: var(--server-accent);
  }
  @media (prefers-reduced-motion: reduce) {
    button,
    .server-name,
    .server-selector .selector-thumb {
      transition: none;
    }
    button:active:not(:disabled) .server-name {
      transform: none;
    }
  }
</style>
