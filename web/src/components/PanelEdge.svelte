<script lang="ts">
  import Icon from './Icon.svelte';
  import { PANEL } from '../lib/store.svelte';

  /** The inner edge of a side panel of the library (desktop): drag it to resize the
   * panel, double-click it for the default width; the tab in its middle hides the panel,
   * and brings it back from the window's edge. Sits between the panel and the photos as
   * a zero-width flex item. */
  let {
    side,
    open,
    width,
    min,
    max,
    def,
    maxVw,
    label,
    hint = '',
    onresize,
    ondone,
    ontoggle,
  }: {
    /** 'left': the right edge of a panel on the left; 'right': the left edge of one on the right. */
    side: 'left' | 'right';
    open: boolean;
    /** The width asked for (px); the panel may be drawn narrower in a small window. */
    width: number;
    min: number;
    max: number;
    /** The default width (double-click). */
    def: number;
    /** The widest the panel is drawn, in % of the window (the CSS clamp). */
    maxVw: number;
    /** 'sidebar', 'info panel'. */
    label: string;
    /** The shortcut, as store.keyHint gives it: " (F7)". */
    hint?: string;
    onresize: (w: number) => void;
    ondone: () => void;
    ontoggle: () => void;
  } = $props();

  let el: HTMLDivElement | undefined = $state();
  let dragging = $state(false);
  let startX = 0;
  let startW = 0;
  let limit = 0;

  /** The panel and the photos next to this edge. */
  function neighbours(): { panel: Element | null; content: Element | null } {
    const before = el?.previousElementSibling ?? null;
    const after = el?.nextElementSibling ?? null;
    return side === 'left' ? { panel: before, content: after } : { panel: after, content: before };
  }

  /** The widest the panel can go now: its own limit, the window's, and the photos keep
   * PANEL.content px. */
  function widest(drawn: number): number {
    const { content } = neighbours();
    const room = content ? drawn + content.getBoundingClientRect().width - PANEL.content : max;
    return Math.max(min, Math.min(max, Math.floor((window.innerWidth * maxVw) / 100), Math.max(drawn, room)));
  }

  const clamp = (w: number, hi: number) => Math.round(Math.max(min, Math.min(hi, w)));

  function down(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    const drawn = neighbours().panel?.getBoundingClientRect().width ?? width;
    startX = e.clientX;
    startW = drawn;
    limit = widest(drawn);
    dragging = true;
    document.documentElement.classList.add('col-resizing');
  }

  function move(e: PointerEvent) {
    if (!dragging) return;
    const dx = e.clientX - startX;
    const w = clamp(side === 'left' ? startW + dx : startW - dx, limit);
    if (w !== width) onresize(w);
  }

  function up() {
    if (!dragging) return;
    dragging = false;
    document.documentElement.classList.remove('col-resizing');
    ondone();
  }

  function reset() {
    onresize(def);
    ondone();
  }

  /** Arrow keys on the focused edge: 16 px a press, 64 with Shift. */
  function key(e: KeyboardEvent) {
    const step = e.shiftKey ? 64 : 16;
    const drawn = neighbours().panel?.getBoundingClientRect().width ?? width;
    let w: number;
    if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
      const grow = (e.key === 'ArrowRight') === (side === 'left');
      w = drawn + (grow ? step : -step);
    } else if (e.key === 'Home') w = min;
    else if (e.key === 'End') w = max;
    else return;
    // The library's own keys (next photo...) stay out of it.
    e.preventDefault();
    e.stopPropagation();
    onresize(clamp(w, widest(drawn)));
    ondone();
  }
</script>

<div class="edge {side}" class:open class:dragging bind:this={el}>
  {#if open}
    <!-- A focusable separator is a window splitter, an interactive widget (WAI-ARIA). -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
    <div
      class="grip"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the {label}: drag, or use the arrow keys; double-click for the default width"
      aria-valuenow={width}
      aria-valuemin={min}
      aria-valuemax={max}
      tabindex="0"
      onpointerdown={down}
      onpointermove={move}
      onpointerup={up}
      onpointercancel={up}
      onlostpointercapture={up}
      ondblclick={reset}
      onkeydown={key}
    ></div>
  {/if}
  <button class="tab" onclick={ontoggle} title="{open ? 'Hide' : 'Show'} the {label}{hint}" aria-label="{open ? 'Hide' : 'Show'} the {label}" aria-expanded={open}>
    <Icon name="chevron" size={12} stroke={2.5} />
  </button>
</div>

<style>
  .edge {
    position: relative;
    flex: none;
    width: 0;
    z-index: 12;
  }
  /* The handle: a strip over the border, which lights up while it can be dragged. */
  .grip {
    position: absolute;
    top: 0;
    bottom: 0;
    left: -0.3125rem;
    width: 0.625rem;
    cursor: col-resize;
    touch-action: none;
    outline: none;
  }
  .grip::after {
    content: '';
    position: absolute;
    top: 0;
    bottom: 0;
    left: calc(50% - 1px);
    width: 2px;
    border-radius: 2px;
    background: var(--accent);
    opacity: 0;
    transition: opacity var(--dur) var(--ease-out);
  }
  .grip:hover::after,
  .grip:focus-visible::after,
  .dragging .grip::after {
    opacity: 0.85;
    transition-delay: 0.12s;
  }
  .dragging .grip::after {
    opacity: 1;
    transition-delay: 0s;
  }
  @media (pointer: coarse) {
    .grip {
      left: -0.625rem;
      width: 1.25rem;
    }
  }
  /* The tab: a small pill in the middle of the edge; against the window's edge when the
     panel is hidden. */
  .tab {
    position: absolute;
    top: 50%;
    left: 0;
    width: 1rem;
    height: 2.75rem;
    margin-top: -1.375rem;
    translate: -50% 0;
    display: grid;
    place-items: center;
    padding: 0;
    border: 1px solid var(--border-strong);
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--text-3);
    box-shadow: 0 2px 8px -3px rgba(0, 0, 0, 0.45);
    cursor: pointer;
    opacity: 0.8;
    transition:
      opacity var(--dur-fast),
      color var(--dur-fast),
      background-color var(--dur-fast),
      border-color var(--dur-fast),
      width var(--dur) var(--ease-out),
      scale var(--dur) var(--ease-spring);
  }
  .edge:hover .tab,
  .tab:focus-visible {
    opacity: 1;
  }
  .tab:hover {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 16%, var(--surface-2));
    border-color: color-mix(in srgb, var(--accent) 50%, transparent);
    scale: 1.08;
  }
  .tab:active {
    scale: 0.94;
  }
  .tab :global(svg) {
    transition: transform var(--dur) var(--ease-out);
  }
  /* The chevron points the way the panel goes: in towards its side to hide, out to show. */
  .left.open .tab :global(svg),
  .right:not(.open) .tab :global(svg) {
    transform: rotate(180deg);
  }
  /* Hidden panel: the tab fits in the grid's margin, and stays fully visible. */
  .left:not(.open) .tab {
    translate: 0 0;
    width: 0.75rem;
    border-left: 0;
    border-radius: 0 999px 999px 0;
    opacity: 1;
  }
  .right:not(.open) .tab {
    translate: -100% 0;
    width: 0.75rem;
    border-right: 0;
    border-radius: 999px 0 0 999px;
    opacity: 1;
  }
  .left:not(.open) .tab:hover,
  .right:not(.open) .tab:hover {
    width: 1.125rem;
    scale: 1;
  }
  .dragging .tab {
    opacity: 0;
    pointer-events: none;
  }
</style>
