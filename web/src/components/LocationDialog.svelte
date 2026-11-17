<script lang="ts">
  import { onMount } from 'svelte';
  import type * as Leaflet from 'leaflet';
  import { store } from '../lib/store.svelte';
  import { api } from '../lib/api';
  import Icon from './Icon.svelte';

  let el: HTMLDivElement | undefined = $state();
  let L: typeof Leaflet;
  let map: Leaflet.Map | null = null;
  let marker: Leaflet.Marker | null = null;
  let pinIcon: Leaflet.DivIcon;
  let pos = $state<{ lat: number; lon: number } | null>(null);
  let q = $state('');
  let results = $state<{ display_name: string; lat: string; lon: string }[]>([]);
  let searching = $state(false);
  let fillPlace = $state(localStorage.getItem('hf.revgeo') !== '0');
  let saving = $state(false);
  let existing = $state(0);

  const n = store.locationTargets.length;

  const pinHtml = {
    className: 'hf-pin',
    html: '<svg width="34" height="44" viewBox="0 0 34 44"><path d="M17 43s15-14.6 15-26A15 15 0 0 0 2 17c0 11.4 15 26 15 26Z" fill="#0070f2" stroke="#fff" stroke-width="2.5"/><circle cx="17" cy="17" r="5.5" fill="#fff"/></svg>',
  };

  function place(lat: number, lon: number, fly = false) {
    pos = { lat, lon };
    if (!map) return;
    if (!marker) {
      marker = L.marker([lat, lon], { draggable: true, icon: pinIcon }).addTo(map);
      marker.on('dragend', () => {
        const p = marker!.getLatLng();
        pos = { lat: p.lat, lon: p.lng };
      });
    } else marker.setLatLng([lat, lon]);
    if (fly) map.setView([lat, lon], Math.max(map.getZoom(), 14));
  }

  onMount(() => {
    let dead = false;
    (async () => {
      L = (await import('leaflet')).default;
      if (dead || !el) return;
      pinIcon = L.divIcon({ ...pinHtml, iconSize: [34, 44], iconAnchor: [17, 43] });
      setup();
    })();
    return () => {
      dead = true;
      map?.remove();
      map = null;
    };
  });

  function setup() {
    if (!el) return;
    map = L.map(el, { worldCopyJump: true }).setView([30, -40], 2);
    L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
      maxZoom: 19,
      attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>',
    }).addTo(map);
    map.on('click', (e: Leaflet.LeafletMouseEvent) => place(e.latlng.lat, e.latlng.lng));
    // Start from the photo's own position, or the last place used.
    const d = store.detail;
    if (n === 1 && d && d.id === store.locationTargets[0] && d.lat != null && d.lon != null) {
      place(d.lat, d.lon);
      map.setView([d.lat, d.lon], 14);
    } else {
      try {
        const last = JSON.parse(localStorage.getItem('hf.lastLoc') ?? 'null');
        if (last) map.setView([last.lat, last.lon], last.z ?? 12);
      } catch {
        /* ignore */
      }
    }
    // How many of the targets already have a position.
    api.photos({ ...store.params(), gps: 1, limit: 100000 }).then((l) => {
      const s = new Set(l.id);
      existing = store.locationTargets.filter((id) => s.has(id)).length;
    }).catch(() => {});
    setTimeout(() => map?.invalidateSize(), 60);
  }

  async function search() {
    const text = q.trim();
    if (!text) return;
    // Plain coordinates: "35.59, -82.55"
    const m = text.match(/^\s*(-?\d+(?:\.\d+)?)\s*[, ]\s*(-?\d+(?:\.\d+)?)\s*$/);
    if (m) {
      place(+m[1], +m[2], true);
      results = [];
      return;
    }
    searching = true;
    try {
      const r = await fetch(`https://nominatim.openstreetmap.org/search?format=jsonv2&limit=8&q=${encodeURIComponent(text)}`, { headers: { 'Accept-Language': navigator.language } });
      results = await r.json();
    } catch {
      store.toast('Place search is unavailable (OpenStreetMap Nominatim)', 'error');
    } finally {
      searching = false;
    }
  }

  async function reverse(lat: number, lon: number) {
    try {
      const r = await fetch(`https://nominatim.openstreetmap.org/reverse?format=jsonv2&zoom=14&addressdetails=1&lat=${lat}&lon=${lon}`, { headers: { 'Accept-Language': navigator.language } });
      const j = await r.json();
      const a = j.address ?? {};
      return {
        city: a.city ?? a.town ?? a.village ?? a.hamlet ?? a.municipality ?? a.county ?? null,
        state: a.state ?? a.region ?? null,
        country: a.country ?? null,
        country_code: a.country_code ? String(a.country_code).toUpperCase() : null,
      };
    } catch {
      return null;
    }
  }

  async function apply() {
    if (!pos) return;
    saving = true;
    localStorage.setItem('hf.revgeo', fillPlace ? '1' : '0');
    localStorage.setItem('hf.lastLoc', JSON.stringify({ lat: pos.lat, lon: pos.lon, z: map?.getZoom() ?? 12 }));
    const patch: Record<string, unknown> = { gps: { lat: +pos.lat.toFixed(7), lon: +pos.lon.toFixed(7) } };
    if (fillPlace) {
      const p = await reverse(pos.lat, pos.lon);
      if (p) Object.assign(patch, p);
    }
    const r = await store.patch(store.locationTargets, patch, `Location set for ${n} photo${n > 1 ? 's' : ''}`);
    saving = false;
    if (r) {
      store.dialog = null;
      store.reload(true);
    }
  }

  async function clear() {
    saving = true;
    const r = await store.patch(store.locationTargets, { gps: null }, `Location removed from ${n} photo${n > 1 ? 's' : ''}`);
    saving = false;
    if (r) {
      store.dialog = null;
      store.reload(true);
    }
  }
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (store.dialog = null)}>
  <div class="dialog wide" role="dialog" aria-modal="true" aria-label="Set location">
    <header>
      <Icon name="pin" />
      <h2>Set location · {n} photo{n > 1 ? 's' : ''}</h2>
      <button class="btn ghost icon" onclick={() => (store.dialog = null)} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="searchbar">
      <form class="row grow" onsubmit={(e) => { e.preventDefault(); search(); }}>
        <input class="field grow" bind:value={q} placeholder="Search a place or paste coordinates (35.595, -82.551)" />
        <button class="btn" type="submit">{#if searching}<span class="spinner"></span>{:else}<Icon name="search" />{/if}</button>
      </form>
      {#if results.length}
        <div class="results pop">
          {#each results as r}
            <button class="menu-item" onclick={() => { place(+r.lat, +r.lon, true); results = []; }}><Icon name="pin" /><span class="ellipsis">{r.display_name}</span></button>
          {/each}
        </div>
      {/if}
    </div>
    <div class="mapbox" bind:this={el}></div>
    <footer>
      <div class="grow left">
        {#if pos}<span class="tnum">{pos.lat.toFixed(5)}, {pos.lon.toFixed(5)}</span>{:else}<span class="dim">Click the map or search a place</span>{/if}
        <label class="check small"><input type="checkbox" bind:checked={fillPlace} /> Fill city, state & country (OpenStreetMap)</label>
      </div>
      {#if existing > 0}<button class="btn danger" onclick={clear} disabled={saving}>Remove location ({existing})</button>{/if}
      <button class="btn primary" onclick={apply} disabled={!pos || saving}>{#if saving}<span class="spinner"></span>{/if}Apply to {n}</button>
    </footer>
  </div>
</div>

<style>
  .dialog.wide {
    width: min(64rem, 100%);
    height: min(86vh, 48rem);
  }
  .searchbar {
    position: relative;
    padding: 0.75rem 1.25rem;
    border-bottom: 1px solid var(--border);
  }
  .results {
    left: 1.25rem;
    right: 1.25rem;
    top: calc(100% - 0.25rem);
    max-height: 18rem;
    overflow: auto;
    z-index: 1000;
  }
  .mapbox {
    flex: 1;
    min-height: 0;
  }
  .left {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: var(--fs-sm);
  }
  .small {
    font-size: var(--fs-sm);
  }
  footer {
    align-items: center;
  }
  :global(.hf-pin) {
    background: none;
    border: 0;
  }
  @media (max-width: 760px) {
    .dialog.wide {
      height: 100%;
      max-height: 100%;
      border-radius: 0;
    }
    :global(.scrim:has(.dialog.wide)) {
      padding: 0;
    }
    footer {
      flex-wrap: wrap;
    }
  }
</style>
