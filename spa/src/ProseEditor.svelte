<script lang="ts">
  import { onDestroy } from 'svelte';
  import { api, message } from './lib/api';

  // A Markdown text field: a textarea that grows with its text, and a live
  // preview rendered by hoja's own parser, the one that feeds the templates.
  let { id, value = $bindable(), disabled = false }: { id: string; value: string | boolean; disabled?: boolean } = $props();

  const text = $derived(typeof value === 'string' ? value : '');

  let previewing = $state(false);
  let html = $state('');
  let previewError = $state('');
  let timer: number | undefined;

  const rows = $derived(Math.min(30, Math.max(4, text.split('\n').length + 1, Math.ceil(text.length / 90) + 1)));

  function render(text: string) {
    window.clearTimeout(timer);
    timer = window.setTimeout(async () => {
      try {
        html = (await api.markdown(text)).html;
        previewError = '';
      } catch (e) { previewError = message(e, 'Could not render the preview.'); }
    }, 250);
  }

  $effect(() => { if (previewing) render(text); });
  onDestroy(() => window.clearTimeout(timer));
</script>

<div class="prose-editor" class:split={previewing}>
  <textarea {id} {rows} value={text} oninput={e => value = e.currentTarget.value} {disabled} spellcheck="true"></textarea>
  {#if previewing}
    <div class="prose-preview" aria-live="polite">
      {#if previewError}<p class="error">{previewError}</p>{:else if html}{@html html}{:else}<p class="muted">Nothing to preview.</p>{/if}
    </div>
  {/if}
</div>
<div class="prose-tools">
  <span class="muted">Markdown: **bold**, *italic*, - lists, ## headings</span>
  <button type="button" class="link" onclick={() => previewing = !previewing}>{previewing ? 'Hide preview' : 'Preview'}</button>
</div>
