// Motion: the transitions, actions and small animation helpers the UI shares. Every one
// of them collapses to nothing when the system asks for less motion.

import type { TransitionConfig } from 'svelte/transition';

const mq = typeof window !== 'undefined' ? matchMedia('(prefers-reduced-motion: reduce)') : null;

/** The system asks for less motion. */
export function reduced(): boolean {
  return !!mq?.matches;
}

/** easeOutExpo-like: fast start, long soft landing. */
export const easeOut = (t: number) => (t === 1 ? 1 : 1 - Math.pow(2, -10 * t));
/** A little overshoot, for things that pop into place. */
export const easeSpring = (t: number) => {
  const c1 = 1.70158 * 1.2;
  const c3 = c1 + 1;
  return 1 + c3 * Math.pow(t - 1, 3) + c1 * Math.pow(t - 1, 2);
};

function d(ms: number) {
  return reduced() ? 0 : ms;
}

/** Scale and fade from a point: popovers, menus, badges. */
export function pop(node: Element, { duration = 180, y = -4, scale = 0.96, origin = '' } = {}): TransitionConfig {
  if (origin) (node as HTMLElement).style.transformOrigin = origin;
  return {
    duration: d(duration),
    easing: easeOut,
    css: (t, u) => `opacity:${t};transform:translateY(${u * y}px) scale(${scale + (1 - scale) * t})`,
  };
}

/** Rise into place: dialogs, cards, toasts. */
export function rise(node: Element, { duration = 300, y = 14, scale = 0.98, delay = 0 } = {}): TransitionConfig {
  return {
    duration: d(duration),
    delay: d(delay),
    easing: easeOut,
    css: (t, u) => `opacity:${t};transform:translateY(${u * y}px) scale(${scale + (1 - scale) * t})`,
  };
}

/** Slide in from a side (drawers, sheets): x and y in % of the element. */
export function slide(node: Element, { duration = 280, x = 0, y = 100 } = {}): TransitionConfig {
  return {
    duration: d(duration),
    easing: easeOut,
    css: (_t, u) => `transform:translate(${u * x}%, ${u * y}%)`,
  };
}

/** Plain fade. */
export function fade(node: Element, { duration = 180, delay = 0 } = {}): TransitionConfig {
  return { duration: d(duration), delay: d(delay), css: (t) => `opacity:${t}` };
}

/** Height reveal for expanding lists (tree children, details). */
export function reveal(node: Element, { duration = 220 } = {}): TransitionConfig {
  const h = (node as HTMLElement).offsetHeight;
  return {
    duration: d(duration),
    easing: easeOut,
    css: (t) => `overflow:hidden;height:${t * h}px;opacity:${Math.min(1, t * 1.6)}`,
  };
}

/**
 * Segmented control: an indicator slides to the button that is `.on`. Put it on the
 * `.seg` element; it follows class changes and size changes by itself.
 */
export function segmented(node: HTMLElement) {
  const ind = document.createElement('span');
  ind.className = 'seg-ind';
  ind.setAttribute('aria-hidden', 'true');
  node.prepend(ind);
  node.classList.add('slides');
  let first = true;
  const place = () => {
    const on = node.querySelector<HTMLElement>(':scope > button.on');
    if (!on) {
      ind.style.opacity = '0';
      return;
    }
    const x = on.offsetLeft;
    const w = on.offsetWidth;
    if (first || reduced()) {
      // No slide on the first placement: the indicator just appears under the option.
      ind.style.transition = 'none';
      ind.style.transform = `translateX(${x}px)`;
      ind.style.width = `${w}px`;
      void ind.offsetWidth;
      ind.style.transition = '';
      first = false;
    } else {
      ind.style.transform = `translateX(${x}px)`;
      ind.style.width = `${w}px`;
    }
    ind.style.opacity = '1';
  };
  const mo = new MutationObserver(place);
  mo.observe(node, { subtree: true, attributes: true, attributeFilter: ['class'], childList: true });
  const ro = new ResizeObserver(place);
  ro.observe(node);
  requestAnimationFrame(place);
  return {
    destroy() {
      mo.disconnect();
      ro.disconnect();
      ind.remove();
    },
  };
}

/**
 * Re-runs a CSS animation class on an element whenever `key` changes to a new value (a
 * rating set, a flag toggled): `use:bump={value}`. A null or false key records the
 * change without animating (something switched off). The class is `pop-once` unless
 * the element names another in `data-bump`.
 */
export function bump(node: HTMLElement, key: unknown) {
  let last = key;
  const cls = node.dataset.bump || 'pop-once';
  return {
    update(k: unknown) {
      if (k === last) return;
      last = k;
      if (k == null || k === false || reduced()) return;
      node.classList.remove(cls);
      void node.offsetWidth;
      node.classList.add(cls);
    },
  };
}

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/**
 * The shared-element zoom between a grid tile and the single view: a copy of the
 * thumbnail flies from one rectangle to the other, over everything, then fades out.
 * The box changes shape as it flies and the picture is re-cropped to it each frame, so
 * nothing is stretched when the two rectangles have different proportions. Resolves
 * when it has landed (at once with less motion).
 */
export function flyImage(src: string, from: Rect, to: Rect, opts: { duration?: number; radius?: [number, number]; fadeOut?: number } = {}): Promise<void> {
  const duration = opts.duration ?? 320;
  if (reduced() || !src || from.w < 2 || to.w < 2) return Promise.resolve();
  const box = document.createElement('div');
  const img = document.createElement('img');
  img.src = src;
  img.alt = '';
  img.decoding = 'sync';
  Object.assign(img.style, { display: 'block', width: '100%', height: '100%', objectFit: 'cover' } satisfies Partial<CSSStyleDeclaration>);
  const [r0, r1] = opts.radius ?? [6, 0];
  Object.assign(box.style, {
    position: 'fixed',
    left: '0',
    top: '0',
    overflow: 'hidden',
    zIndex: '95',
    pointerEvents: 'none',
    willChange: 'transform, width, height',
    contain: 'layout paint',
    boxShadow: '0 30px 80px -20px rgba(0,0,0,.7)',
  } satisfies Partial<CSSStyleDeclaration>);
  box.appendChild(img);
  document.body.appendChild(box);
  const frame = (r: Rect, radius: number) => ({ transform: `translate(${r.x}px, ${r.y}px)`, width: `${r.w}px`, height: `${r.h}px`, borderRadius: `${radius}px` });
  const anim = box.animate([frame(from, r0), frame(to, r1)], { duration, easing: 'cubic-bezier(0.16, 1, 0.3, 1)', fill: 'forwards' });
  return anim.finished
    .catch(() => {})
    .then(() => {
      const out = box.animate([{ opacity: 1 }, { opacity: 0 }], { duration: opts.fadeOut ?? 140, fill: 'forwards' });
      return out.finished.catch(() => {});
    })
    .then(() => box.remove());
}
