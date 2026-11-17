<script lang="ts">
  let {
    label,
    value,
    placeholder = '',
    multiline = false,
    readonly = false,
    onsave,
    inputRef = $bindable(),
  }: {
    label: string;
    value: string | null;
    placeholder?: string;
    multiline?: boolean;
    readonly?: boolean;
    onsave: (v: string | null) => void;
    inputRef?: HTMLInputElement | HTMLTextAreaElement;
  } = $props();

  let draft = $state('');
  let focused = $state(false);

  $effect(() => {
    const v = value ?? '';
    if (!focused) draft = v;
  });

  function commit() {
    const next = draft.trim();
    if (next !== (value ?? '').trim()) onsave(next === '' ? null : next);
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      draft = value ?? '';
      (e.target as HTMLElement).blur();
      e.stopPropagation();
    } else if (e.key === 'Enter' && (!multiline || e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      (e.target as HTMLElement).blur();
    }
  }
</script>

<label class="ef">
  <span class="lab">{label}</span>
  {#if multiline}
    <textarea
      class="field"
      bind:this={inputRef}
      bind:value={draft}
      {placeholder}
      {readonly}
      rows="2"
      onfocus={() => (focused = true)}
      onblur={() => { focused = false; commit(); }}
      onkeydown={onKey}
    ></textarea>
  {:else}
    <input
      class="field"
      bind:this={inputRef}
      bind:value={draft}
      {placeholder}
      {readonly}
      onfocus={() => (focused = true)}
      onblur={() => { focused = false; commit(); }}
      onkeydown={onKey}
    />
  {/if}
</label>

<style>
  .ef {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .lab {
    font-size: var(--fs-sm);
    color: var(--text-3);
    font-weight: 600;
  }
  .field {
    width: 100%;
    height: 2rem;
    border-color: transparent;
    background: var(--surface-2);
    transition: border-color 0.12s, background 0.12s;
  }
  textarea.field {
    height: auto;
    min-height: 3.25rem;
  }
  .field:hover {
    border-color: var(--border-strong);
  }
  .field:focus {
    background: var(--field);
  }
</style>
