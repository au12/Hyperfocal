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
    if (!on) {
      this.open.clear();
      this.focus = null;
    }
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
    this.focus = null;
    store.reload(true);
  };

  // ---------------------------------------------------------- series review
  // One series at a time, its frames side by side (SeriesView): ←/→ go through its
  // frames, ↑/↓ to the series before and after, Esc back to the library on its stack.
  // The single view opened from here stays within the series too.

  /** The series under review (its stack key), or null. */
  focus = $state<number | null>(null);
  /** The series was unfolded before the review (it stays so after). */
  private focusWasOpen = false;

  /** The list positions of the series under review: [start, end). */
  range = (): [number, number] | null => {
    const key = this.focus;
    const sk = store.list.sk;
    if (key == null || !sk) return null;
    const c = store.loupe ? store.current : store.cursor;
    let a = sk[c] === key ? c : sk.indexOf(key);
    if (a < 0) return null;
    let b = a;
    while (a > 0 && sk[a - 1] === key) a--;
    while (b < sk.length && sk[b] === key) b++;
    return [a, b];
  };

  /** Clamps a list position to the series under review (when there is one). */
  clamp = (i: number): number => {
    const r = this.range();
    return r ? Math.max(r[0], Math.min(r[1] - 1, i)) : i;
  };

  /** Reviews the series of the photo at list position `i` (default: the cursor). */
  review = async (i = store.loupe ? store.current : store.cursor) => {
    const L = store.list;
    const key = L.sk?.[i] ?? 0;
    if (!this.on() || !key) {
      store.toast(this.on() ? 'Not part of a series' : 'Turn culling on to review series', 'info', undefined, 2000);
      return;
    }
    if (this.focus === key) return;
    if (this.focus != null) this.unfocus();
    this.focusWasOpen = this.open.has(key);
    store.clearSelection();
    store.cursor = i;
    this.focus = key;
    if (!this.focusWasOpen) {
      this.open.add(key);
      await store.reload(true);
    }
  };

  /** Folds the series under review back (unless it was unfolded before). */
  private unfocus() {
    const key = this.focus;
    this.focus = null;
    if (key != null && !this.focusWasOpen) this.open.delete(key);
  }

  /** Ends the review: back to the library, the cursor on the series' stack. */
  leave = async () => {
    const r = this.range();
    if (this.focus == null) return;
    const L = store.list;
    if (r) {
      let best = r[0];
      for (let i = r[0]; i < r[1]; i++) if (L.sb?.[i] === 1) best = i;
      store.cursor = best;
    }
    const reload = !this.focusWasOpen;
    store.clearSelection();
    this.unfocus();
    if (reload) await store.reload(true);
  };

  /** The series `d` series after (before) the one under review, as its list position
   * (its first frame), or null at the end. */
  private neighbour(d: 1 | -1): number | null {
    const r = this.range();
    const sk = store.list.sk;
    if (!r || !sk) return null;
    if (d > 0) {
      for (let j = r[1]; j < sk.length; j++) if (sk[j]) return j;
    } else {
      for (let j = r[0] - 1; j >= 0; j--) {
        if (sk[j]) {
          while (j > 0 && sk[j - 1] === sk[j]) j--;
          return j;
        }
      }
    }
    return null;
  }

  /** Moves the review to the next (previous) series, on its best frame. */
  step = async (d: 1 | -1) => {
    const j = this.neighbour(d);
    if (j == null) {
      store.toast(d > 0 ? 'That was the last series' : 'This is the first series', 'info', undefined, 1600);
      return false;
    }
    const L = store.list;
    const key = L.sk![j];
    // The stack's best frame (a folded stack shows only it).
    let at = j;
    for (let k = j; k < L.id.length && L.sk![k] === key; k++) if (L.sb?.[k] === 1) at = k;
    const id = L.id[at];
    const wasOpen = this.open.has(key);
    store.clearSelection();
    this.unfocus();
    this.focus = key;
    this.focusWasOpen = wasOpen;
    this.open.add(key);
    store.cursor = at;
    if (store.loupe) store.current = at;
    await store.reload(true);
    const i = store.index.get(id);
    if (i != null) {
      store.cursor = i;
      if (store.loupe) store.goTo(i);
    }
    return true;
  };

  /** Where the series under review sits among the series on screen: its number and
   * how many there are. */
  position = (): { n: number; of: number } | null => {
    const r = this.range();
    const sk = store.list.sk;
    if (!r || !sk) return null;
    let n = 0;
    let of = 0;
    for (let j = 0; j < sk.length; j++) {
      if (sk[j] && sk[j] !== sk[j - 1]) {
        of++;
        if (j <= r[0]) n++;
      }
    }
    return { n, of };
  };

  /** Keeps these frames of the series under review and rejects the others (one undoable
   * move); then the next series. */
  private keeping = false;

  keep = async (ids: number[]) => {
    const r = this.range();
    if (!r || !ids.length || this.keeping) return;
    this.keeping = true;
    try {
      await this.keepNow(r, ids);
    } finally {
      this.keeping = false;
    }
  };

  private keepNow = async (r: [number, number], ids: number[]) => {
    const L = store.list;
    const keep = new Set(ids);
    const others: number[] = [];
    for (let i = r[0]; i < r[1]; i++) if (!keep.has(L.id[i])) others.push(L.id[i]);
    if (others.length) await store.reject(others);
    for (const id of [...ids, ...others]) this.photos.delete(id);
    if (!(await this.step(1))) this.leave();
  };

  /** After frames left the list: a series under review down to one frame is done (on to
   * the next), and one with none left ends the review. */
  afterDrop = () => {
    if (this.focus == null || this.keeping) return;
    const r = this.range();
    if (!r) {
      this.focus = null;
      return;
    }
    if (r[1] - r[0] <= 1) {
      setTimeout(() => {
        if (this.focus == null || this.keeping) return;
        this.step(1).then((ok) => {
          if (!ok) this.leave();
        });
      }, 350);
    }
  };

  /** Keeps the best frame of the series under review. */
  keepBestHere = () => {
    const r = this.range();
    if (!r) return;
    const L = store.list;
    let best = -1;
    for (let i = r[0]; i < r[1]; i++) if (L.sb?.[i] === 1) best = i;
    if (best < 0) best = r[0];
    return this.keep([L.id[best]]);
  };

  /** Frames left the list (rejected, moved): a series named after one of them takes the
   * name of its first frame left, so it stays unfolded (and under review). */
  rekey = (next: { id: number[]; sk?: number[] }, gone: Set<number>) => {
    const sk = next.sk;
    if (!sk) return;
    const renamed = new Map<number, number>();
    for (let i = 0; i < sk.length; i++) {
      const k = sk[i];
      if (!k || !gone.has(k)) continue;
      let to = renamed.get(k);
      if (to == null) {
        to = next.id[i];
        renamed.set(k, to);
      }
      sk[i] = to;
    }
    for (const [from, to] of renamed) {
      if (this.open.delete(from)) this.open.add(to);
      if (this.focus === from) this.focus = to;
    }
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
