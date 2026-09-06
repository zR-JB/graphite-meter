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
  } = $props();
  const items = $derived([
    ...(aggregate
      ? [
          {
            id: "",
            text: "All",
            description: "Aggregate throughput across the selected servers",
            location: "",
          },
        ]
      : []),
    ...servers.map((server, index) => ({
      id: server.id,
      text: expanded ? server.name : `${index + 1}`,
      description: [server.name, server.location, new URL(server.url).host]
        .filter(Boolean)
        .join("\n"),
      location: server.location ?? new URL(server.url).host,
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
        ? "All servers, aggregate throughput"
        : `${expanded ? "" : `Server ${item.text}: `}${item.description.replaceAll("\n", ", ")}`}
      tabindex={multiple || item.id === tabId ? 0 : -1}
      disabled={disabled || disabledIds.includes(item.id)}
      class:chosen={selected(item.id)}
      use:tooltip={item.description}
      onclick={() => onchange(item.id)}
      onkeydown={(event) => navigate(event, index)}
    >
      <span>{item.text}</span>{#if expanded && item.location}<small
          >{item.location}</small
        >{/if}
    </button>
  {/each}
</div>

<style>
  .server-pills {
    display: flex;
    flex-wrap: wrap;
    gap: 3px;
    padding: 3px;
    width: fit-content;
    max-width: 100%;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
  }
  button {
    display: grid;
    align-content: center;
    gap: 2px;
    min-width: 34px;
    min-height: 32px;
    padding: 4px 10px;
    border: 1px solid transparent;
    border-radius: calc(var(--r-chrome) - 2px);
    background: transparent;
    color: var(--text-muted);
    font: 600 var(--type-sm)/1.3 var(--font-sans);
    cursor: pointer;
  }
  button:hover:not(:disabled) {
    color: var(--text);
    background: var(--surface-1);
  }
  button.chosen {
    color: var(--brand-strong);
    background: var(--surface-2);
    border-color: var(--border-strong);
  }
  button:focus-visible {
    outline: 2px solid var(--brand);
    outline-offset: 1px;
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .expanded {
    width: 100%;
  }
  .expanded button {
    flex: 1 1 110px;
    text-align: left;
    min-height: 48px;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  small {
    color: var(--text-muted);
    font-size: var(--type-xs);
    font-weight: 400;
  }
</style>
