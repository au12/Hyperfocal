// Where the mouse is, so the zoom keys (Z, Space) can zoom about it. Tracked on the
// window, because a view that opens under a mouse that is not moving gets no move event
// of its own. A lifted finger points nowhere, and a mouse outside the window is unknown.

let at: { x: number; y: number } | null = null;

function track(e: PointerEvent) {
  at = e.pointerType === 'touch' ? null : { x: e.clientX, y: e.clientY };
}

if (typeof window !== 'undefined') {
  const opts = { capture: true, passive: true };
  window.addEventListener('pointermove', track, opts);
  window.addEventListener('pointerdown', track, opts);
  // Also when the page changes under a still mouse: a resize can report the mouse gone
  // (below) and then back.
  window.addEventListener('pointerover', track, opts);
  // No related target: the mouse left the window.
  window.addEventListener('pointerout', (e) => {
    if (!e.relatedTarget) at = null;
  }, opts);
}

/** The mouse position over `el` in device px from its top left corner, or null when the
 * mouse is not over it. */
export function pointerOver(el: Element | null | undefined): [number, number] | null {
  if (!at || !el) return null;
  const r = el.getBoundingClientRect();
  if (at.x < r.left || at.x >= r.right || at.y < r.top || at.y >= r.bottom) return null;
  const dpr = window.devicePixelRatio || 1;
  return [(at.x - r.left) * dpr, (at.y - r.top) * dpr];
}
