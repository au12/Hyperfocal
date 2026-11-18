<script lang="ts">
  import type { RawInfo } from '../lib/api';

  let { raw = null, rgb = null, approximate = false }: { raw?: RawInfo | null; rgb?: Uint32Array[] | null; approximate?: boolean } = $props();
  let canvas: HTMLCanvasElement | undefined = $state();

  const W = 300;
  const H = 120;

  $effect(() => {
    const c = canvas;
    if (!c) return;
    const dpr = window.devicePixelRatio || 1;
    c.width = W * dpr;
    c.height = H * dpr;
    const g = c.getContext('2d')!;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, W, H);
    const series = raw ? raw.hist : rgb ? rgb.map((a) => Array.from(a)) : null;
    if (!series) return;
    // Log scale on counts, like FastRawViewer's RAW histogram.
    let max = 1;
    for (const s of series) for (let i = 1; i < s.length - 1; i++) max = Math.max(max, s[i]);
    const lmax = Math.log1p(max);
    const colors = ['rgba(255,70,70,0.9)', 'rgba(70,220,90,0.9)', 'rgba(80,140,255,0.95)'];
    g.globalCompositeOperation = 'lighter';
    series.forEach((s, ch) => {
      const n = s.length;
      g.beginPath();
      g.moveTo(0, H);
      for (let i = 0; i < n; i++) {
        const x = (i / (n - 1)) * W;
        const y = H - (Math.log1p(s[i]) / lmax) * (H - 6);
        g.lineTo(x, y);
      }
      g.lineTo(W, H);
      g.closePath();
      g.fillStyle = colors[ch].replace(/[\d.]+\)$/, '0.28)');
      g.fill();
      g.strokeStyle = colors[ch];
      g.lineWidth = 1.25;
      g.stroke();
    });
    g.globalCompositeOperation = 'source-over';
    // EV grid for RAW histograms.
    if (raw) {
      g.strokeStyle = 'rgba(255,255,255,0.13)';
      g.fillStyle = 'rgba(255,255,255,0.45)';
      g.font = '9px sans-serif';
      const bins = raw.hist[0].length;
      for (let ev = 0; ev >= raw.hist_min_ev; ev -= 2) {
        const bin = (ev - raw.hist_min_ev) * raw.hist_per_ev;
        const x = (bin / (bins - 1)) * W;
        g.beginPath();
        g.moveTo(x, 0);
        g.lineTo(x, H);
        g.stroke();
        g.fillText(`${ev}`, Math.min(x + 2, W - 12), 10);
      }
    } else {
      g.strokeStyle = 'rgba(255,255,255,0.13)';
      for (let k = 1; k < 4; k++) {
        g.beginPath();
        g.moveTo((k * W) / 4, 0);
        g.lineTo((k * W) / 4, H);
        g.stroke();
      }
    }
  });

  // Three significant digits, like FastRawViewer's exposure stats (0.105%, 1.36%, 4.10%).
  const pct = (v: number) => {
    const p = v * 100;
    if (!(p > 0)) return '0';
    if (p < 0.001) return '<0.001%';
    return `${p >= 99.95 ? '100' : p.toPrecision(3)}%`;
  };
  // Older servers sent one underexposure fraction.
  const under = $derived(raw ? (Array.isArray(raw.under) ? raw.under : [raw.under, raw.under, raw.under, raw.under]) : null);
</script>

<div class="hist">
  <div class="head">
    <span>{raw ? 'RAW histogram' : 'Histogram'}{#if approximate} (approx.){/if}{#if raw?.decoder === 'LibRaw'}<span class="dim" title="Decoded with LibRaw (FlashRaw could not read this file)"> · LibRaw</span>{/if}</span>
    {#if raw}<span class="dim">EV below clipping</span>{/if}
  </div>
  <canvas bind:this={canvas} style:width={`${W}px`} style:height={`${H}px`}></canvas>
  {#if raw}
    <div class="stats tnum">
      <span title="Overexposed samples per channel (within 0.05 EV of saturation)">OE <b class="r">{pct(raw.over[0])}</b> <b class="g">{pct(raw.over[1])}</b> <b class="b">{pct(raw.over[2])}</b></span>
      {#if under}<span title="Underexposed samples per channel (more than 7.7 EV below saturation)">UE <b class="r">{pct(under[0])}</b> <b class="g">{pct(under[1])}</b> <b class="b">{pct(under[2])}</b></span>{/if}
    </div>
  {/if}
</div>

<style>
  .hist {
    background: rgba(14, 18, 22, 0.82);
    backdrop-filter: blur(8px);
    border-radius: var(--r-lg);
    padding: 0.5rem 0.625rem 0.5rem;
    color: #e8edf2;
    font-size: 0.6875rem;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.4);
    border: 1px solid rgba(255, 255, 255, 0.08);
  }
  .head {
    display: flex;
    justify-content: space-between;
    margin-bottom: 0.25rem;
    font-weight: 600;
  }
  .dim {
    color: rgba(232, 237, 242, 0.6);
    font-weight: 400;
  }
  canvas {
    display: block;
  }
  .stats {
    display: flex;
    justify-content: space-between;
    margin-top: 0.25rem;
  }
  .r {
    color: #ff7676;
  }
  .g {
    color: #5ee07a;
  }
  .b {
    color: #7aa8ff;
  }
</style>
