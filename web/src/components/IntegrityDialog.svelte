<script lang="ts">
  import { onMount } from 'svelte';
  import { store, fmtSize } from '../lib/store.svelte';
  import { api, type IntegrityProblem, type IntegritySummary } from '../lib/api';
  import Icon from './Icon.svelte';

  let s = $state.raw<IntegritySummary | null>(null);
  let error = $state('');
  let acting = $state(false);
  let testing = $state(false);

  async function load() {
    try {
      s = await api.integrity();
      error = '';
    } catch (e) {
      error = (e as Error).message;
    }
  }

  // Refresh while open: often while it works, now and then otherwise, and whenever the
  // problem counts change.
  onMount(() => {
    let t: ReturnType<typeof setTimeout>;
    const tick = () => {
      const busy = !!s && !['idle', 'off', 'offline'].includes(s.worker.phase);
      t = setTimeout(async () => {
        await load();
        tick();
      }, busy ? 1500 : 8000);
    };
    tick();
    return () => clearTimeout(t);
  });
  $effect(() => {
    void store.integrity.changed;
    void store.integrity.missing;
    void store.integrity.unreadable;
    load();
  });

  const working = $derived(!!s && !['idle', 'off', 'offline'].includes(s.worker.phase));
  const total = $derived(s ? s.changed + s.missing + s.unreadable : 0);
  const hashedPct = $derived(s && s.indexed ? Math.min(100, (100 * s.originals) / s.indexed) : 0);
  const recentPct = $derived(s && s.originals ? Math.round((100 * s.verified_recently) / s.originals) : 0);
  const groups = $derived.by(() => {
    const g: Record<number, IntegrityProblem[]> = { 1: [], 2: [], 3: [] };
    for (const p of s?.problems ?? []) g[p.status]?.push(p);
    return g;
  });

  const GROUP = { 1: ['Changed', 'shield-alert'], 2: ['Missing', 'x'], 3: ['Unreadable', 'info'] } as const;
  const path = (p: IntegrityProblem) => (p.folder ? `${p.folder}/${p.name}` : p.name);
  const plural = (n: number, w: string) => `${n.toLocaleString()} ${w}${n === 1 ? '' : 's'}`;
  const when = (ms: number | null) => (ms ? new Date(ms).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }) : '');
  const day = (ms: number | null) => (ms ? new Date(ms).toLocaleDateString(undefined, { dateStyle: 'medium' }) : '—');

  function nextLabel(): string {
    if (!s) return '';
    if (s.window.open) return 'now (the nightly window is open)';
    const d = new Date(s.window.next);
    const today = new Date();
    const tomorrow = new Date(today.getTime() + 86_400_000);
    const t = d.toLocaleTimeString(undefined, { timeStyle: 'short' });
    const dayName = d.toDateString() === today.toDateString() ? 'today' : d.toDateString() === tomorrow.toDateString() ? 'tomorrow' : d.toLocaleDateString();
    return `${dayName} at ${t}`;
  }

  function phaseLabel(): string {
    if (!s) return '';
    const w = s.worker;
    const what = { checking: 'Following up on the last scan', hashing: 'Hashing new photos', verifying: 'Verifying', offline: 'The library is offline' }[w.phase] ?? '';
    return w.current ? `${what}: ${w.current}` : what;
  }

  async function act(fn: () => Promise<unknown>, done?: string) {
    acting = true;
    try {
      await fn();
      if (done) store.toast(done, 'ok');
      await load();
    } catch (e) {
      store.toast((e as Error).message, 'error', undefined, 7000);
    } finally {
      acting = false;
    }
  }

  const runNow = () => act(() => api.integrityRun({}), 'Checking now');
  const stop = () => act(() => api.integrityRun({ stop: true }));
  function verifyAll() {
    if (!s) return;
    const hours = s.mbps ? s.bytes / (s.mbps * 1048576) / 3600 : 0;
    const est = hours >= 1 ? `about ${Math.round(hours)} hour${Math.round(hours) === 1 ? '' : 's'}` : 'under an hour';
    if (confirm(`Read every original again now (${fmtSize(s.bytes)}, ${est} at ${s.mbps} MB/s)? It pauses while you browse.`)) act(() => api.integrityRun({ all: true }), 'Verifying every original');
  }
  const recheck = (ids: number[]) => act(() => api.integrityRecheck(ids), ids.length > 1 ? `Checking ${ids.length} files again` : 'Checking again');
  function accept(ps: IntegrityProblem[]) {
    const one = ps.length === 1;
    if (confirm(one ? `Take ${path(ps[0])} as it is now as the reference? Only do this if the change was intended.` : `Take the ${ps.length} changed files as they are now as the reference? Only do this if the changes were intended.`))
      act(() => api.integrityAccept(ps.map((p) => p.id)), one ? 'Accepted' : `Accepted ${ps.length} files`);
  }
  function forget(ps: IntegrityProblem[]) {
    const one = ps.length === 1;
    if (confirm(one ? `Stop reporting ${path(ps[0])}? Do this when it was deleted on purpose.` : `Stop reporting the ${ps.length} missing files? Do this when they were deleted on purpose.`))
      act(() => api.integrityForget(ps.map((p) => p.id)), one ? 'Forgotten' : `Forgot ${ps.length} files`);
  }

  async function show(p: IntegrityProblem) {
    if (p.folder_id == null || p.photo_id == null) return;
    store.dialog = null;
    await store.openFolder(p.folder_id);
    const i = store.index.get(p.photo_id);
    if (i != null) store.openLoupe(i);
  }

  async function testAlert() {
    testing = true;
    try {
      await api.integrityTestAlert();
      store.toast(`Test alert sent to ${s?.alert?.host}`, 'ok');
    } catch (e) {
      store.toast(`The test alert failed: ${(e as Error).message}`, 'error', undefined, 9000);
    } finally {
      testing = false;
    }
  }
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (store.dialog = null)}>
  <div class="dialog integ" role="dialog" aria-modal="true" aria-label="Archive integrity">
    <header>
      <Icon name="shield" />
      <h2>Archive integrity</h2>
      <button class="btn ghost icon" onclick={() => (store.dialog = null)} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="body">
      {#if error && !s}
        <p class="err">{error}</p>
      {:else if !s}
        <div class="spinner"></div>
      {:else if !s.enabled}
        <p>The integrity monitor is off. Set <code>HYPERFOCAL_INTEGRITY=true</code> (the default) to have Hyperfocal hash every original and verify it again over time.</p>
      {:else}
        <div class="card" class:bad={total > 0}>
          <div class="big">
            <Icon name={total ? 'shield-alert' : 'shield-check'} size={28} />
            <div>
              {#if total}
                <div class="headline">{[s.changed ? `${s.changed.toLocaleString()} changed` : '', s.missing ? `${s.missing.toLocaleString()} missing` : '', s.unreadable ? `${s.unreadable.toLocaleString()} unreadable` : ''].filter(Boolean).join(' · ')}</div>
                <div class="dim small">Originals that are not as they were when first checked. Nothing was changed by Hyperfocal: it only reads them.</div>
              {:else if s.originals}
                <div class="headline">{plural(s.originals, 'original')} as they were</div>
                <div class="dim small">Each is read again every {s.days} days and compared with its first check.</div>
              {:else}
                <div class="headline">No original checked yet</div>
                <div class="dim small">The first check of each photo happens in the nightly window, or now with Run now.</div>
              {/if}
            </div>
          </div>
          <div class="facts small">
            <div class="fact">
              <span class="dim">Checked once</span>
              <span class="tnum">{s.originals.toLocaleString()} of {s.indexed.toLocaleString()} files ({hashedPct < 99.95 ? hashedPct.toFixed(1) : '100'}%)</span>
            </div>
            {#if s.originals < s.indexed}<div class="bar"><div style:width={`${hashedPct}%`}></div></div>{/if}
            <div class="fact"><span class="dim">Verified in the last {s.days} days</span><span class="tnum">{recentPct}% · oldest check {day(s.oldest_verified)}</span></div>
            <div class="fact"><span class="dim">Next nightly check</span><span>{nextLabel()}, for {s.window.minutes >= 60 ? `${+(s.window.minutes / 60).toFixed(1)} h` : `${s.window.minutes} min`}{s.mbps ? ` at up to ${s.mbps} MB/s` : ''}</span></div>
            {#if s.worker.session}
              {@const x = s.worker.session}
              <div class="fact">
                <span class="dim">{x.ended ? 'Last check' : 'This check'}</span>
                <span class="tnum">{when(x.started)} · {plural(x.files, 'file')}, {fmtSize(x.bytes)}{x.problems ? ` · ${plural(x.problems, 'problem')}` : ''}</span>
              </div>
            {/if}
            {#if working || s.worker.phase === 'offline'}<div class="now ellipsis"><span class="spinner"></span>{phaseLabel()}</div>{/if}
          </div>
          <div class="row wrap">
            {#if working && s.worker.phase === 'verifying'}
              <button class="btn" onclick={stop} disabled={acting}><Icon name="pause" />Stop</button>
            {:else}
              <button class="btn" onclick={runNow} disabled={acting}><Icon name="play" />Run now</button>
            {/if}
            <button class="btn ghost" onclick={verifyAll} disabled={acting || !s.originals}><Icon name="refresh" />Verify everything…</button>
            {#if s.alert}<button class="btn ghost" onclick={testAlert} disabled={testing}><Icon name="link" />Send a test alert</button>{/if}
          </div>
        </div>

        {#each [1, 2, 3] as const as k}
          {@const list = groups[k]}
          {#if list.length}
            <section>
              <div class="sechead">
                <Icon name={GROUP[k][1]} size={16} />
                <h3>{GROUP[k][0]} <span class="dim tnum">{list.length.toLocaleString()}</span></h3>
                {#if list.length > 1}
                  {#if k === 1}<button class="btn sm ghost" onclick={() => accept(list)} disabled={acting}>Accept all</button>{/if}
                  {#if k === 2}<button class="btn sm ghost" onclick={() => forget(list)} disabled={acting}>Forget all</button>{/if}
                  {#if k !== 2}<button class="btn sm ghost" onclick={() => recheck(list.map((p) => p.id))} disabled={acting}>Check all again</button>{/if}
                {/if}
              </div>
              {#each list as p (p.id)}
                <div class="prob">
                  <div class="grow">
                    <div class="pth" title={path(p)}>{path(p)}</div>
                    <div class="dim small">{p.detail ?? ''}{p.since ? ` · since ${when(p.since)}` : ''}{p.notified ? ' · alert sent' : ''}</div>
                  </div>
                  <div class="acts">
                    {#if p.photo_id != null}<button class="btn sm ghost" onclick={() => show(p)} title="Open the photo"><Icon name="eye" />Show</button>{/if}
                    {#if k !== 2}<button class="btn sm ghost" onclick={() => recheck([p.id])} disabled={acting}><Icon name="refresh" />Check again</button>{/if}
                    {#if k === 1}<button class="btn sm ghost" onclick={() => accept([p])} disabled={acting}><Icon name="check" />Accept</button>{/if}
                    {#if k === 2}<button class="btn sm ghost" onclick={() => forget([p])} disabled={acting}><Icon name="x" />Forget</button>{/if}
                  </div>
                </div>
              {/each}
            </section>
          {/if}
        {/each}

        <p class="foot dim small">
          Hyperfocal records a content hash of every original and reads each one again every {s.days} days, at night so array disks can sleep the rest of the day; new photos, and files whose size or date changed, are checked soon after a scan finds them.
          For JPEG, HEIC, TIFF and DNG files, which other apps write metadata into, only the image data counts, so a metadata edit is not reported. A file moved within the library is followed, not reported as missing.
          {#if s.alert}
            Alerts go to {s.alert.format === 'json' ? 'the webhook at' : s.alert.format === 'gotify' ? 'Gotify at' : 'ntfy at'} {s.alert.host}{s.worker.alert_error ? '' : '.'}
            {#if s.worker.alert_error}<span class="err">, but the last one failed: {s.worker.alert_error}.</span>{/if}
          {:else}
            Set <code>HYPERFOCAL_ALERT_URL</code> (an ntfy topic, a Gotify message URL or any webhook) to be alerted.
          {/if}
        </p>
      {/if}
    </div>
  </div>
</div>

<style>
  .dialog.integ {
    width: min(48rem, 100%);
  }
  .small {
    font-size: var(--fs-sm);
  }
  .err {
    color: var(--negative);
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 0.875rem;
    padding: 1rem;
    border-radius: var(--r-lg);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .card.bad {
    border-color: color-mix(in srgb, var(--negative) 55%, transparent);
  }
  .big {
    display: flex;
    gap: 0.875rem;
    align-items: flex-start;
  }
  .big > :global(svg) {
    flex: none;
    color: var(--positive);
  }
  .card.bad .big > :global(svg) {
    color: var(--negative);
  }
  .headline {
    font-weight: 700;
    font-size: 1.0625rem;
  }
  .facts {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }
  .fact {
    display: flex;
    justify-content: space-between;
    gap: 1rem;
    flex-wrap: wrap;
  }
  .bar {
    height: 0.3125rem;
    border-radius: 0.25rem;
    background: var(--surface-3);
    overflow: hidden;
  }
  .bar > div {
    height: 100%;
    background: var(--accent-strong);
  }
  .now {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    color: var(--text-2);
  }
  .now .spinner {
    width: 0.875rem;
    height: 0.875rem;
    flex: none;
  }
  .wrap {
    flex-wrap: wrap;
  }
  section {
    margin-top: 1.25rem;
  }
  .sechead {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding-bottom: 0.375rem;
    border-bottom: 1px solid var(--border);
  }
  .sechead h3 {
    margin: 0;
    font-size: var(--fs);
    flex: 1;
  }
  .prob {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    padding: 0.625rem 0;
    border-bottom: 1px solid var(--border);
  }
  .pth {
    overflow-wrap: anywhere;
    font-weight: 600;
  }
  .acts {
    display: flex;
    gap: 0.125rem;
    flex: none;
  }
  .foot {
    margin: 1.25rem 0 0;
    line-height: 1.5;
  }
  code {
    font-size: 0.85em;
  }
  @media (max-width: 760px) {
    .dialog.integ {
      height: 100%;
      max-height: 100%;
      border-radius: 0;
      padding-top: env(safe-area-inset-top);
    }
    :global(.scrim:has(.dialog.integ)) {
      padding: 0;
    }
    .prob {
      flex-direction: column;
      align-items: stretch;
    }
    .acts {
      flex-wrap: wrap;
    }
    .fact {
      flex-direction: column;
      gap: 0;
    }
  }
</style>
