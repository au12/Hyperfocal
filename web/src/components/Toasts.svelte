<script lang="ts">
  import { flip } from 'svelte/animate';
  import { store } from '../lib/store.svelte';
  import { rise, fade, reduced } from '../lib/motion';
  import Icon from './Icon.svelte';

  const icon = (k: string) => (k === 'error' ? 'info' : k === 'ok' ? 'check' : 'sparkles');
</script>

<!-- Above the selection bar while it is shown. -->
<div class="toasts" class:lifted={store.selected.size > 0 && !store.loupe && !store.compare} aria-live="polite">
  {#each store.toasts as t (t.id)}
    <div class="toast {t.kind}" animate:flip={{ duration: reduced() ? 0 : 260 }} in:rise={{ y: 18, scale: 0.96, duration: 340 }} out:fade={{ duration: 160 }}>
      <span class="ico"><Icon name={icon(t.kind)} size={14} stroke={2.25} /></span>
      <span class="grow msg">{t.text}</span>
      {#if t.action}
        <button class="act" onclick={() => { t.action?.run(); store.toasts = store.toasts.filter((x) => x.id !== t.id); }}>{t.action.label}</button>
      {/if}
      {#if t.ms}<span class="life" style:animation-duration="{t.ms}ms"></span>{/if}
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
    width: min(30rem, calc(100% - 1.5rem));
    transition: bottom var(--dur-slow) var(--ease-out);
  }
  .toasts.lifted {
    bottom: calc(4.5rem + env(safe-area-inset-bottom));
  }
  .toast {
    position: relative;
    pointer-events: auto;
    display: flex;
    align-items: center;
    gap: 0.625rem;
    width: 100%;
    padding: 0.5625rem 0.5625rem 0.5625rem 0.625rem;
    border-radius: 0.875rem;
    background: rgba(24, 26, 32, 0.86);
    backdrop-filter: blur(20px) saturate(1.6);
    -webkit-backdrop-filter: blur(20px) saturate(1.6);
    color: #f1f2f5;
    box-shadow:
      0 18px 40px -14px rgba(0, 0, 0, 0.65),
      0 0 0 1px rgba(255, 255, 255, 0.09),
      inset 0 1px 0 rgba(255, 255, 255, 0.06);
    font-size: var(--fs);
    overflow: hidden;
  }
  .msg {
    line-height: 1.35;
  }
  .ico {
    width: 1.5rem;
    height: 1.5rem;
    flex: none;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: rgba(142, 156, 255, 0.18);
    color: #b3bcff;
  }
  .toast.ok .ico {
    background: rgba(69, 208, 140, 0.18);
    color: #6ee7a8;
    animation: tick 0.5s var(--ease-spring);
  }
  .toast.error .ico {
    background: rgba(255, 107, 107, 0.2);
    color: #ff8a8a;
  }
  .toast.error {
    box-shadow:
      0 18px 40px -14px rgba(0, 0, 0, 0.65),
      0 0 0 1px rgba(255, 107, 107, 0.35),
      inset 0 1px 0 rgba(255, 255, 255, 0.06);
  }
  @keyframes tick {
    from {
      transform: scale(0.4) rotate(-30deg);
    }
  }
  .act {
    border: 0;
    background: rgba(255, 255, 255, 0.1);
    color: #c3cbff;
    font-weight: 650;
    border-radius: 0.5rem;
    padding: 0.3125rem 0.6875rem;
    cursor: pointer;
    transition: background-color var(--dur-fast), transform var(--dur-fast) var(--ease-out);
  }
  .act:hover {
    background: rgba(255, 255, 255, 0.18);
  }
  .act:active {
    transform: scale(0.96);
  }
  /* How long the toast stays: a hairline that runs out. */
  .life {
    position: absolute;
    left: 0;
    bottom: 0;
    height: 2px;
    width: 100%;
    background: var(--spectrum);
    opacity: 0.55;
    transform-origin: left;
    animation: life linear forwards;
  }
  .toast.error .life {
    background: #ff6b6b;
  }
  @keyframes life {
    from {
      transform: scaleX(1);
    }
    to {
      transform: scaleX(0);
    }
  }
</style>
