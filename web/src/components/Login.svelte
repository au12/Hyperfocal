<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { api } from '../lib/api';

  let pw = $state('');
  let err = $state('');
  let busy = $state(false);
  let shake = $state(0);

  async function submit(e: Event) {
    e.preventDefault();
    busy = true;
    err = '';
    try {
      await api.login(pw);
      store.needLogin = false;
      store.init();
    } catch (x) {
      err = (x as Error).message;
      shake++;
    } finally {
      busy = false;
    }
  }
</script>

<div class="wrap">
  <div class="aura" aria-hidden="true"></div>
  {#key shake}
    <form class="card" class:shake={shake > 0} onsubmit={submit}>
      <img class="logo" src="./logo.png" alt="" width="64" height="64" />
      <h1>Hyperfocal</h1>
      <p class="tag">Sign in to your library</p>
      <!-- svelte-ignore a11y_autofocus -->
      <input class="field" type="password" bind:value={pw} placeholder="Password" autocomplete="current-password" autofocus />
      {#if err}<div class="err">{err}</div>{/if}
      <button class="btn primary" type="submit" disabled={busy || !pw}>
        {#if busy}<span class="spinner light"></span>{/if}Sign in
      </button>
    </form>
  {/key}
</div>

<style>
  .wrap {
    position: relative;
    height: 100dvh;
    display: grid;
    place-items: center;
    padding: 1rem;
    overflow: hidden;
    isolation: isolate;
  }
  /* A slow-turning glow in the colours of the logo's lens. */
  .aura {
    position: absolute;
    z-index: -1;
    left: 50%;
    top: 50%;
    width: min(44rem, 140vw);
    aspect-ratio: 1;
    border-radius: 50%;
    background: conic-gradient(from 0deg, #ff9a3d, #ff4f8b, #a35bff, #4f8cff, #ff9a3d);
    filter: blur(110px);
    opacity: 0.2;
    transform: translate(-50%, -50%);
    animation: turn 22s linear infinite;
  }
  @keyframes turn {
    to {
      transform: translate(-50%, -50%) rotate(360deg);
    }
  }
  .card {
    width: min(22rem, 100%);
    background: color-mix(in srgb, var(--surface) 84%, transparent);
    backdrop-filter: blur(24px) saturate(1.3);
    -webkit-backdrop-filter: blur(24px) saturate(1.3);
    border-radius: var(--r-2xl);
    box-shadow: var(--shadow-2);
    padding: 2.25rem 2rem 2rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    align-items: stretch;
    text-align: center;
    animation: dialog-in 0.5s var(--ease-out);
  }
  .card.shake {
    animation: shake 0.42s var(--ease-out);
  }
  @keyframes shake {
    20%,
    60% {
      transform: translateX(-7px);
    }
    40%,
    80% {
      transform: translateX(7px);
    }
  }
  .logo {
    width: 4rem;
    height: 4rem;
    margin: 0 auto 0.25rem;
    filter: drop-shadow(0 10px 24px rgba(99, 114, 255, 0.35));
    animation: logo-in 0.9s var(--ease-spring) both;
  }
  .card.shake .logo {
    animation: none;
  }
  @keyframes logo-in {
    from {
      transform: rotate(-120deg) scale(0.6);
      opacity: 0;
    }
  }
  h1 {
    margin: 0;
    font-size: 1.5rem;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  .tag {
    margin: -0.375rem 0 0.75rem;
    color: var(--text-3);
    font-size: var(--fs);
  }
  .field {
    height: 2.5rem;
    text-align: center;
  }
  .btn {
    height: 2.5rem;
  }
  .spinner.light {
    width: 0.875rem;
    height: 0.875rem;
    background: conic-gradient(from 0deg, transparent 0 25%, #fff 100%);
  }
  .err {
    color: var(--negative);
    font-size: var(--fs-sm);
    animation: chip-in var(--dur) var(--ease-out);
  }
</style>
