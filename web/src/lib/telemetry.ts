// Debug mode (HYPERFOCAL_DEBUG on the server): what this browser sees, for the debug
// report. Errors, how long the viewer and the grid take, every request as the browser saw
// it (with the server's share from Server-Timing), the event stream, long tasks and slow
// interactions, dropped frames while you scroll or pan, memory, battery and network.
// A report goes to the server about once a minute while the page is visible, and one
// when it is hidden; nothing runs in the background while it is hidden.
//
// Nothing here does anything until the server says debug mode is on (`tel.start`):
// before that, every call is one boolean check.

const BASE = new URL('./api/', document.baseURI).pathname;
/** Opens "Mark a moment" anywhere in the app (components/DebugBadge.svelte), unless the
 * user gave the key to an action. */
export const MARK_KEY = 'Alt+Shift+M';
const REPORT_MS = 60_000;
const MB = 1048576;

/** Same buckets as the server (server/src/telemetry/hist.rs): 0-7 µs exact, then four
 * per power of two. */
function bucket(us: number): number {
  const u = Math.min(Math.max(0, Math.round(us)), 0xffffffff);
  if (u < 8) return u;
  const log = 31 - Math.clz32(u);
  const sub = (u >>> (log - 2)) & 3;
  return Math.min(127, 8 + (log - 3) * 4 + sub);
}

class Hist {
  n = 0;
  err = 0;
  sum = 0;
  max = 0;
  b = new Map<number, number>();
  add(ms: number, err = false) {
    const us = Math.max(0, ms * 1000);
    this.n++;
    if (err) this.err++;
    this.sum += us;
    if (us > this.max) this.max = us;
    const i = bucket(us);
    this.b.set(i, (this.b.get(i) ?? 0) + 1);
  }
  json() {
    return { n: this.n, err: this.err, sum_us: Math.round(this.sum), max_us: Math.round(this.max), b: Object.fromEntries(this.b) };
  }
}

interface Res {
  n: number;
  bytes: number;
  cached: number;
  sw: number;
  dur: Hist;
  srv: Hist;
}

interface Err {
  kind: string;
  msg: string;
  n: number;
  src?: string;
  stack?: string;
  ctx?: Record<string, unknown>;
}

const rid = (p: string) => p + Math.random().toString(36).slice(2, 10) + Date.now().toString(36).slice(-4);

function deviceId(): string {
  try {
    let d = localStorage.getItem('hf.dev');
    if (!d) {
      d = rid('d');
      localStorage.setItem('hf.dev', d);
    }
    return d;
  } catch {
    return rid('d');
  }
}

/** A request's route, as the server names them ("/api/photo/{id}/thumb"); null for the
 * telemetry's own requests. */
export function routeOf(pathname: string): string | null {
  const i = pathname.indexOf('/api/');
  if (i >= 0) {
    const p = pathname.slice(i);
    if (p.startsWith('/api/debug/')) return null;
    return p
      .split('/')
      .map((s) => (/^\d+$/.test(s) ? '{id}' : s))
      .join('/');
  }
  if (pathname.includes('/assets/')) return '/assets/*' + (pathname.match(/\.[a-z0-9]+$/i)?.[0] ?? '');
  if (pathname.includes('/fonts/')) return '/fonts/*';
  return '/ (app files)';
}

function describe(el: unknown): string {
  const e = el as Element | null;
  if (!e || !e.tagName) return '';
  const cls = [...(e.classList ?? [])].find((c) => !c.startsWith('svelte-'));
  return e.tagName.toLowerCase() + (e.id ? '#' + e.id : '') + (cls ? '.' + cls : '');
}

const round1 = (x: number) => Math.round(x * 10) / 10;

class Telemetry {
  on = false;
  readonly page = rid('p');
  readonly dev = deviceId();
  /** What is on screen (set by the app): attached to errors, slow interactions and marks. */
  context: () => Record<string, unknown> = () => ({});
  private version = '';
  private seq = 0;
  private envSent = false;
  private timings = new Map<string, Hist>();
  private counts = new Map<string, number>();
  private res = new Map<string, Res>();
  private sse = new Map<string, { n: number; bytes: number; hidden: number }>();
  private long = { n: 0, ms: 0, max: 0 };
  private loaf = new Map<string, { n: number; ms: number }>();
  private events: Record<string, unknown>[] = [];
  private frames = new Map<string, { n: number; dropped: number; worst: number }>();
  private errors = new Map<string, Err>();
  private gauges = new Map<string, () => number | undefined>();
  private observers: PerformanceObserver[] = [];
  private timer: ReturnType<typeof setInterval> | undefined;
  private battery: { level: number; charging: boolean } | null = null;
  private storageMb: number | undefined;
  // Visible and hidden time since the last report.
  private visAt = performance.now();
  private visibleMs = 0;
  private hiddenMs = 0;
  // Frame pacing while the user interacts (see onInput).
  private activeUntil = 0;
  private raf = 0;
  private lastFrame = 0;
  private budget = 1000 / 60;
  private budgetSamples: number[] = [];
  /** Since this page loaded, for the debug panel. */
  readonly totals = { errors: 0, requests: 0, bytes: 0, longN: 0, longMs: 0, frames: 0, dropped: 0, sseN: 0, sseBytes: 0, reports: 0 };

  /** Debug mode is on: start collecting. */
  start(o: { version: string }) {
    if (this.on) return;
    this.on = true;
    this.version = o.version;
    // For the browser's console: hfTelemetry.snapshot(), hfTelemetry.flush().
    (window as unknown as { hfTelemetry: Telemetry }).hfTelemetry = this;
    window.addEventListener('error', this.onError, true);
    window.addEventListener('unhandledrejection', this.onRejection);
    document.addEventListener('visibilitychange', this.onVisibility);
    window.addEventListener('pagehide', this.onPageHide);
    for (const ev of ['pointerdown', 'pointermove', 'wheel', 'keydown', 'touchmove', 'scroll']) {
      window.addEventListener(ev, this.onInput, { passive: true, capture: true });
    }
    this.observe();
    this.wrapConsole();
    const nav = navigator as Navigator & { getBattery?: () => Promise<{ level: number; charging: boolean }> };
    nav.getBattery?.().then((b) => (this.battery = b)).catch(() => {});
    if (!document.hidden) this.schedule();
    // The first report soon: what this browser is, and how long the page took to load.
    setTimeout(() => this.flush(), 5000);
  }

  count(name: string, n = 1) {
    if (!this.on) return;
    this.counts.set(name, (this.counts.get(name) ?? 0) + n);
  }

  /** A duration (ms) of something worth watching ("viewer.show.raw"). */
  time(name: string, ms: number, err = false) {
    if (!this.on || !(ms >= 0)) return;
    let h = this.timings.get(name);
    if (!h) this.timings.set(name, (h = new Hist()));
    h.add(ms, err);
  }

  error(kind: string, msg: string, extra?: { src?: string; stack?: string }) {
    if (!this.on) return;
    this.totals.errors++;
    const m = String(msg).slice(0, 400);
    const key = kind + '|' + m;
    const e = this.errors.get(key);
    if (e) e.n++;
    else if (this.errors.size < 100) this.errors.set(key, { kind, msg: m, n: 1, src: extra?.src, stack: extra?.stack?.slice(0, 2000), ctx: this.ctx() });
  }

  /** A message on the event stream (`type`, `bytes` long). */
  sseMessage(type: string, bytes: number) {
    if (!this.on) return;
    this.totals.sseN++;
    this.totals.sseBytes += bytes;
    let s = this.sse.get(type);
    if (!s) this.sse.set(type, (s = { n: 0, bytes: 0, hidden: 0 }));
    s.n++;
    s.bytes += bytes;
    if (document.hidden) s.hidden++;
  }

  /** A number to report with memory (sampled at each report). Fine to call before start. */
  gauge(name: string, f: () => number | undefined) {
    this.gauges.set(name, f);
  }

  /** Marks this moment in the server's telemetry, with a note and what is on screen. */
  async mark(note: string): Promise<boolean> {
    if (!this.on) return false;
    this.flush();
    try {
      const r = await fetch(BASE + 'debug/mark', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify({ note, dev: this.dev, ctx: { ...this.ctx(), hash: location.hash, mem: this.mem() } }),
      });
      return r.ok;
    } catch {
      return false;
    }
  }

  /** Errors collected since the last report (for the browser's console). */
  pending() {
    return [...this.errors.values()];
  }

  /** This page's numbers so far, for the debug panel. */
  snapshot() {
    const t = this.totals;
    const m = this.mem();
    return {
      errors: t.errors,
      requests: t.requests,
      mb: round1(t.bytes / MB),
      long: t.longN,
      longS: round1(t.longMs / 1000),
      dropped: t.frames + t.dropped > 0 ? round1((t.dropped * 100) / (t.frames + t.dropped)) : null,
      sse: t.sseN,
      sseKb: Math.round(t.sseBytes / 1024),
      jsMb: m.js_used_mb,
      bitmapsMb: m.bitmaps_mb,
      gpuMb: m.gpu_mb,
      battery: this.battery ? { level: this.battery.level, charging: this.battery.charging } : null,
      reports: t.reports,
    };
  }

  // ---------------------------------------------------------------- collection

  private ctx(): Record<string, unknown> {
    try {
      return this.context();
    } catch {
      return {};
    }
  }

  private view(): string {
    const c = this.ctx();
    return c.loupe ? 'loupe' : c.compare ? 'compare' : String(c.view ?? 'app');
  }

  private observe() {
    const types: readonly string[] = PerformanceObserver.supportedEntryTypes ?? [];
    const watch = (type: string, cb: (e: PerformanceEntry) => void, opts: Record<string, unknown> = {}) => {
      if (!types.includes(type)) return;
      try {
        const o = new PerformanceObserver((l) => {
          for (const e of l.getEntries()) cb(e);
        });
        o.observe({ type, buffered: true, ...opts } as PerformanceObserverInit);
        this.observers.push(o);
      } catch {
        /* not supported here */
      }
    };
    watch('resource', (e) => this.onResource(e as PerformanceResourceTiming));
    // Long animation frames say which script was busy; long tasks only that something was.
    if (types.includes('long-animation-frame')) watch('long-animation-frame', (e) => this.onLoaf(e));
    else watch('longtask', (e) => this.onLong(e.duration));
    watch('event', (e) => this.onEventTiming(e), { durationThreshold: 104 });
  }

  private onResource(e: PerformanceResourceTiming) {
    let route: string | null;
    try {
      const u = new URL(e.name, location.href);
      if (u.origin !== location.origin) return;
      route = routeOf(u.pathname);
    } catch {
      return;
    }
    if (!route) return;
    let r = this.res.get(route);
    if (!r) {
      if (this.res.size >= 200) return;
      this.res.set(route, (r = { n: 0, bytes: 0, cached: 0, sw: 0, dur: new Hist(), srv: new Hist() }));
    }
    r.n++;
    r.bytes += e.transferSize || 0;
    if (!e.transferSize && e.decodedBodySize > 0) {
      if (e.workerStart > 0) r.sw++;
      else r.cached++;
    }
    r.dur.add(e.duration);
    const st = (e as PerformanceResourceTiming & { serverTiming?: { name: string; duration: number }[] }).serverTiming?.find((s) => s.name === 'app');
    if (st) r.srv.add(st.duration);
    this.totals.requests++;
    this.totals.bytes += e.transferSize || 0;
    if (document.hidden) this.count('hidden.requests');
  }

  private onLong(ms: number) {
    this.long.n++;
    this.long.ms += ms;
    if (ms > this.long.max) this.long.max = ms;
    this.totals.longN++;
    this.totals.longMs += ms;
    if (document.hidden) this.count('hidden.long_tasks');
  }

  private onLoaf(e: PerformanceEntry) {
    const f = e as PerformanceEntry & { blockingDuration?: number; scripts?: { invoker?: string; invokerType?: string; sourceURL?: string; sourceFunctionName?: string; sourceCharPosition?: number; duration: number }[] };
    this.onLong(e.duration);
    for (const s of f.scripts ?? []) {
      if (s.duration < 20) continue;
      const file = (s.sourceURL ?? '').split('/').pop() ?? '';
      const key = `${s.invokerType ?? ''} ${s.invoker ?? ''} → ${s.sourceFunctionName || '(anonymous)'} @ ${file}:${s.sourceCharPosition ?? -1}`;
      let a = this.loaf.get(key);
      if (!a) {
        if (this.loaf.size >= 100) continue;
        this.loaf.set(key, (a = { n: 0, ms: 0 }));
      }
      a.n++;
      a.ms += s.duration;
    }
  }

  private onEventTiming(e: PerformanceEntry) {
    const ev = e as PerformanceEntry & { processingStart: number; processingEnd: number; target?: unknown };
    this.time('input.' + e.name, e.duration);
    if (e.duration >= 200 && this.events.length < 30) {
      this.events.push({
        type: e.name,
        target: describe(ev.target),
        ms: Math.round(e.duration),
        delay: Math.round(ev.processingStart - e.startTime),
        proc: Math.round(ev.processingEnd - ev.processingStart),
        view: this.view(),
      });
    }
  }

  /** Frames are timed only while the user is interacting (and a second after), so the
   * measuring never keeps an idle page awake. */
  private onInput = () => {
    this.activeUntil = performance.now() + 1000;
    if (!this.raf && !document.hidden) {
      this.lastFrame = 0;
      this.raf = requestAnimationFrame(this.frame);
    }
  };

  private frame = (t: number) => {
    this.raf = 0;
    if (this.lastFrame) {
      const dt = t - this.lastFrame;
      if (this.budgetSamples.length < 90) {
        this.budgetSamples.push(dt);
        if (this.budgetSamples.length === 90) {
          // The display's refresh interval: the median frame, snapped to a common rate.
          const med = [...this.budgetSamples].sort((a, b) => a - b)[45];
          this.budget = [1000 / 144, 1000 / 120, 1000 / 90, 1000 / 60, 1000 / 50, 1000 / 30].reduce((a, b) => (Math.abs(b - med) < Math.abs(a - med) ? b : a));
        }
      }
      const v = this.view();
      let f = this.frames.get(v);
      if (!f) this.frames.set(v, (f = { n: 0, dropped: 0, worst: 0 }));
      f.n++;
      this.totals.frames++;
      if (dt > this.budget * 1.5) {
        const missed = Math.round(dt / this.budget) - 1;
        f.dropped += missed;
        this.totals.dropped += missed;
      }
      if (dt > f.worst) f.worst = dt;
    }
    this.lastFrame = t;
    if (t < this.activeUntil && !document.hidden) this.raf = requestAnimationFrame(this.frame);
    else this.lastFrame = 0;
  };

  private onError = (e: Event) => {
    const t = e.target as (Element & { src?: string; currentSrc?: string; href?: string }) | null;
    if (t && (t as unknown) !== window && t.tagName) {
      // A resource that failed to load (an image, a script).
      const src = t.currentSrc || t.src || t.href || '';
      let where = src;
      try {
        where = routeOf(new URL(src, location.href).pathname) ?? src;
      } catch {
        /* keep the address */
      }
      this.error('resource', `${t.tagName.toLowerCase()} failed to load: ${where}`, { src: src.slice(0, 300) });
      return;
    }
    const ev = e as ErrorEvent;
    const file = (ev.filename ?? '').split('/').pop();
    this.error('js', ev.message || String(ev.error), { src: `${file}:${ev.lineno}:${ev.colno}`, stack: (ev.error as Error | undefined)?.stack });
  };

  private onRejection = (e: PromiseRejectionEvent) => {
    const r = e.reason as { name?: string; message?: string; stack?: string } | undefined;
    // A cancelled request is how the viewer stops loading what it moved past: not an error.
    if (r?.name === 'AbortError') {
      this.count('aborted');
      return;
    }
    this.error('promise', r?.message ?? String(r), { stack: r?.stack });
  };

  private wrapConsole() {
    for (const level of ['error', 'warn'] as const) {
      const orig = console[level].bind(console);
      console[level] = (...args: unknown[]) => {
        try {
          this.error('console.' + level, args.map((a) => (a instanceof Error ? a.message : typeof a === 'string' ? a : JSON.stringify(a))).join(' '), {
            stack: args.find((a): a is Error => a instanceof Error)?.stack,
          });
        } catch {
          /* never break logging */
        }
        orig(...args);
      };
    }
  }

  private onVisibility = () => {
    const now = performance.now();
    if (document.hidden) {
      this.visibleMs += now - this.visAt;
      this.visAt = now;
      if (this.raf) cancelAnimationFrame(this.raf);
      this.raf = 0;
      this.unschedule();
      this.flush(true);
    } else {
      this.hiddenMs += now - this.visAt;
      this.visAt = now;
      this.schedule();
    }
  };

  private lastFlush = 0;
  private onPageHide = () => {
    // Usually just sent, as the page was hidden.
    if (performance.now() - this.lastFlush > 2000) this.flush(true);
  };

  private schedule() {
    this.unschedule();
    this.timer = setInterval(() => this.flush(), REPORT_MS);
  }

  private unschedule() {
    if (this.timer) clearInterval(this.timer);
    this.timer = undefined;
  }

  private mem(): Record<string, number> {
    const m: Record<string, number> = {};
    const pm = (performance as Performance & { memory?: { usedJSHeapSize: number; totalJSHeapSize: number; jsHeapSizeLimit: number } }).memory;
    if (pm) {
      m.js_used_mb = round1(pm.usedJSHeapSize / MB);
      m.js_total_mb = round1(pm.totalJSHeapSize / MB);
      m.js_limit_mb = Math.round(pm.jsHeapSizeLimit / MB);
    }
    for (const [k, f] of this.gauges) {
      try {
        const v = f();
        if (v != null && isFinite(v)) m[k] = round1(v);
      } catch {
        /* a gauge that fails is left out */
      }
    }
    m.dom = document.getElementsByTagName('*').length;
    if (this.storageMb != null) m.storage_mb = this.storageMb;
    return m;
  }

  private env() {
    const nav = navigator as Navigator & { deviceMemory?: number; userAgentData?: { platform?: string }; standalone?: boolean };
    let gpu: string | undefined;
    let maxTex: number | undefined;
    try {
      const cv = document.createElement('canvas');
      const gl = (cv.getContext('webgl2') ?? cv.getContext('webgl')) as WebGLRenderingContext | null;
      if (gl) {
        const ext = gl.getExtension('WEBGL_debug_renderer_info');
        gpu = String(gl.getParameter(ext ? ext.UNMASKED_RENDERER_WEBGL : gl.RENDERER));
        maxTex = gl.getParameter(gl.MAX_TEXTURE_SIZE);
        gl.getExtension('WEBGL_lose_context')?.loseContext();
      }
    } catch {
      /* no WebGL */
    }
    const ne = performance.getEntriesByType('navigation')[0] as PerformanceNavigationTiming | undefined;
    return {
      ua: navigator.userAgent,
      platform: nav.userAgentData?.platform ?? navigator.platform,
      mobile: matchMedia('(pointer: coarse)').matches,
      vw: innerWidth,
      vh: innerHeight,
      dpr: devicePixelRatio,
      screen: [screen.width, screen.height],
      cores: navigator.hardwareConcurrency,
      mem_gb: nav.deviceMemory,
      gpu,
      max_tex: maxTex,
      pwa: matchMedia('(display-mode: standalone)').matches || nav.standalone === true,
      proto: ne?.nextHopProtocol,
      sw: !!navigator.serviceWorker?.controller,
      tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
      lang: navigator.language,
      app: this.version,
      battery_api: 'getBattery' in navigator,
      loaf: (PerformanceObserver.supportedEntryTypes ?? []).includes('long-animation-frame'),
    };
  }

  private nav() {
    const ne = performance.getEntriesByType('navigation')[0] as PerformanceNavigationTiming | undefined;
    if (!ne) return undefined;
    let assetsKb = 0;
    for (const r of performance.getEntriesByType('resource') as PerformanceResourceTiming[]) {
      if (r.name.includes('/assets/')) assetsKb += (r.transferSize || 0) / 1024;
    }
    return {
      type: ne.type,
      ttfb: Math.round(ne.responseStart),
      dom: Math.round(ne.domContentLoadedEventEnd),
      load: Math.round(ne.loadEventEnd),
      html_kb: Math.round((ne.transferSize || 0) / 1024),
      assets_kb: Math.round(assetsKb),
    };
  }

  /** Sends what was collected since the last report (`beacon`: the page is going away). */
  flush(beacon = false) {
    if (!this.on) return;
    const now = performance.now();
    this.lastFlush = now;
    if (document.hidden) this.hiddenMs += now - this.visAt;
    else this.visibleMs += now - this.visAt;
    this.visAt = now;
    const net = (navigator as Navigator & { connection?: { effectiveType?: string; rtt?: number; downlink?: number; saveData?: boolean; type?: string } }).connection;
    const r: Record<string, unknown> = {
      v: 1,
      dev: this.dev,
      page: this.page,
      seq: this.seq++,
      t: Date.now(),
      up: round1(now / 1000),
      ctx: this.ctx(),
      vis: { visible_s: round1(this.visibleMs / 1000), hidden_s: round1(this.hiddenMs / 1000), hidden: document.hidden },
      timings: Object.fromEntries([...this.timings].map(([k, h]) => [k, h.json()])),
      counts: Object.fromEntries(this.counts),
      res: Object.fromEntries([...this.res].map(([k, x]) => [k, { n: x.n, bytes: x.bytes, cached: x.cached, sw: x.sw, dur: x.dur.json(), srv: x.srv.json() }])),
      sse: Object.fromEntries(this.sse),
      long: { n: this.long.n, ms: Math.round(this.long.ms), max: Math.round(this.long.max) },
      loaf: [...this.loaf].sort((a, b) => b[1].ms - a[1].ms).slice(0, 25).map(([key, a]) => ({ key, n: a.n, ms: Math.round(a.ms) })),
      events: this.events,
      frames: Object.fromEntries([...this.frames].map(([k, f]) => [k, { n: f.n, dropped: f.dropped, worst: Math.round(f.worst), budget: round1(this.budget) }])),
      mem: this.mem(),
      bat: this.battery ? { level: this.battery.level, charging: this.battery.charging } : undefined,
      net: net ? { type: net.type, eff: net.effectiveType, rtt: net.rtt, down: net.downlink, save: net.saveData } : undefined,
      errors: [...this.errors.values()],
    };
    if (!this.envSent) {
      r.env = this.env();
      r.nav = this.nav();
      this.envSent = true;
    }
    this.timings.clear();
    this.counts.clear();
    this.res.clear();
    this.sse.clear();
    this.long = { n: 0, ms: 0, max: 0 };
    this.loaf.clear();
    this.events = [];
    this.frames.clear();
    this.errors.clear();
    this.visibleMs = 0;
    this.hiddenMs = 0;
    this.totals.reports++;
    if (this.totals.reports % 5 === 1) {
      navigator.storage
        ?.estimate?.()
        .then((e) => (this.storageMb = round1((e.usage ?? 0) / MB)))
        .catch(() => {});
    }
    const body = JSON.stringify(r);
    const url = BASE + 'debug/client';
    if (beacon && body.length < 60_000 && navigator.sendBeacon?.(url, body)) return;
    fetch(url, { method: 'POST', body, credentials: 'same-origin', keepalive: body.length < 60_000, headers: { 'content-type': 'application/json' } }).catch(() => {});
  }
}

export const tel = new Telemetry();
