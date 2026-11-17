<script lang="ts">
  import { store } from '../lib/store.svelte';
  import Icon from './Icon.svelte';
</script>

<div class="toasts" aria-live="polite">
  {#each store.toasts as t (t.id)}
    <div class="toast {t.kind}">
      <Icon name={t.kind === 'error' ? 'info' : t.kind === 'ok' ? 'check' : 'info'} size={16} />
      <span class="grow">{t.text}</span>
      {#if t.action}
        <button class="act" onclick={() => { t.action?.run(); store.toasts = store.toasts.filter((x) => x.id !== t.id); }}>{t.action.label}</button>
      {/if}
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    left: 50%;
    bottom: calc(1.25rem + env(safe-area-inset-bottom));
    transform: translateX(-50%);
    z-index: 100;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    align-items: center;
    pointer-events: none;
    width: min(32rem, calc(100% - 1.5rem));
  }
  .toast {
    pointer-events: auto;
    display: flex;
    align-items: center;
    gap: 0.625rem;
    width: 100%;
    padding: 0.625rem 0.75rem 0.625rem 0.875rem;
    border-radius: var(--r-lg);
    background: #22303d;
    color: #f5f6f7;
    box-shadow: var(--shadow-2);
    animation: pop-in 0.18s var(--ease);
    font-size: var(--fs);
  }
  .toast.ok :global(svg) {
    color: #97dd40;
  }
  .toast.error {
    background: #4a1f24;
  }
  .toast.error :global(svg) {
    color: #fa6161;
  }
  .act {
    border: 0;
    background: rgba(255, 255, 255, 0.12);
    color: #9ad3ff;
    font-weight: 700;
    border-radius: var(--r);
    padding: 0.25rem 0.625rem;
    cursor: pointer;
  }
  .act:hover {
    background: rgba(255, 255, 255, 0.2);
  }
</style>
