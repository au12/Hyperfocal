<script lang="ts">
  import { store, applyTheme } from '../lib/store.svelte';
  import { api } from '../lib/api';
  import { offlineSupported, setOffline, clearOffline, offlineUsage } from '../lib/offline';
  import { mlProgress, mlState } from '../lib/ml';
  import Icon from './Icon.svelte';

  const s = store.settings;
  let rescanning = $state(false);

  function set<K extends keyof typeof s>(k: K, v: (typeof s)[K]) {
    store.settings[k] = v;
    if (k === 'theme') applyTheme(v as typeof s.theme);
    store.persist();
  }

  async function rescan() {
    rescanning = true;
    await api.rescan().catch(() => {});
    store.toast('Re-scan of the whole library started', 'ok');
    rescanning = false;
  }

  async function logout() {
    await api.logout().catch(() => {});
    // Offline copies are this user's photos: not left behind on a shared device.
    await clearOffline().catch(() => {});
    location.reload();
  }

  let usage = $state<number | null>(null);
  const refreshUsage = () => offlineUsage().then((u) => (usage = u));
  refreshUsage();

  async function toggleOffline(on: boolean) {
    set('offlineCache', on);
    await setOffline(on);
    refreshUsage();
  }

  async function clearCopies() {
    await clearOffline();
    store.toast('Offline copies cleared', 'ok');
    refreshUsage();
  }

  const mb = (b: number) => (b >= 1e9 ? `${(b / 1e9).toFixed(1)} GB` : `${Math.round(b / 1e6)} MB`);

  const st = $derived(store.status);

  const fmtMin = (m: number) => (m < 2 ? `${Math.round(m * 60)} s` : `${Math.round(m)} min`);

  /** What the AI engine does: its features, where it runs, and the space its models take. */
  function aiLine(m: NonNullable<typeof st>['ml']): string {
    if (!m || !m.enabled) return 'Off (HYPERFOCAL_ML)';
    const on = [m.faces && 'people and pets', m.search && 'search by content', m.eyes && 'closed eyes'].filter(Boolean).join(', ');
    const disk = m.disk_mb > 0 ? ` · ${m.disk_mb >= 1000 ? `${(m.disk_mb / 1000).toFixed(1)} GB` : `${m.disk_mb} MB`} of models` : '';
    const idle = m.idle_min ? ` · stops after ${fmtMin(m.idle_min)} without work` : '';
    return `${mlState(m)} · ${on}${m.phase === 'ready' || m.phase === 'asleep' ? disk + idle : ''}`;
  }
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (store.dialog = null)}>
  <div class="dialog" role="dialog" aria-modal="true" aria-label="Settings">
    <header>
      <Icon name="settings" />
      <h2>Settings</h2>
      <button class="btn ghost icon" onclick={() => (store.dialog = null)} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="body">
      <h3>Appearance</h3>
      <div class="opt">
        <span>Theme</span>
        <div class="seg">
          {#each [['dark', 'Dark'], ['light', 'Light'], ['auto', 'System']] as [k, label]}
            <button class:on={s.theme === k} onclick={() => set('theme', k as typeof s.theme)}>{label}</button>
          {/each}
        </div>
      </div>
      <label class="opt"><span>Show file names on thumbnails</span><span class="switch"><input type="checkbox" checked={s.showNames} onchange={(e) => set('showNames', (e.target as HTMLInputElement).checked)} /><span></span></span></label>
      <label class="opt"><span>Neutral gray background in the single view</span><span class="switch"><input type="checkbox" checked={s.bgGray} onchange={(e) => set('bgGray', (e.target as HTMLInputElement).checked)} /><span></span></span></label>

      <h3>Culling</h3>
      <div class="opt">
        <span>Keyboard shortcuts</span>
        <div class="seg">
          <button class:on={s.keymap === 'frv'} onclick={() => set('keymap', 'frv')}>FastRawViewer</button>
          <button class:on={s.keymap === 'lr'} onclick={() => set('keymap', 'lr')}>Lightroom / Excire</button>
        </div>
      </div>
      <div class="opt">
        <span>Your own shortcuts{#if Object.keys(store.keyOverrides()).length}{` (${Object.keys(store.keyOverrides()).length} changed)`}{/if}</span>
        <button class="btn sm" onclick={() => (store.dialog = 'help')}><Icon name="keyboard" />Customize…</button>
      </div>
      <div class="opt">
        <span>Single view opens as</span>
        <div class="seg">
          <button class:on={s.defaultSource === 'jpeg'} onclick={() => { set('defaultSource', 'jpeg'); store.setViewer('source', 'jpeg'); }}>Embedded JPEG</button>
          <button class:on={s.defaultSource === 'raw'} onclick={() => { set('defaultSource', 'raw'); store.setViewer('source', 'raw'); }}>RAW</button>
        </div>
      </div>
      <label class="opt"><span>Next photo after rating or labelling</span><span class="switch"><input type="checkbox" checked={s.advanceAfterRate} onchange={(e) => set('advanceAfterRate', (e.target as HTMLInputElement).checked)} /><span></span></span></label>
      <label class="opt"><span>Show the next photo after a reject</span><span class="switch"><input type="checkbox" checked={s.advanceAfterReject} onchange={(e) => set('advanceAfterReject', (e.target as HTMLInputElement).checked)} /><span></span></span></label>
      <label class="opt"><span>Photos to prefetch ahead</span>
        <select class="field small" value={s.prefetch} onchange={(e) => set('prefetch', +(e.target as HTMLSelectElement).value)}>
          {#each [0, 1, 2, 3, 4, 6, 8] as n}<option value={n}>{n}</option>{/each}
        </select>
      </label>

      <h3>View tools</h3>
      <label class="opt"><span>Screen sharpening amount, level 1 / 2</span>
        <span class="row">
          <input class="field small" type="number" min="0" max="3" step="0.05" value={s.sharpen1} onchange={(e) => set('sharpen1', +(e.target as HTMLInputElement).value)} />
          <input class="field small" type="number" min="0" max="4" step="0.05" value={s.sharpen2} onchange={(e) => set('sharpen2', +(e.target as HTMLInputElement).value)} />
        </span>
      </label>
      <label class="opt"><span>Focus peaking colour</span><input type="color" value={s.peakColor} onchange={(e) => set('peakColor', (e.target as HTMLInputElement).value)} /></label>
      <div class="opt">
        <span>Overexposure display</span>
        <div class="seg">
          <button class:on={s.overColor === 'channels'} onclick={() => set('overColor', 'channels')}>Per channel</button>
          <button class:on={s.overColor === 'red'} onclick={() => set('overColor', 'red')}>Solid red</button>
        </div>
      </div>
      <div class="opt">
        <span>Underexposure display</span>
        <span class="row">
          <div class="seg">
            <button class:on={s.underDisplay === 'channels'} onclick={() => set('underDisplay', 'channels')}>Per channel</button>
            <button class:on={s.underDisplay === 'solid'} onclick={() => set('underDisplay', 'solid')}>Solid</button>
          </div>
          {#if s.underDisplay === 'solid'}<input type="color" value={s.underColor} onchange={(e) => set('underColor', (e.target as HTMLInputElement).value)} />{/if}
        </span>
      </div>
      <div class="opt">
        <span>Full-resolution RAW when zoomed in</span>
        <div class="seg">
          {#each [['auto', 'Auto'], ['gpu', 'This device'], ['server', 'Server'], ['off', 'Off']] as [k, label]}
            <button class:on={s.rawDetail === k} onclick={() => set('rawDetail', k as typeof s.rawDetail)}>{label}</button>
          {/each}
        </div>
      </div>
      <p class="note dim">Zoomed in to 75% or more, the RAW view shows the full sensor resolution, demosaiced (RCD) tile by tile: on this device's graphics chip (more data over the network, instant tone changes) or on the server (smaller JPEG tiles). Auto uses the graphics chip on computers and the server on phones and tablets.</p>
      <p class="note dim">Per channel, as in FastRawViewer. Overexposed: cyan = red clipped, magenta = green, yellow = blue, black = all channels. Underexposed: red, green or blue for that channel, mixed when several are (magenta = red and blue), white = all. In RAW view both are measured on the raw data, before white balance: clipped within 0.05 EV of the sensor's saturation, underexposed 7.7 EV below it. Sharpening levels 1 and 2 use a 1 and 2 pixel radius.</p>

      <h3>Offline</h3>
      {#if offlineSupported}
        <label class="opt"><span>Keep thumbnails, previews and folder lists for offline use</span><span class="switch"><input type="checkbox" checked={s.offlineCache} onchange={(e) => toggleOffline((e.target as HTMLInputElement).checked)} /><span></span></span></label>
        <div class="opt">
          <span>Saved in this browser{usage != null ? `: ${mb(usage)}` : ''}</span>
          <button class="btn sm" onclick={clearCopies} disabled={!s.offlineCache}><Icon name="x" />Clear</button>
        </div>
        <p class="note dim">Photos looked at stay viewable when the server cannot be reached (up to a few thousand thumbnails and previews; the oldest go first). Changes need the server. Signing out clears them.</p>
      {:else}
        <p class="note dim">Offline copies need a secure connection: open Hyperfocal over HTTPS (for example through your reverse proxy) to keep thumbnails and previews for offline use.</p>
      {/if}

      <h3>Library</h3>
      {#if st}
        <dl>
          <dt>Version</dt><dd>{st.version}</dd>
          {#if st.decoders}
            {@const d = st.decoders}
            <dt>Decoders</dt>
            <dd>
              {d.raw === 'libraw' ? 'LibRaw ' + (d.libraw ?? '') + ' for all RAW files' : 'FlashRaw ' + d.flashraw + (d.libraw ? ', LibRaw ' + d.libraw + ' as fallback' : '')}
              · {d.libheif ? 'libheif ' + d.libheif : 'no HEIC support'}
            </dd>
          {/if}
          <dt>Photos</dt><dd class="tnum">{st.counts.photos.toLocaleString()} in {st.counts.folders.toLocaleString()} folders · {st.counts.geotagged.toLocaleString()} with GPS</dd>
          {#if st.ml}
            {@const m = st.ml}
            {@const pct = mlProgress(m)}
            <dt>AI</dt>
            <dd>
              {aiLine(m)}
              {#if pct != null}<span class="meter"><span style:width={`${pct * 100}%`}></span></span>{/if}
              {#if m.note}<div class="note dim">{m.note}</div>{/if}
            </dd>
          {/if}
          <dt>Writes</dt><dd>{st.readonly ? 'Read-only — nothing is written' : 'XMP sidecars and moves to ' + st.reject_dir + ' only; originals are never changed'}</dd>
          <dt>Index</dt><dd>{st.progress.phase}{#if st.progress.last_full_scan}{` · last full scan ${new Date(st.progress.last_full_scan).toLocaleString()}`}{/if}</dd>
        </dl>
      {/if}
      <div class="row wrap">
        <button class="btn" onclick={() => (store.dialog = 'keywords')}><Icon name="tags" /> Keywords</button>
        <button class="btn" onclick={() => (store.dialog = 'geotag')}><Icon name="route" /> Geotag from a track</button>
        <button class="btn" onclick={() => (store.dialog = 'duplicates')}><Icon name="copy" /> Duplicates</button>
        {#if st?.integrity !== false}<button class="btn" onclick={() => (store.dialog = 'integrity')}><Icon name="shield" /> Archive integrity</button>{/if}
        <button class="btn" onclick={rescan} disabled={rescanning}><Icon name="refresh" /> Re-scan library</button>
        {#if st?.auth}<button class="btn" onclick={logout}><Icon name="logout" /> Sign out</button>{/if}
      </div>
    </div>
  </div>
</div>

<style>
  h3 {
    margin: 1.25rem 0 0.5rem;
    font-size: var(--fs);
    color: var(--text-3);
  }
  h3:first-child {
    margin-top: 0;
  }
  .opt {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
  }
  .seg > button {
    height: 1.875rem;
    font-size: var(--fs-sm);
  }
  .field.small {
    width: 5rem;
    height: 2rem;
  }
  input[type='color'] {
    width: 2.5rem;
    height: 2rem;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-sm);
    background: none;
    padding: 0.125rem;
  }
  .note {
    font-size: var(--fs-sm);
  }
  dl {
    display: grid;
    grid-template-columns: 5rem 1fr;
    gap: 0.375rem 1rem;
    margin: 0 0 1rem;
  }
  dt {
    color: var(--text-3);
  }
  dd {
    margin: 0;
  }
  .meter {
    display: block;
    height: 3px;
    margin-top: 0.375rem;
    border-radius: 2px;
    background: var(--border);
    overflow: hidden;
  }
  .meter > span {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 0.6s var(--ease-out, ease-out);
  }
  .wrap {
    flex-wrap: wrap;
  }
  @media (max-width: 760px) {
    .opt {
      flex-wrap: wrap;
    }
  }
</style>
