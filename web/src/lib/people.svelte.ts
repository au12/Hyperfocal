// People & pets: the state of the People view and every change to who is who.
//
// The server does the matching and writes the names to the sidecars; this keeps
// what is on screen in step (the event stream says when something changed).

import { api, ApiError, type DetKind, type Detection, type PeopleStatus, type PhotoPeople, type Subject } from './api';
import { store } from './store.svelte';

export type PeopleTab = 'person' | 'pet';

/** A display name for a subject: its name, or what it is. */
export function subjectLabel(s: { name: string | null; kind: string }): string {
  if (s.name) return s.name;
  return s.kind === 'person' ? 'Unnamed person' : s.kind === 'cat' ? 'Unnamed cat' : 'Unnamed dog';
}

/** What a detection is, in words: "face", "dog", "cat". */
export const kindWord = (k: DetKind | string) => (k === 'face' ? 'face' : k === 'cat' ? 'cat' : 'dog');

/** The "not a …" of a detection's kind ("not a face", "not a dog or cat"), for n of them. */
export function notWord(k: DetKind | string, n = 1): string {
  if (k === 'face') return n === 1 ? 'not a face' : 'not faces';
  return n === 1 ? 'not a dog or cat' : 'not dogs or cats';
}

/** A detection nobody is named for yet, in words: "Unknown person", "Unknown cat". */
export function unknownLabel(d: Detection): string {
  if (d.who) return d.kind === 'face' ? 'Unnamed person' : `Unnamed ${kindWord(d.kind)}`;
  return d.kind === 'face' ? 'Unknown person' : `Unknown ${kindWord(d.kind)}`;
}

class People {
  status = $state<PeopleStatus | null>(null);
  tab = $state<PeopleTab>('person');
  named = $state.raw<Subject[]>([]);
  groups = $state.raw<Subject[]>([]);
  /** Unnamed groups with fewer photos than shown (one photo each). */
  groupsTotal = $state(0);
  hidden = $state.raw<Subject[]>([]);
  showHidden = $state(false);
  /** Show groups of a single photo too. */
  showSingles = $state(false);
  loading = $state(false);
  /** The subject whose sheet is open. */
  open = $state<number | null>(null);
  /** Bumped when who is in which photos changed (see `affects`). */
  rev = $state(0);
  /** Bumped by changes that may touch any photo (your edits, a regrouping). */
  allRev = $state(0);
  /** The detection under the pointer, in the info panel or on the photo: shown in both. */
  hover = $state<number | null>(null);
  private pending = new Set<number>();
  private pendingAll = false;
  private lastChanged = new Set<number>();
  private lastAll = true;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private abort: AbortController | null = null;
  private cache = new Map<number, Promise<PhotoPeople>>();
  private listAt = 0;
  private listTimer: ReturnType<typeof setTimeout> | null = null;
  private statusAt = 0;

  /** People and pets are available: face recognition is on, or names exist. */
  get enabled(): boolean {
    const s = this.status;
    return !!s && (s.status.enabled || s.people + s.pets + s.groups > 0);
  }

  loadStatus = async () => {
    this.statusAt = Date.now();
    try {
      this.status = await api.peopleStatus();
    } catch {
      /* keep the last one */
    }
  };

  load = async () => {
    this.listAt = Date.now();
    this.abort?.abort();
    const ctl = new AbortController();
    this.abort = ctl;
    this.loading = true;
    const kind = this.tab;
    try {
      const [named, groups, hidden] = await Promise.all([
        api.subjects({ kind, named: 'named', limit: 2000 }, ctl.signal),
        api.subjects({ kind, named: 'unnamed', min: this.showSingles ? 1 : 2, limit: 600 }, ctl.signal),
        this.showHidden ? api.subjects({ kind, hidden: true, limit: 2000 }, ctl.signal) : Promise.resolve({ subjects: [], total: 0 }),
      ]);
      if (ctl.signal.aborted) return;
      this.named = named.subjects;
      this.groups = groups.subjects;
      this.groupsTotal = groups.total;
      this.hidden = hidden.subjects;
    } catch (e) {
      if ((e as Error).name !== 'AbortError') store.toast(`People: ${(e as Error).message}`, 'error');
    } finally {
      if (this.abort === ctl) this.loading = false;
    }
  };

  /** Reload the lists soon, at most every few seconds (the analysis sends many events). */
  private scheduleLoad() {
    if (this.listTimer) return;
    const wait = Math.max(0, 4000 - (Date.now() - this.listAt));
    this.listTimer = setTimeout(() => {
      this.listTimer = null;
      if (store.view === 'people') this.load();
    }, wait);
  }

  setTab = (t: PeopleTab) => {
    if (this.tab === t) return;
    this.tab = t;
    this.named = [];
    this.groups = [];
    this.hidden = [];
    this.load();
  };

  /** Whether the last change (`rev`) may have touched this photo. */
  affects = (photo: number): boolean => this.lastAll || this.lastChanged.has(photo);

  /** The faces and animals of a photo (shared by the info panel and the overlay). */
  photo = (id: number): Promise<PhotoPeople> => {
    let p = this.cache.get(id);
    if (!p) {
      p = api.photoPeople(id);
      this.cache.set(id, p);
      p.catch(() => this.cache.delete(id));
      if (this.cache.size > 300) this.cache.delete(this.cache.keys().next().value!);
    }
    return p;
  };

  /** Something changed: these photos (null: possibly any). */
  private bump(photos: Set<number> | null) {
    this.lastAll = photos == null;
    this.lastChanged = photos ?? new Set();
    if (photos == null) {
      this.cache.clear();
      this.allRev++;
    } else for (const p of photos) this.cache.delete(p);
    this.rev++;
  }

  /** An event from the server. */
  onEvent = (e: { type: string; photos?: number[] } & Record<string, unknown>) => {
    if (e.type === 'people_status') {
      const { type, ...st } = e;
      void type;
      if (this.status) this.status = { ...this.status, status: st as unknown as PeopleStatus['status'] };
      else this.loadStatus();
      return;
    }
    // 'people': who is in these photos changed (none listed: many).
    const photos = e.photos ?? [];
    if (photos.length === 0) this.pendingAll = true;
    for (const p of photos) this.pending.add(p);
    if (this.timer) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.bump(this.pendingAll ? null : this.pending);
      this.pending = new Set();
      this.pendingAll = false;
      if (Date.now() - this.statusAt > 5000) this.loadStatus();
      if (store.view === 'people') this.scheduleLoad();
    }, 500);
  };

  private async run<T>(what: string, f: () => Promise<T>): Promise<T | null> {
    try {
      const r = await f();
      return r;
    } catch (e) {
      store.toast(`${what}: ${(e as Error).message}`, 'error');
      return null;
    } finally {
      this.loadStatus();
      if (store.view === 'people') this.load();
      this.bump(null);
    }
  }

  /** Name (or rename) a subject. A name someone else has: offer to merge the two. */
  rename = async (s: Subject, name: string): Promise<boolean> => {
    const n = name.trim().replace(/\s+/g, ' ');
    if (!n || n === s.name) return false;
    try {
      await api.updateSubject(s.id, { name: n });
      store.toast(s.name ? `Renamed to ${n}` : `Named ${n}${s.photos > 1 ? ` in ${s.photos} photos` : ''}`, 'ok');
      return true;
    } catch (e) {
      const m = e instanceof ApiError && e.status === 409 ? /^merge:(\d+)$/.exec(e.message) : null;
      if (m) {
        const other = Number(m[1]);
        if (confirm(`There is already ${s.kind === 'person' ? 'someone' : 'a pet'} called ${n}. Are these the same ${s.kind === 'person' ? 'person' : 'pet'}? Their photos will be merged.`)) {
          await this.merge([s.id], other, n);
          return true;
        }
        return false;
      }
      store.toast(`Not renamed: ${(e as Error).message}`, 'error');
      return false;
    } finally {
      this.loadStatus();
      if (store.view === 'people') this.load();
      this.bump(null);
    }
  };

  removeName = (s: Subject) => this.run('Name not removed', () => api.updateSubject(s.id, { name: null }));

  setHidden = (s: Subject, hidden: boolean) =>
    this.run(hidden ? 'Not hidden' : 'Not shown', async () => {
      await api.updateSubject(s.id, { hidden });
      store.toast(hidden ? `${subjectLabel(s)} is hidden` : `${subjectLabel(s)} is shown again`, 'ok', hidden ? { label: 'Undo', run: () => this.setHidden(s, false) } : undefined);
    });

  remove = (s: Subject) =>
    this.run('Not removed', async () => {
      const r = await api.deleteSubject(s.id);
      store.toast(`${subjectLabel(s)} removed: ${r.freed} face${r.freed === 1 ? '' : 's'} unknown again`, 'ok');
      if (this.open === s.id) this.open = null;
    });

  merge = (from: number[], into: number, name?: string) =>
    this.run('Not merged', async () => {
      const r = await api.mergeSubjects(from, into);
      store.toast(`Merged${name ? ` into ${name}` : ''}`, 'ok');
      if (this.open != null && from.includes(this.open)) this.open = r.subject;
      return r;
    });

  /** "This is …": by subject or by name (a new person or pet when nobody has it). */
  assign = (ids: number[], who: { subject?: number; name?: string }, label?: string) =>
    this.run('Not named', async () => {
      const r = await api.assign(ids, who);
      const n = ids.length;
      store.toast(`${label ?? who.name ?? 'Named'}${n > 1 ? `: ${n} faces` : ''}`, 'ok');
      return r;
    });

  confirm = (b: { ids?: number[]; subject?: number }) =>
    this.run('Not confirmed', async () => {
      const r = await api.confirm(b);
      if (r.confirmed > 1) store.toast(`${r.confirmed} confirmed`, 'ok');
      return r;
    });

  reject = (ids: number[]) => this.run('Not changed', () => api.rejectDetections(ids));

  /** "Not a face" (not a dog or cat): the detector got these wrong. Out of everything,
   * and kept out when the photos are analysed again. */
  notDetected = (ids: number[], kind: DetKind) =>
    this.run('Not changed', async () => {
      const r = await api.ignore({ ids });
      if (r.changed) this.toastNot(r, kind);
      return r;
    });

  /** None of a group's detections are what the detector took them for (a statue's
   * "faces", a pattern). */
  notGroup = (s: Subject) =>
    this.run('Not changed', async () => {
      const r = await api.ignore({ subject: s.id });
      if (this.open === s.id) this.open = null;
      if (r.changed) this.toastNot(r, s.kind === 'person' ? 'face' : s.kind);
      return r;
    });

  private toastNot(r: { changed: number; ids: number[] }, kind: DetKind | string) {
    const n = r.changed;
    store.toast(`${n === 1 ? 'Marked' : `${n.toLocaleString()} marked`} as ${notWord(kind, n)}`, 'ok', { label: 'Undo', run: () => this.restore(r.ids) });
  }

  /** Take back a "not a face". */
  restore = (ids: number[]) =>
    this.run('Not restored', async () => {
      const r = await api.ignore({ ids }, false);
      if (r.changed) store.toast(r.changed === 1 ? 'Restored' : `${r.changed.toLocaleString()} restored`, 'ok');
      return r;
    });

  /** "It's a dog" / "It's a cat": faces the face detector found on an animal (`face`:
   * back to a face). Matched to the pets once the AI engine has looked at them. */
  setKind = (ids: number[], kind: DetKind) => this.kindOf({ ids }, kind);

  /** A whole group of faces that are dogs or cats. */
  setGroupKind = (s: Subject, kind: DetKind) => {
    if (this.open === s.id) this.open = null;
    return this.kindOf({ subject: s.id }, kind);
  };

  private kindOf = (who: { ids?: number[]; subject?: number }, kind: DetKind) =>
    this.run('Not changed', async () => {
      const r = await api.setKind(who, kind);
      const n = r.changed;
      if (!n) return r;
      if (kind === 'face') store.toast(n === 1 ? 'A face again: the photo is analysed once more' : `${n.toLocaleString()} faces again: their photos are analysed once more`, 'ok');
      else
        store.toast(`${n === 1 ? 'Now a' : `${n.toLocaleString()} now`} ${kind}${n === 1 ? '' : 's'}: matched to your pets in a moment`, 'ok', {
          label: 'Undo',
          run: () => this.kindOf({ ids: r.ids }, 'face'),
        });
      return r;
    });

  pause = (paused: boolean) => this.run('Not changed', () => api.peopleControl({ pause: paused }));
  regroup = () =>
    this.run('Not started', async () => {
      await api.peopleControl({ regroup: true });
      store.toast('Forming the groups again: named people and pets stay as they are', 'info');
    });

  /** The photos of a subject in the grid. */
  showPhotos = (s: Subject) => {
    this.open = null;
    store.showSubject(s.id, subjectLabel(s));
  };

  /** Open the photo a detection is in, in the single view (among the subject's photos). */
  openPhoto = async (s: Subject, d: Detection) => {
    this.open = null;
    await store.showSubject(s.id, subjectLabel(s));
    const i = store.index.get(d.photo);
    if (i != null) store.openLoupe(i);
  };
}

export const people = new People();
