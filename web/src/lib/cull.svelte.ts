// Culling help. With culling on, the grid shows each series (a burst, a run of attempts
// at one moment) as a stack topped by its best frame, and badges for what may be wrong
// with a photo; the single view lines up the faces of a series side by side. The server
// measures every photo in the background: the sharpness of its subject (faces first,
// from the RAW), closed eyes and clipping. Nothing is ever rejected automatically: the
// "possible rejects" are a filter, and keeping the best frame of a series is one click.

import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import { api, ISSUE_BLUR, ISSUE_EYES, ISSUE_OVER, ISSUE_UNDER, type CullIssue, type CullPhoto, type CullStatus } from './api';
import { store } from './store.svelte';
import { tuning } from './tuning.svelte';

export interface IssueInfo {
  key: CullIssue;
  bit: number;
  label: string;
  icon: string;
  hint: string;
}

export const ISSUES: IssueInfo[] = [
  { key: 'blur', bit: ISSUE_BLUR, label: 'Blurry', icon: 'blur', hint: 'The subject is out of focus or shaken (measured on the faces, else the sharpest part)' },
  { key: 'eyes', bit: ISSUE_EYES, label: 'Eyes closed', icon: 'eye-closed', hint: 'Someone blinked or looked down' },
  { key: 'over', bit: ISSUE_OVER, label: 'Clipped', icon: 'sun', hint: 'Highlights clipped (measured on the RAW data)' },
  { key: 'under', bit: ISSUE_UNDER, label: 'Too dark', icon: 'moon', hint: 'Half the photo is deep in the shadows' },
];

export const issueOf = (k: CullIssue) => ISSUES.find((i) => i.key === k)!;

/** Sharpness 0..1 in words. */
export function focusWord(f: number | null | undefined): string {
  if (f == null) return 'Not measured';
  return f >= tuning.sharp() ? 'Sharp' : f >= tuning.blurry() ? 'Soft' : 'Blurry';
}

/** A colour class for a sharpness: good, fair, poor. */
export function focusTone(f: number | null | undefined): 'good' | 'fair' | 'poor' | 'none' {
  if (f == null) return 'none';
  return f >= tuning.sharp() ? 'good' : f >= tuning.blurry() ? 'fair' : 'poor';
}

class Cull {
  /** Stacks shown whole, by key (their first frame). */
  open = new SvelteSet<number>();
  /** Measurements, faces and series of photos looked at, by id. */
  photos = new SvelteMap<number, CullPhoto>();
  private loading = new Map<number, Promise<CullPhoto | null>>();
  private reloadTimer: ReturnType<typeof setTimeout> | null = null;

  /** Culling is on. */
  on = () => store.settings.cull;

  setOn = (on: boolean) => {
    if (store.settings.cull === on) return;
    store.settings.cull = on;
    if (!on) this.open.clear();
    store.persist();
    store.reload(true);
    store.toast(on ? 'Culling on: series as stacks, possible rejects marked' : 'Culling off', 'info', undefined, 2600);
  };

  toggle = () => this.setOn(!store.settings.cull);

  /** Shows a stack whole, or folds it back to its best frame. */
  toggleStack = (key: number) => {
    if (!key) return;
    if (this.open.has(key)) this.open.delete(key);
    else this.open.add(key);
    store.reload(true);
  };

  /** The stack of the photo under the cursor (or shown), folded or unfolded. */
  toggleStackAt = () => {
    const L = store.list;
    const i = store.loupe ? store.current : store.cursor;
    const key = L.sk?.[i];
    if (key) this.toggleStack(key);
    else store.toast('Not part of a series', 'info', undefined, 1800);
  };

  expandAll = () => {
    const L = store.list;
    for (const k of L.sk ?? []) if (k) this.open.add(k);
    store.reload(true);
  };

  collapseAll = () => {
    this.open.clear();
    store.reload(true);
  };

  /** A photo's measurements (cached; `fresh` asks again). */
  get = (id: number, fresh = false): Promise<CullPhoto | null> => {
    if (!fresh) {
      const have = this.photos.get(id);
      if (have) return Promise.resolve(have);
      const pending = this.loading.get(id);
      if (pending) return pending;
    }
    const p = api
      .cullPhoto(id)
      .then((c) => {
        this.photos.set(id, c);
        // Keep the cache small.
        if (this.photos.size > 300) {
          const first = this.photos.keys().next().value;
          if (first != null && first !== id) this.photos.delete(first);
        }
        return c;
      })
      .catch(() => null)
      .finally(() => this.loading.delete(id));
    this.loading.set(id, p);
    return p;
  };

  /** Goes to a frame of a series, unfolding its stack first when it is folded away. */
  showFrame = async (id: number, key: number) => {
    const i = store.index.get(id);
    if (i != null) {
      if (store.loupe) store.goTo(i);
      else store.moveCursor(i);
      return;
    }
    if (key && !this.open.has(key)) {
      this.open.add(key);
      await store.reload(true);
      const j = store.index.get(id);
      if (j != null) {
        if (store.loupe) store.goTo(j);
        else store.moveCursor(j);
      }
    }
  };

  /** Keeps the best frame of a series and moves the others to the reject folder (one
   * undoable move). */
  keepBest = async (c: CullPhoto) => {
    const s = c.series;
    if (!s) return;
    const others = s.frames.map((f) => f.id).filter((id) => id !== s.best);
    if (!others.length) return;
    await store.reject(others);
    for (const id of [s.best, ...others]) this.photos.delete(id);
  };

  /** Progress and new measurements from the server. */
  onEvent = (e: { phase: string; done: number; total: number; ids?: number[] }) => {
    if (store.status) {
      const prev = store.status.cull;
      store.status = { ...store.status, cull: { ...(prev ?? { rate: 0, error: null, eyes: false }), phase: e.phase, done: e.done, total: e.total } as CullStatus };
    }
    const ids = e.ids ?? [];
    if (!ids.length) return;
    for (const id of ids) this.photos.delete(id);
    // Badges and stacks follow the new measurements (a moment later, all at once).
    if (this.on() && ids.some((id) => store.index.has(id))) {
      if (this.reloadTimer) clearTimeout(this.reloadTimer);
      this.reloadTimer = setTimeout(() => {
        this.reloadTimer = null;
        store.reload(true);
      }, 1500);
    }
    this.rev++;
  };

  /** New advanced settings: series, best frames and badges may all have changed. */
  onTuning = () => {
    this.photos.clear();
    this.rev++;
    if (this.on()) store.reload(true);
  };

  /** Bumped when measurements arrive (the single view asks again). */
  rev = $state(0);
}

export const cull = new Cull();
