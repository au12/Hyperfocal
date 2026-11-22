import { untrack } from 'svelte';
import { store, type Place } from './store.svelte';
import { people, type PeopleTab } from './people.svelte';

/** The browser's Back and Forward buttons.
 *
 * Every change of screen or window gets an entry in the browser's history: a folder, a
 * collection or a period, Locations, People & pets and a person's sheet, the single view,
 * the compare view, a dialog. Moving from one photo to another in the single view (or the
 * compare view's panes, or the grid's cursor) does not: it only updates the entry, so
 * Back goes to the screen before, showing the photo you were last on.
 *
 * Nothing calls this module when it navigates: an effect watches what is on screen and
 * pushes an entry when its key (below) changes, or rewrites the current one when only
 * details changed (the photo, the cursor, the search). Each entry carries a snapshot,
 * and Back or Forward restores it.
 *
 * Closing a dialog or a person's sheet onto the screen it opened on goes back instead of
 * pushing, so Back after closing a dialog doesn't open it again. Closing the single view
 * pushes: going back to the photo you looked at is the point. */

type Dialog = NonNullable<typeof store.dialog>;

/** Dialogs Back and Forward open again. Location and Send to Immich work on the photos
 * picked when they opened, so they stay out of the history. */
const RESTORABLE = new Set<Dialog>(['settings', 'help', 'journal', 'keywords', 'integrity', 'geotag', 'duplicates']);

/** Passing collections, keyed by what they show: one Duplicates group isn't the next. */
const PASSING = new Set(['dupes', 'like']);

interface Snap {
  place: Place;
  q: string;
  view: 'grid' | 'map' | 'people';
  /** The photo in the single view. */
  photo: number | null;
  /** The photo at the grid's cursor. */
  cursor: number | null;
  compare: number[] | null;
  active: number;
  tab: PeopleTab;
  /** The person or pet whose sheet is open (People & pets). */
  subject: number | null;
  dialog: Dialog | null;
}

interface Entry {
  hf: 1;
  key: string;
  /** The key of the entry this one was pushed from. */
  prev: string | null;
  snap: Snap;
}

const SEP = '\u0001';
/** Only details changed: the entry is rewritten this long after the last change. */
const REPLACE_MS = 300;

function snapshot(): Snap {
  const L = store.list;
  const view = store.view;
  const d = store.dialog;
  return {
    place: store.place(),
    q: store.query,
    view,
    photo: store.loupe ? (L.id[store.current] ?? null) : null,
    cursor: L.id[store.cursor] ?? null,
    compare: store.compare ? [...store.compare] : null,
    active: store.compareActive,
    tab: people.tab,
    subject: view === 'people' ? people.open : null,
    dialog: d && RESTORABLE.has(d) ? d : null,
  };
}

function placeKey(p: Place): string {
  if (p.k === 'c') return PASSING.has(String(p.c.id)) ? `c:${p.c.id}:${JSON.stringify(p.c.spec)}` : `c:${p.c.id}`;
  if (p.k === 'd') return p.r.nodate ? 'd:none' : `d:${p.r.from}_${p.r.to}`;
  return `f:${p.id}:${p.subj.join(',')}:${p.kw}`;
}

/** The screen without its dialog or sheet. */
const baseKey = (s: Snap) => [placeKey(s.place), s.view, s.photo != null ? 'loupe' : '', s.compare ? 'compare' : '', s.view === 'people' ? s.tab : ''].join(SEP);
const keyOf = (s: Snap) => [baseKey(s), s.dialog ?? '', s.subject ?? ''].join(SEP);

/** From `a` to `b` a dialog or a sheet closed, and nothing else changed. */
function closedOverlay(a: Snap, b: Snap): boolean {
  if (baseKey(a) !== baseKey(b)) return false;
  const dialog = a.dialog === b.dialog || b.dialog == null;
  const subject = a.subject === b.subject || b.subject == null;
  return dialog && subject && (a.dialog !== b.dialog || a.subject !== b.subject);
}

function entryNow(): Entry | null {
  const s = history.state as Entry | null;
  return s && s.hf === 1 ? s : null;
}

let started = false;
/** The key and snapshot of the current entry, as last seen. */
let lastKey: string | null = null;
let lastSnap: Snap | null = null;
/** A rewrite of the current entry is due. */
let dirty = false;
let replaceTimer: ReturnType<typeof setTimeout> | undefined;
/** Restoring an entry: what changes on screen meanwhile is not a new entry. */
let restoring = 0;
let target: Entry | null = null;
/** Went back after a dialog or sheet closed, waiting for the browser to get there. */
let popping = false;
let popTimer: ReturnType<typeof setTimeout> | undefined;
/** The key being left, for a push. */
let lastKeyBefore: string | null = null;

function write(push: boolean, snap: Snap, key: string) {
  const cur = entryNow();
  const entry: Entry = { hf: 1, key, prev: push ? lastKeyBefore : (cur?.prev ?? null), snap: $state.snapshot(snap) as Snap };
  try {
    if (push) history.pushState(entry, '', store.hashNow());
    else history.replaceState(entry, '', location.href);
  } catch {
    // Too many changes in a short time (Firefox and Safari refuse them).
  }
}
/** Rewrites the current entry with what is on screen now (photo, cursor, search). */
function flush() {
  clearTimeout(replaceTimer);
  if (!dirty || !lastSnap || !lastKey) return;
  dirty = false;
  write(false, lastSnap, lastKey);
}

function commit(snap: Snap) {
  if (restoring || popping) return;
  const key = keyOf(snap);
  if (key === lastKey) {
    lastSnap = snap;
    dirty = true;
    clearTimeout(replaceTimer);
    replaceTimer = setTimeout(flush, REPLACE_MS);
    return;
  }
  // The entry being left keeps the last photo, cursor and search it showed.
  flush();
  const before = lastSnap;
  lastKeyBefore = lastKey;
  lastKey = key;
  lastSnap = snap;
  const cur = entryNow();
  if (cur && before && cur.prev === key && closedOverlay(before, snap)) {
    // A dialog or sheet closed onto the screen it opened on: that is Back.
    popping = true;
    clearTimeout(popTimer);
    popTimer = setTimeout(() => {
      if (!popping) return;
      popping = false;
      const s = snapshot();
      lastKey = keyOf(s);
      lastSnap = s;
      write(false, s, lastKey);
    }, 1000);
    history.back();
    return;
  }
  write(true, snap, key);
}

function onPop(e: PopStateEvent) {
  const st = e.state as Entry | null;
  // An address typed by hand: store.onHash.
  if (!st || st.hf !== 1) return;
  clearTimeout(replaceTimer);
  dirty = false;
  if (popping) {
    popping = false;
    clearTimeout(popTimer);
    const s = snapshot();
    lastKey = keyOf(s);
    lastSnap = s;
    write(false, s, lastKey);
    return;
  }
  target = st;
  if (!restoring) void run();
}

async function run() {
  restoring++;
  try {
    while (target) {
      const t = target;
      target = null;
      try {
        await restore(t.snap);
      } catch {
        /* show what could be shown */
      }
    }
  } finally {
    restoring--;
    // The entry records what could be restored (a photo may have gone meanwhile).
    const s = snapshot();
    lastKey = keyOf(s);
    lastSnap = s;
    write(false, s, lastKey);
  }
}

async function restore(s: Snap) {
  if (store.dialog && store.dialog !== s.dialog) store.dialog = null;
  if (store.compare && !s.compare) store.closeCompare();
  const placeChanged = placeKey(store.place()) !== placeKey(s.place) || store.query !== s.q;
  if (placeChanged) {
    if (store.loupe) store.closeLoupe();
    await store.restorePlace(s.place, s.q);
  }
  if (s.view === 'people') {
    if (store.view !== 'people') store.openPeople();
    if (people.tab !== s.tab) people.setTab(s.tab);
    people.open = s.subject;
  } else {
    people.open = null;
    if (s.view === 'map') {
      if (store.view !== 'map') await store.openLocations();
    } else if (store.view !== 'grid') store.toGrid();
  }
  if (s.photo != null) {
    const i = store.index.get(s.photo);
    if (i != null) {
      if (!store.loupe || store.current !== i) store.openLoupe(i);
    } else if (store.loupe) store.closeLoupe();
  } else {
    if (store.loupe) store.closeLoupe();
    if (s.cursor != null && !s.compare) {
      const i = store.index.get(s.cursor);
      if (i != null) store.cursor = i;
    }
  }
  if (s.compare) {
    store.compareIds(s.compare);
    if (store.compare) store.setCompareActive(Math.min(s.active, store.compare.length - 1));
  }
  store.dialog = s.dialog;
}

export const nav = {
  /** Called once the library has opened what the address asked for. */
  start() {
    if (started) return;
    started = true;
    window.addEventListener('popstate', onPop);
    const s = snapshot();
    lastKey = keyOf(s);
    lastSnap = s;
    write(false, s, lastKey);
    $effect.root(() => {
      $effect(() => {
        const snap = snapshot();
        untrack(() => commit(snap));
      });
    });
  },
};
