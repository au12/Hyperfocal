// The user's arrangement of the side panels: the order of their sections and entries,
// and what is hidden. Kept with the settings, in this browser. Albums keep the library's
// own order (shared, see albums.arrange); only hiding one is per browser.

import { store } from './store.svelte';

export type ArrangeGroup = 'side' | 'side.library' | 'side.tools' | 'side.albums' | 'side.collections' | 'info';

export interface Arrangement {
  /** Ids in the user's order (elements it does not know keep their default place). */
  order?: string[];
  hidden?: string[];
}

/** The default order of the fixed groups. */
export const DEFAULT_ORDER = {
  side: ['library', 'albums', 'collections', 'tools', 'timeline', 'folders'],
  'side.library': ['all', 'people', 'locations', 'picks', 'fivestar', 'nogps', 'rejected'],
  'side.tools': ['keywords', 'geotag', 'duplicates', 'integrity'],
  info: ['rating', 'cull', 'text', 'people', 'albums', 'keywords', 'location', 'capture', 'file', 'credits'],
} as const;

/** Section names, for menus and the list of what is hidden. */
export const SECTION_NAMES: Record<string, string> = {
  library: 'Library',
  albums: 'Albums',
  collections: 'Collections',
  tools: 'Library tools',
  timeline: 'Timeline',
  folders: 'Folders',
};

export const INFO_NAMES: Record<string, string> = {
  rating: 'Rating & label',
  cull: 'Culling',
  text: 'Title & caption',
  people: 'People & pets',
  albums: 'Albums',
  keywords: 'Keywords',
  location: 'Location',
  capture: 'Capture',
  file: 'File',
  credits: 'Creator & copyright',
};

function get(g: ArrangeGroup): Arrangement {
  return store.settings.arrange?.[g] ?? {};
}

function put(g: ArrangeGroup, a: Arrangement) {
  const all = { ...(store.settings.arrange ?? {}) };
  if (!a.order?.length && !a.hidden?.length) delete all[g];
  else all[g] = a;
  store.settings.arrange = all;
  store.persist();
}

/** `ids` (what there is now, in its default order) in the user's order, hidden ones
 * included. An element the saved order does not know goes after the one before it in
 * the default order. */
export function ordered(g: ArrangeGroup, ids: readonly string[]): string[] {
  const saved = get(g).order;
  if (!saved?.length) return [...ids];
  const have = new Set(ids);
  const out = saved.filter((id) => have.has(id));
  const placed = new Set(out);
  ids.forEach((id, i) => {
    if (placed.has(id)) return;
    let at = 0;
    for (let k = i - 1; k >= 0; k--) {
      const j = out.indexOf(ids[k]);
      if (j >= 0) {
        at = j + 1;
        break;
      }
    }
    out.splice(at, 0, id);
    placed.add(id);
  });
  return out;
}

export const isHidden = (g: ArrangeGroup, id: string): boolean => !!get(g).hidden?.includes(id);

/** What is shown, in order. */
export function visible(g: ArrangeGroup, ids: readonly string[]): string[] {
  const h = new Set(get(g).hidden ?? []);
  return ordered(g, ids).filter((id) => !h.has(id));
}

/** What is hidden, in order. */
export function hiddenOf(g: ArrangeGroup, ids: readonly string[]): string[] {
  const h = new Set(get(g).hidden ?? []);
  return ordered(g, ids).filter((id) => h.has(id));
}

/** The order with `id` moved past its neighbour among the shown ones (`d` -1: up), or
 * null when it is already first (last). */
export function movedOrder(order: string[], hidden: (id: string) => boolean, id: string, d: -1 | 1): string[] | null {
  const i = order.indexOf(id);
  if (i < 0) return null;
  let j = i + d;
  while (j >= 0 && j < order.length && hidden(order[j])) j += d;
  if (j < 0 || j >= order.length) return null;
  const next = order[j];
  const out = order.filter((x) => x !== id);
  const k = out.indexOf(next);
  out.splice(d < 0 ? k : k + 1, 0, id);
  return out;
}

export function canMove(g: ArrangeGroup, ids: readonly string[], id: string, d: -1 | 1): boolean {
  return movedOrder(ordered(g, ids), (x) => isHidden(g, x), id, d) != null;
}

export function move(g: ArrangeGroup, ids: readonly string[], id: string, d: -1 | 1) {
  const next = movedOrder(ordered(g, ids), (x) => isHidden(g, x), id, d);
  if (next) put(g, { ...get(g), order: next });
}

export function hide(g: ArrangeGroup, id: string) {
  const a = get(g);
  if (a.hidden?.includes(id)) return;
  put(g, { ...a, hidden: [...(a.hidden ?? []), id] });
}

export function show(g: ArrangeGroup, id: string) {
  const a = get(g);
  put(g, { ...a, hidden: (a.hidden ?? []).filter((x) => x !== id) });
}

/** Back to the default order, everything shown. */
export function reset(...groups: ArrangeGroup[]) {
  const all = { ...(store.settings.arrange ?? {}) };
  for (const g of groups) delete all[g];
  store.settings.arrange = all;
  store.persist();
}
