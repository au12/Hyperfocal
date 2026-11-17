<script lang="ts">
  import { onMount } from 'svelte';
  import type * as Leaflet from 'leaflet';
  import { store } from '../lib/store.svelte';
  import { api, urls, type Cluster } from '../lib/api';
  import Icon from './Icon.svelte';

  let el: HTMLDivElement | undefined = $state();
  let L: typeof Leaflet;
  let map: Leaflet.Map | null = null;
  let layer: Leaflet.LayerGroup | null = null;
  let ctl: AbortController | null = null;
  let total = $state(0);
  let located = $state(0);
  let fitted = false;

  function scopeParams() {
    const p = store.params();
    delete (p as any).sort;
    delete (p as any).desc;
    return p;
  }

  async function refresh() {
    if (!map) return;
    ctl?.abort();
    ctl = new AbortController();
    const b = map.getBounds();
    const z = map.getZoom();
    try {
      if (!fitted) {
        const all = await api.clusters({ ...scopeParams(), zoom: 1 }, ctl.signal);
        fitted = true;
        if (all.clusters.length) {
          let s = 90, w = 180, n = -90, e = -180;
          for (const c of all.clusters) {
            s = Math.min(s, c.bounds[0]);
            w = Math.min(w, c.bounds[1]);
            n = Math.max(n, c.bounds[2]);
            e = Math.max(e, c.bounds[3]);
          }
          map.fitBounds([[s, w], [n, e]], { padding: [40, 40], maxZoom: 13, animate: false });
          return; // moveend triggers the real refresh
        }
      }
      const r = await api.clusters(
        { ...scopeParams(), zoom: z, west: b.getWest() - 1, east: b.getEast() + 1, south: b.getSouth() - 1, north: b.getNorth() + 1 },
        ctl.signal,
      );
      draw(r.clusters);
    } catch {
      /* aborted */
    }
  }

  function draw(cs: Cluster[]) {
    if (!map || !layer) return;
    layer.clearLayers();
    total = cs.reduce((a, c) => a + c.n, 0);
    for (const c of cs) {
      const size = c.n > 1 ? 64 : 52;
      const html = `<div class="mk${c.n > 1 ? ' many' : ''}"><img src="${c.v ? urls.thumb(c.id, c.v) : ''}" alt="" />${c.n > 1 ? `<span class="badge">${c.n > 999 ? Math.round(c.n / 1000) + 'k' : c.n}</span>` : ''}</div>`;
      const m = L.marker([c.lat, c.lon], {
        icon: L.divIcon({ html, className: 'hf-marker', iconSize: [size, size], iconAnchor: [size / 2, size / 2] }),
        riseOnHover: true,
      });
      m.on('click', () => openCluster(c));
      layer.addLayer(m);
    }
  }

  function openCluster(c: Cluster) {
    if (!map) return;
    if (c.n === 1) {
      const i = store.index.get(c.id);
      if (i != null) {
        store.openLoupe(i);
        return;
      }
    }
    const [s, w, n, e] = c.bounds;
    if (map.getZoom() < 16 && (n - s > 1e-5 || e - w > 1e-5)) {
      map.fitBounds([[s, w], [n, e]], { padding: [60, 60], maxZoom: 17 });
    } else {
      // Same spot: show those photos in the grid.
      store.view = 'grid';
      const ids = new Set<number>();
      const L2 = store.list;
      for (let k = 0; k < L2.id.length; k++) ids.add(L2.id[k]);
      const i = store.index.get(c.id);
      if (i != null) store.openLoupe(i);
    }
  }

  onMount(() => {
    let dead = false;
    (async () => {
      L = (await import('leaflet')).default;
      if (dead || !el) return;
      setup();
    })();
    return () => {
      dead = true;
      ctl?.abort();
      map?.remove();
      map = null;
    };
  });

  function setup() {
    if (!el) return;
    map = L.map(el, { zoomControl: true, worldCopyJump: true, preferCanvas: false }).setView([30, -40], 2);
    L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
      maxZoom: 19,
      attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>',
    }).addTo(map);
    layer = L.layerGroup().addTo(map);
    map.on('moveend', refresh);
    refresh();
    located = store.status?.counts.geotagged ?? 0;
  }

  // Re-query when the scope or filters change.
  $effect(() => {
    void store.rev;
    void store.folderId;
    void store.recursive;
    void store.query;
    refresh();
  });
</script>

<div class="mapwrap">
  <div class="map" bind:this={el}></div>
  <div class="info">
    <Icon name="pin" size={16} />
    <span class="tnum">{total.toLocaleString()} photo{total === 1 ? '' : 's'} with a location in view</span>
    {#if store.selected.size}
      <button class="btn sm primary" onclick={() => store.openLocation()}>Place {store.selected.size} selected…</button>
    {/if}
  </div>
  {#if located === 0}
    <div class="hint">
      <p><b>No geotagged photos yet.</b></p>
      <p class="dim">Select photos in the grid and press <kbd>L</kbd> (or use “Set location”) to place them on the map.</p>
    </div>
  {/if}
</div>

<style>
  .mapwrap {
    flex: 1;
    position: relative;
    min-height: 0;
  }
  .map {
    position: absolute;
    inset: 0;
  }
  :global([data-theme='dark']) .map :global(.leaflet-tile-pane) {
    filter: brightness(0.85) contrast(1.05);
  }
  .info {
    position: absolute;
    left: 50%;
    top: 0.75rem;
    transform: translateX(-50%);
    z-index: 500;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: var(--raised);
    border-radius: 1.25rem;
    padding: 0.375rem 0.5rem 0.375rem 0.875rem;
    box-shadow: var(--shadow-1);
    font-size: var(--fs-sm);
    white-space: nowrap;
  }
  .hint {
    position: absolute;
    left: 50%;
    bottom: 2rem;
    transform: translateX(-50%);
    z-index: 500;
    background: var(--raised);
    border-radius: var(--r-lg);
    padding: 0.5rem 1rem;
    box-shadow: var(--shadow-2);
    max-width: 90%;
    text-align: center;
  }
  .hint p {
    margin: 0.25rem 0;
  }
  :global(.hf-marker) {
    background: none;
    border: 0;
  }
  :global(.hf-marker .mk) {
    width: 100%;
    height: 100%;
    border-radius: 0.5rem;
    overflow: visible;
    position: relative;
    box-shadow: 0 0 0 2px #fff, 0 3px 10px rgba(0, 0, 0, 0.45);
    background: #2a3540;
    transition: transform 0.12s;
  }
  :global(.hf-marker .mk:hover) {
    transform: scale(1.08);
  }
  :global(.hf-marker .mk img) {
    width: 100%;
    height: 100%;
    object-fit: cover;
    border-radius: 0.5rem;
  }
  :global(.hf-marker .badge) {
    position: absolute;
    top: -0.5rem;
    right: -0.5rem;
    box-shadow: 0 0 0 2px #fff;
  }
</style>
