import { store } from './store.svelte';

/** Full screen that behaves like a desktop app: Esc keeps its usual meaning (close the
 * photo, clear the selection, clear the search) and does not leave full screen. It is
 * left with the full screen button, F, or a quick double Esc on the library page.
 *
 * Browsers leave full screen on Esc themselves. Chromium browsers (Chrome, Edge, Brave,
 * Arc) let a page take the key with the Keyboard Lock API while it is in full screen;
 * holding Esc for two seconds still leaves (the browser's own escape hatch, which pages
 * cannot remove). Firefox and Safari have no such API: Firefox can be told with
 * `browser.fullscreen.exit_on_escape = false` in about:config; Safari always leaves. */

/** Two Esc presses closer than this leave full screen. */
const QUICK_MS = 400;
/** Presses further apart than this start counting again. */
const STREAK_MS = 3000;
/** How long the "press Esc twice quickly" hint stays up. */
const HINT_MS = 5500;

type KeyboardLock = { lock?: (keys?: string[]) => Promise<void>; unlock?: () => void };
const keyboard = (): KeyboardLock | undefined => (navigator as Navigator & { keyboard?: KeyboardLock }).keyboard;

class Fullscreen {
  on = $state(false);
  private lastEsc = 0;
  private streak = 0;
  private hintId: number | null = null;

  constructor() {
    if (typeof document === 'undefined') return;
    this.on = !!document.fullscreenElement;
    document.addEventListener('fullscreenchange', () => {
      this.on = !!document.fullscreenElement;
      this.resetEsc();
      if (this.on) {
        // Takes Esc away from the browser (Chromium; needs a secure context).
        keyboard()?.lock?.(['Escape']).catch(() => {});
      } else {
        keyboard()?.unlock?.();
        this.dismissHint();
      }
    });
  }

  supported = () => typeof document !== 'undefined' && !!document.documentElement.requestFullscreen;

  toggle = () => {
    if (document.fullscreenElement) this.exit();
    else document.documentElement.requestFullscreen?.().catch(() => {});
  };

  exit = () => {
    if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
  };

  /** An Esc anywhere but the library page: it does not count towards a double press. */
  resetEsc = () => {
    this.lastEsc = 0;
    this.streak = 0;
  };

  /** Esc on the library page. `act` does what Esc does there and says whether it did
   * anything. In full screen, a second press within QUICK_MS leaves full screen instead;
   * slower presses that find nothing else to do bring up a hint. */
  libraryEsc = (e: KeyboardEvent, act: () => boolean) => {
    if (e.repeat) return; // a held key is not a double press
    if (!this.on) {
      act();
      return;
    }
    const now = performance.now();
    if (this.lastEsc && now - this.lastEsc < QUICK_MS) {
      this.resetEsc();
      this.dismissHint();
      this.exit();
      return;
    }
    this.streak = this.lastEsc && now - this.lastEsc < STREAK_MS ? this.streak + 1 : 1;
    this.lastEsc = now;
    const acted = act();
    if (this.streak >= 2 && !acted) this.hint();
  };

  private hint() {
    this.dismissHint();
    this.hintId = store.toast('Esc keeps full screen on. Press it twice quickly to leave full screen.', 'info', { label: 'Exit full screen', run: this.exit }, HINT_MS);
  }

  private dismissHint() {
    if (this.hintId == null) return;
    const id = this.hintId;
    this.hintId = null;
    store.toasts = store.toasts.filter((t) => t.id !== id);
  }
}

export const fullscreen = new Fullscreen();
