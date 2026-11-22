<script lang="ts">
  // The one context menu (lib/menu.svelte.ts): at the pointer, kept inside the window,
  // above everything. Its entries are rebuilt whenever what they read changes, so
  // "Move up" can be pressed again and again while the element moves behind it.
  import { tick } from 'svelte';
  import { menu, swallowClick, type MenuEntry } from '../lib/menu.svelte';
  import Icon from './Icon.svelte';

  let el: HTMLDivElement | undefined = $state();
  let pos = $state<{ left: number; top: number } | null>(null);

  const entries = $derived<MenuEntry[]>(menu.build ? menu.build() : []);

  // Place it once per opening: at the pointer, flipped to stay on screen.
  $effect(() => {
    const at = menu.at;
    pos = null;
    if (!at) return;
    tick().then(() => {
      if (!el || menu.at !== at) return;
      const r = el.getBoundingClientRect();
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      let left: number;
      let top: number;
      if (at.row) {
        // Under the row (above it when there is no room), from near its start.
        left = Math.max(8, Math.min(at.row.left + 12, vw - r.width - 8));
        top = at.row.bottom + 4;
        if (top + r.height > vh - 8) top = at.row.top - r.height - 4;
        top = Math.max(8, Math.min(top, vh - r.height - 8));
      } else {
        left = at.x;
        if (left + r.width > vw - 8) left = Math.max(8, at.x - r.width);
        top = at.y;
        if (top + r.height > vh - 8) top = Math.max(8, Math.min(at.y - r.height, vh - r.height - 8));
      }
      pos = { left, top };
      el.focus({ preventScroll: true });
    });
  });

  // While it is open: a press elsewhere, Esc, the wheel outside it or a resize close it,
  // and the keyboard moves through it (and nothing else hears the keys). Scroll events
  // alone do not: a panel still settling after a scroll into view would close it.
  $effect(() => {
    if (!menu.at) return;
    // A press elsewhere only closes it: what is under it is not clicked as well.
    const outside = (e: PointerEvent) => {
      if (el?.contains(e.target as Node)) return;
      menu.close();
      if (e.button === 0) swallowClick();
    };
    const key = (e: KeyboardEvent) => {
      e.stopPropagation();
      if (e.key === 'Escape' || e.key === 'Tab') {
        e.preventDefault();
        menu.close();
        return;
      }
      const items = [...(el?.querySelectorAll<HTMLButtonElement>('button.menu-item:not(:disabled)') ?? [])];
      if (!items.length) return;
      const i = items.indexOf(document.activeElement as HTMLButtonElement);
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault();
        const n = items.length;
        const down = e.key === 'ArrowDown';
        items[i < 0 ? (down ? 0 : n - 1) : (i + (down ? 1 : -1) + n) % n].focus();
      } else if (e.key === 'Home' || e.key === 'End') {
        e.preventDefault();
        items[e.key === 'Home' ? 0 : items.length - 1].focus();
      } else if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        if (i >= 0) items[i].click();
      }
    };
    const wheel = (e: Event) => {
      if (!el?.contains(e.target as Node)) menu.close();
    };
    const away = () => menu.close();
    window.addEventListener('pointerdown', outside, true);
    window.addEventListener('keydown', key, true);
    window.addEventListener('wheel', wheel, { capture: true, passive: true });
    window.addEventListener('resize', away);
    window.addEventListener('blur', away);
    return () => {
      window.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('keydown', key, true);
      window.removeEventListener('wheel', wheel, { capture: true });
      window.removeEventListener('resize', away);
      window.removeEventListener('blur', away);
    };
  });

  function run(e: Extract<MenuEntry, { kind: 'item' }>) {
    if (e.disabled) return;
    e.run();
    if (!e.stay) menu.close();
  }
</script>

{#if menu.at}
  <div
    bind:this={el}
    class="pop ctxmenu"
    role="menu"
    tabindex="-1"
    style:left={`${pos?.left ?? 0}px`}
    style:top={`${pos?.top ?? 0}px`}
    style:visibility={pos ? 'visible' : 'hidden'}
    oncontextmenu={(e) => e.preventDefault()}
  >
    {#each entries as e, i (i)}
      {#if e.kind === 'label'}
        <div class="menu-label ellipsis">{e.text}</div>
      {:else if e.kind === 'sep'}
        <div class="menu-sep"></div>
      {:else}
        <button class="menu-item" class:danger={e.danger} role="menuitem" disabled={e.disabled} aria-disabled={e.disabled} onclick={() => run(e)}>
          {#if e.icon}<Icon name={e.icon} />{/if}
          <span class="txt ellipsis">{e.text}</span>
          {#if e.hint}<span class="hint">{e.hint}</span>{/if}
        </button>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .ctxmenu {
    position: fixed;
    z-index: 130;
    width: max-content;
    min-width: 13rem;
    max-width: min(20rem, calc(100vw - 1rem));
    max-height: calc(100dvh - 1rem);
    overflow-y: auto;
    outline: none;
    transform-origin: top left;
  }
  .menu-item:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .menu-item:disabled:hover {
    background: none;
  }
  .menu-item:focus-visible {
    outline: none;
    background: var(--surface-3);
  }
  .txt {
    flex: 1;
    min-width: 0;
  }
  .hint {
    flex: none;
    margin-left: 0.75rem;
    font-size: var(--fs-xs);
    color: var(--text-3);
  }
  .danger,
  .danger :global(svg) {
    color: var(--negative);
  }
  .danger:hover {
    background: color-mix(in srgb, var(--negative) 12%, transparent);
  }
  .menu-label {
    max-width: 100%;
  }
  /* Fingers need room. */
  @media (pointer: coarse) {
    .menu-item {
      min-height: 2.75rem;
    }
  }
</style>
