<script lang="ts">
  import { api } from '../lib/api';
  import Icon from './Icon.svelte';

  let {
    keywords,
    hierarchical = [],
    onadd,
    onremove,
    onpick,
    readonly = false,
    inputRef = $bindable(),
  }: {
    keywords: string[];
    hierarchical?: string[];
    onadd: (k: string) => void;
    onremove: (k: string) => void;
    onpick?: (k: string) => void;
    readonly?: boolean;
    inputRef?: HTMLInputElement;
  } = $props();

  let text = $state('');
  let sugg = $state<[string, number][]>([]);
  let active = $state(-1);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let ctl: AbortController | null = null;

  // Show hierarchical keywords as paths, and plain keywords not covered by one.
  const items = $derived.by(() => {
    const leaves = new Set(hierarchical.map((h) => h.split('|').pop()!.trim().toLowerCase()));
    const plain = keywords.filter((k) => !leaves.has(k.toLowerCase()));
    return [...hierarchical.map((h) => ({ key: h, parts: h.split('|') })), ...plain.map((k) => ({ key: k, parts: [k] }))];
  });

  function onInput() {
    clearTimeout(timer);
    active = -1;
    const q = text.trim();
    if (!q) {
      sugg = [];
      return;
    }
    timer = setTimeout(async () => {
      ctl?.abort();
      ctl = new AbortController();
      try {
        sugg = (await api.keywords(q, ctl.signal)).filter(([k]) => !keywords.some((x) => x.toLowerCase() === k.toLowerCase()) && !hierarchical.includes(k)).slice(0, 8);
      } catch {
        /* aborted */
      }
    }, 120);
  }

  function add(k: string) {
    const v = k.trim().replace(/\s*[>/]\s*/g, '|');
    if (!v) return;
    onadd(v);
    text = '';
    sugg = [];
    active = -1;
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      active = Math.min(sugg.length - 1, active + 1);
      e.preventDefault();
    } else if (e.key === 'ArrowUp') {
      active = Math.max(-1, active - 1);
      e.preventDefault();
    } else if (e.key === 'Enter' || e.key === 'Tab' || e.key === ',') {
      if (active >= 0 && sugg[active]) {
        add(sugg[active][0]);
        e.preventDefault();
      } else if (text.trim()) {
        add(text);
        e.preventDefault();
      }
    } else if (e.key === 'Escape') {
      if (text || sugg.length) {
        text = '';
        sugg = [];
        e.stopPropagation();
      } else (e.target as HTMLElement).blur();
    } else if (e.key === 'Backspace' && !text && items.length) {
      onremove(items[items.length - 1].key);
    }
  }
</script>

<div class="kw">
  <div class="chips">
    {#each items as it (it.key)}
      <span class="chip" title={it.key}>
        <button class="name" onclick={() => onpick?.(it.parts[it.parts.length - 1])} title="Show photos with this keyword">
          {#each it.parts as p, j}{#if j > 0}<span class="sep">›</span>{/if}{p}{/each}
        </button>
        {#if !readonly}<button onclick={() => onremove(it.key)} aria-label={`Remove ${it.key}`}><Icon name="x" size={12} /></button>{/if}
      </span>
    {/each}
    {#if !readonly}
      <div class="inwrap">
        <input
          class="kin"
          bind:this={inputRef}
          bind:value={text}
          oninput={onInput}
          onkeydown={onKey}
          onblur={() => setTimeout(() => (sugg = []), 150)}
          placeholder={items.length ? 'Add…' : 'Add keyword (Places|Portugal|Lisbon for a hierarchy)'}
        />
        {#if sugg.length}
          <div class="sugg pop">
            {#each sugg as [k, n], j}
              <button class="menu-item" class:active={j === active} onmousedown={(e) => { e.preventDefault(); add(k); }}>
                <Icon name="tag" /><span class="grow ellipsis">{k.replaceAll('|', ' › ')}</span><span class="pill-count">{n}</span>
              </button>
            {/each}
          </div>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 0.375rem;
    align-items: center;
  }
  .chip .name {
    background: none;
    border: 0;
    color: inherit;
    padding: 0;
    cursor: pointer;
    font: inherit;
    margin: 0;
    display: inline;
  }
  .chip .name:hover {
    text-decoration: underline;
  }
  .sep {
    color: var(--text-3);
    margin: 0 0.25rem;
  }
  .inwrap {
    position: relative;
    flex: 1;
    min-width: 8rem;
  }
  .kin {
    width: 100%;
    height: 1.75rem;
    border: 1px dashed var(--border-strong);
    border-radius: 1rem;
    background: transparent;
    padding: 0 0.75rem;
    outline: none;
    font-size: var(--fs-sm);
  }
  .kin:focus {
    border-style: solid;
    border-color: var(--accent);
  }
  .sugg {
    top: calc(100% + 0.25rem);
    left: 0;
    right: 0;
    max-height: 16rem;
    overflow: auto;
  }
</style>
