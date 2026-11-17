<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    trigger,
    children,
    align = 'left',
    open = $bindable(false),
    width,
  }: { trigger: Snippet<[() => void, boolean]>; children: Snippet<[() => void]>; align?: 'left' | 'right'; open?: boolean; width?: string } = $props();

  let root: HTMLElement | undefined = $state();

  function toggle() {
    open = !open;
  }
  function close() {
    open = false;
  }

  $effect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (root && !root.contains(e.target as Node)) open = false;
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        open = false;
        e.stopPropagation();
      }
    };
    window.addEventListener('pointerdown', onDown, true);
    window.addEventListener('keydown', onKey, true);
    return () => {
      window.removeEventListener('pointerdown', onDown, true);
      window.removeEventListener('keydown', onKey, true);
    };
  });
</script>

<div class="popwrap" bind:this={root}>
  {@render trigger(toggle, open)}
  {#if open}
    <div class="pop" class:right={align === 'right'} style:width>
      {@render children(close)}
    </div>
  {/if}
</div>

<style>
  .popwrap {
    position: relative;
    display: inline-flex;
  }
  .pop {
    top: calc(100% + 0.375rem);
    left: 0;
  }
  .pop.right {
    left: auto;
    right: 0;
  }
</style>
