<script lang="ts">
  import { tooltip } from "../actions/tooltip";
  import type { ServerIdentity } from "../servers/catalog";
  let {
    servers,
    value,
    onchange,
    label,
    aggregate = false,
    disabled = false,
    expanded = false,
    multiple = false,
    disabledIds = [],
    aggregateLabel = "All",
    aggregateDescription = "Aggregate throughput across the selected servers",
  }: {
    servers: readonly ServerIdentity[];
    value: string | readonly string[];
    onchange: (id: string) => void;
    label: string;
    aggregate?: boolean;
    disabled?: boolean;
    expanded?: boolean;
    multiple?: boolean;
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
          },
        ]
      : []),
    ...servers.map((server) => ({
      id: server.id,
      text: server.name,
      description: [server.name, server.location, new URL(server.url).host]
        .filter(Boolean)
        .join("\n"),
    })),
  ]);
  const tabId = $derived(
    items.find(
      (item) =>
        !disabledIds.includes(item.id) &&
        (typeof value === "string"
          ? value === item.id
          : value.includes(item.id)),
    )?.id ?? items.find((item) => !disabledIds.includes(item.id))?.id,
  );
  const selected = (id: string) =>
    typeof value === "string" ? value === id : value.includes(id);
  function navigate(event: KeyboardEvent, index: number) {
    if (multiple || disabled) return;
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
  class="server-pills"
  class:expanded
  role={multiple ? "group" : "radiogroup"}
  aria-label={label}
>
  {#each items as item, index (item.id)}
    <button
      type="button"
      role={multiple ? undefined : "radio"}
      aria-checked={multiple ? undefined : selected(item.id)}
      aria-pressed={multiple ? selected(item.id) : undefined}
      aria-label={item.id === ""
        ? `${aggregateLabel} servers, ${aggregateDescription}`
        : item.description.replaceAll("\n", ", ")}
      tabindex={multiple || item.id === tabId ? 0 : -1}
      disabled={disabled || disabledIds.includes(item.id)}
      class:chosen={selected(item.id)}
      use:tooltip={item.description}
      onclick={() => onchange(item.id)}
      onkeydown={(event) => navigate(event, index)}
    >
      <span class="selection-mark" aria-hidden="true"
        >{selected(item.id) ? "✓" : multiple ? "+" : ""}</span
      ><span class="server-name">{item.text}</span>
    </button>
  {/each}
</div>

<style>
  .server-pills {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: stretch;
    justify-content: center;
    width: fit-content;
    max-width: 100%;
    overflow: hidden;
    border: 1px solid var(--border-strong);
    border-radius: 16px;
    background: var(--surface-inset);
  }
  button {
    position: relative;
    display: inline-flex;
    flex: 1 1 auto;
    align-items: center;
    justify-content: center;
    min-width: 58px;
    min-height: 30px;
    padding: 5px 22px;
    border: 0;
    border-radius: 0;
    background: transparent;
    color: var(--text-muted);
    font: 600 var(--type-xs)/1.3 var(--font-sans);
    text-align: center;
    cursor: pointer;
    transition:
      background 160ms ease,
      color 160ms ease;
  }
  button + button {
    box-shadow: inset 1px 0 var(--border);
  }
  .server-pills button:not(.chosen):hover:not(:disabled) {
    color: var(--text);
    background: var(--surface-3);
  }
  button:active:not(:disabled) .server-name {
    transform: translateY(1px);
  }
  button.chosen {
    color: var(--text);
    background: color-mix(in srgb, var(--brand) 18%, var(--surface-1));
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
  .expanded {
    width: 100%;
  }
  .expanded button {
    min-height: 34px;
    max-width: 100%;
  }
  .server-name {
    max-width: 18ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    transition: transform 120ms ease;
  }
  .selection-mark {
    position: absolute;
    inset-inline-start: 8px;
    width: 10px;
    color: var(--text);
    font-weight: 700;
    transition:
      opacity 140ms ease,
      transform 140ms ease;
  }
  button:not(.chosen) .selection-mark {
    opacity: 0;
    transform: scale(0.7);
  }
  .expanded button:not(.chosen) .selection-mark {
    opacity: 0.55;
    transform: none;
  }
  @media (prefers-reduced-motion: reduce) {
    button,
    .server-name,
    .selection-mark {
      transition: none;
    }
    button:active:not(:disabled) .server-name {
      transform: none;
    }
  }
</style>
