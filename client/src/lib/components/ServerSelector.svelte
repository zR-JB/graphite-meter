<script lang="ts">
  import type { ServerIdentity } from "../servers/catalog";
  import { serverLabel } from "../presentation/serverAppearance";

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
  const descriptionId = $props.id();
  let trigger: HTMLButtonElement;
  let menu: HTMLDivElement;
  let open = $state(false);
  let active = $state(-1);
  let search = "";
  let searchedAt = 0;
  const options = $derived([
    ...(aggregate ? [{ id: "", name: aggregateLabel }] : []),
    ...servers.map((server) => ({ id: server.id, name: serverLabel(server) })),
  ]);
  const enabled = $derived(
    options
      .map((_, index) => index)
      .filter((index) => !disabledIds.includes(options[index].id)),
  );

  function highlight(index: number) {
    active = index;
    menu.children[index]?.scrollIntoView({ block: "nearest" });
  }
  function show() {
    if (disabled || !enabled.length) return;
    active = enabled.find((index) => options[index].id === value) ?? enabled[0];
    menu.showPopover();
    trigger.focus({ preventScroll: true });
    open = true;
    highlight(active);
  }
  function choose(index: number) {
    const option = options[index];
    if (!option || disabledIds.includes(option.id)) return;
    menu.hidePopover();
    open = false;
    trigger.focus();
    onchange(option.id);
  }
  function keydown(event: KeyboardEvent) {
    open = menu.matches(":popover-open");
    if (event.key === "Tab") {
      menu.hidePopover();
      open = false;
      return;
    }
    if (event.key === "Escape") return; // Native popover dismisses without committing.
    if (["Enter", " "].includes(event.key)) {
      event.preventDefault();
      if (open) choose(active);
      else show();
      return;
    }
    if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
      event.preventDefault();
      const wasOpen = open;
      if (!open) show();
      if (!open) return;
      const offset = event.key === "ArrowUp" ? -1 : 1;
      const next =
        event.key === "Home"
          ? enabled[0]
          : event.key === "End"
            ? enabled.at(-1)!
            : wasOpen
              ? enabled[
                  (enabled.indexOf(active) + offset + enabled.length) %
                    enabled.length
                ]
              : active;
      highlight(next);
    } else if (
      event.key.length === 1 &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey
    ) {
      event.preventDefault();
      if (!open) show();
      const now = performance.now();
      search = now - searchedAt > 700 ? event.key : search + event.key;
      searchedAt = now;
      const query = /^(.)(\1)*$/u.test(search) ? event.key : search;
      const start = enabled.indexOf(active);
      const match = Array.from(
        { length: enabled.length },
        (_, offset) => enabled[(start + offset + 1) % enabled.length],
      ).find((index) =>
        options[index].name
          .toLocaleLowerCase()
          .startsWith(query.toLocaleLowerCase()),
      );
      if (match !== undefined) highlight(match);
    }
  }
  $effect(() => {
    if (!open) return;
    if (disabled || !enabled.length) {
      menu.hidePopover();
      return;
    }
    if (!enabled.includes(active))
      highlight(
        enabled.find((index) => options[index].id === value) ?? enabled[0],
      );
  });
</script>

<button
  bind:this={trigger}
  type="button"
  class="server-selector"
  role="combobox"
  aria-label={label}
  aria-haspopup="listbox"
  aria-controls={`${descriptionId}-list`}
  aria-expanded={open}
  aria-activedescendant={open && active >= 0
    ? `${descriptionId}-${active}`
    : undefined}
  aria-describedby={aggregate && value === "" ? descriptionId : undefined}
  {value}
  {disabled}
  popovertarget={`${descriptionId}-list`}
  style:anchor-name={`--${descriptionId}`}
  onclick={(event) => {
    event.preventDefault();
    if (menu.matches(":popover-open")) {
      menu.hidePopover();
      open = false;
    } else show();
  }}
  onkeydown={keydown}
>
  <span>{options.find((option) => option.id === value)?.name ?? label}</span>
  <svg aria-hidden="true" viewBox="0 0 16 16"><path d="m4 6 4 4 4-4" /></svg>
</button>
<div
  bind:this={menu}
  id={`${descriptionId}-list`}
  class="float popover menu server-menu"
  popover="auto"
  style:position-anchor={`--${descriptionId}`}
  role="listbox"
  aria-label={label}
  ontoggle={(event) => {
    open = event.currentTarget.matches(":popover-open");
  }}
>
  {#each options as option, index (option.id)}
    <button
      type="button"
      role="option"
      id={`${descriptionId}-${index}`}
      tabindex="-1"
      aria-selected={option.id === value}
      disabled={disabledIds.includes(option.id)}
      class:active={active === index}
      onpointerdown={(event) => event.preventDefault()}
      onclick={() => choose(index)}
    >
      <span
        >{option.name}{#if option.id === ""}<small>{aggregateDescription}</small
          >{/if}</span
      >
      <span class="selected" aria-hidden="true"
        >{option.id === value ? "✓" : ""}</span
      >
    </button>
  {/each}
</div>
{#if aggregate}<span id={descriptionId} hidden>{aggregateDescription}</span
  >{/if}

<style>
  .server-selector {
    display: inline-flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    width: var(--selector-width, auto);
    min-width: 0;
    max-width: 100%;
    height: var(--control-h);
    padding: 0 var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-well);
    background: var(--surface-inset);
    font: 600 var(--type-xs) / 1.3 var(--font-sans);
    transition: var(--transition-control);
  }
  @media (hover: hover) {
    .server-selector:hover:not(:disabled) {
      border-color: var(--border-strong);
    }
  }
  .server-selector:disabled {
    color: var(--text-soft);
  }
  .server-selector > span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.5;
  }
  .server-menu {
    min-width: 220px;
    min-width: max(anchor-size(width), 220px);
    max-height: 320px;
  }
  .server-menu > button {
    grid-template-columns: minmax(0, 1fr) 16px;
    min-height: var(--hit);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .server-menu > button > span:first-child {
    display: block;
    color: inherit;
  }
  small {
    display: block;
    margin-top: 2px;
    color: var(--text-muted);
    font-weight: 400;
  }
  .selected {
    color: var(--brand-strong);
  }
  @media (pointer: coarse) {
    .server-selector {
      height: var(--hit);
    }
  }
</style>
