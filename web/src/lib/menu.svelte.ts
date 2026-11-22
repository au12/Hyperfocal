// A context menu: a right-click, the keyboard's menu key, or a held touch on an element
// opens what its host says can be done with it. One menu at a time, drawn by
// ContextMenu.svelte (mounted once in App) above everything.

export type MenuEntry =
  | { kind: 'label'; text: string }
  | { kind: 'sep' }
  | {
      kind: 'item';
      text: string;
      icon?: string;
      /** Small text at the right: a shortcut, or what kind of thing it is. */
      hint?: string;
      run: () => void;
      disabled?: boolean;
      danger?: boolean;
      /** Stays open after it runs (moving something step by step). */
      stay?: boolean;
    };

/** What a host builds for an element; read again whenever it changes. */
export type MenuBuilder = () => MenuEntry[];

class Menu {
  /** Where: at the pointer, or (a held touch) under the row it is about, so the finger
   * and the menu leave the row in sight. */
  at = $state<{ x: number; y: number; row?: { left: number; top: number; bottom: number } } | null>(null);
  build = $state.raw<MenuBuilder | null>(null);
  /** The element it is about: outlined while the menu is open. */
  private target: HTMLElement | null = null;

  open = (x: number, y: number, build: MenuBuilder, target: HTMLElement | null = null, row?: DOMRect) => {
    this.close();
    this.target = target;
    target?.classList.add('menu-target');
    this.build = build;
    this.at = { x, y, row: row ? { left: row.left, top: row.top, bottom: row.bottom } : undefined };
  };

  close = () => {
    this.target?.classList.remove('menu-target');
    this.target = null;
    this.at = null;
    this.build = null;
  };
}

export const menu = new Menu();

/** The click that follows (the end of a held touch, or a press that closed the menu)
 * is not a click on what is under it. */
export function swallowClick() {
  const stop = (e: Event) => {
    e.stopPropagation();
    e.preventDefault();
  };
  window.addEventListener('click', stop, { capture: true, once: true });
  setTimeout(() => window.removeEventListener('click', stop, { capture: true }), 800);
}

/** Held this long without moving, a touch opens the menu. */
const HOLD_MS = 500;
/** How far (px) a finger may wander before the hold counts as a scroll. */
const SLOP = 8;

export interface ContextMenuOptions {
  /** The menu for the element (the closest one under `node` with data-el, else `node`
   * itself) where the press was (`target`); null leaves the browser's own menu. */
  build: (el: HTMLElement, target: HTMLElement) => MenuBuilder | null;
  /** A held touch only counts on elements matching this selector (default: anywhere), so
   * holding text in a panel still selects it. */
  hold?: string;
}

/** Where the browser's own menu is the right one: fields, links, selected text. */
function native(t: HTMLElement): boolean {
  if (t.closest('input, textarea, select, [contenteditable=""], [contenteditable="true"], a[href]')) return true;
  const sel = window.getSelection();
  return !!sel && !sel.isCollapsed && sel.containsNode(t, true);
}

/** Svelte action: context menus for the elements under `node`. */
export function contextMenu(node: HTMLElement, options: ContextMenuOptions) {
  let opts = options;
  let timer = 0;
  let sx = 0;
  let sy = 0;
  let pointer = -1;
  let heldAt = 0;

  function openFor(t: HTMLElement, x: number, y: number, touch = false): boolean {
    if (native(t)) return false;
    const el = (t.closest('[data-el]') as HTMLElement | null) ?? node;
    if (!node.contains(el)) return false;
    const build = opts.build(el, t);
    if (!build) return false;
    // A held touch: the row under the finger (not a whole section) stays visible.
    const row = touch ? ((t.closest('.item, .sec-title, .sec-head, .lab, .rate-row, .row') as HTMLElement | null) ?? el).getBoundingClientRect() : undefined;
    menu.open(x, y, build, el === node ? null : el, row);
    return true;
  }

  function cancel() {
    clearTimeout(timer);
    timer = 0;
  }

  function down(e: PointerEvent) {
    if (e.pointerType === 'mouse' || !e.isPrimary) return;
    cancel();
    const t = e.target as HTMLElement;
    if (opts.hold && !t.closest(opts.hold)) return;
    sx = e.clientX;
    sy = e.clientY;
    pointer = e.pointerId;
    timer = window.setTimeout(() => {
      timer = 0;
      if (!openFor(t, sx, sy, true)) return;
      heldAt = Date.now();
      navigator.vibrate?.(12);
      swallowClick();
    }, HOLD_MS);
  }

  function move(e: PointerEvent) {
    if (timer && e.pointerId === pointer && Math.hypot(e.clientX - sx, e.clientY - sy) > SLOP) cancel();
  }

  function contextmenu(e: MouseEvent) {
    // Android sends its own contextmenu for a held touch: the hold already opened it.
    if (Date.now() - heldAt < 1500) {
      e.preventDefault();
      return;
    }
    cancel();
    // Something inside has its own menu (a face's, in the info panel).
    if (e.defaultPrevented) return;
    const t = e.target as HTMLElement;
    let x = e.clientX;
    let y = e.clientY;
    if (!x && !y) {
      // The keyboard's menu key: under the focused element.
      const r = ((t.closest('[data-el]') as HTMLElement | null) ?? t).getBoundingClientRect();
      x = r.left + 16;
      y = r.top + Math.min(r.height, 32);
    }
    if (openFor(t, x, y)) e.preventDefault();
  }

  node.addEventListener('pointerdown', down);
  node.addEventListener('pointermove', move);
  node.addEventListener('pointerup', cancel);
  node.addEventListener('pointercancel', cancel);
  node.addEventListener('contextmenu', contextmenu);
  return {
    update(o: ContextMenuOptions) {
      opts = o;
    },
    destroy() {
      cancel();
      node.removeEventListener('pointerdown', down);
      node.removeEventListener('pointermove', move);
      node.removeEventListener('pointerup', cancel);
      node.removeEventListener('pointercancel', cancel);
      node.removeEventListener('contextmenu', contextmenu);
    },
  };
}
