<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { api } from '../lib/api';

  let pw = $state('');
  let err = $state('');
  let busy = $state(false);

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
    } finally {
      busy = false;
    }
  }
</script>

<div class="wrap">
  <form class="card" onsubmit={submit}>
    <img class="logo" src="./logo.png" alt="" width="56" height="56" />
    <h1>Hyperfocal</h1>
    <input class="field" type="password" bind:value={pw} placeholder="Password" autocomplete="current-password" />
    {#if err}<div class="err">{err}</div>{/if}
    <button class="btn primary" type="submit" disabled={busy || !pw}>Sign in</button>
  </form>
</div>

<style>
  .wrap {
    height: 100dvh;
    display: grid;
    place-items: center;
    padding: 1rem;
  }
  .card {
    width: min(22rem, 100%);
    background: var(--surface);
    border-radius: var(--r-xl);
    box-shadow: var(--shadow-2);
    padding: 2rem;
    display: flex;
    flex-direction: column;
    gap: 0.875rem;
    align-items: stretch;
    text-align: center;
  }
  .logo {
    width: 3.5rem;
    height: 3.5rem;
    margin: 0 auto;
  }
  h1 {
    margin: 0 0 0.5rem;
    font-size: 1.375rem;
  }
  .err {
    color: var(--negative);
    font-size: var(--fs-sm);
  }
</style>
