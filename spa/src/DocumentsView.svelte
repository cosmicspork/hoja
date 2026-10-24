<script lang="ts">
  import FieldInput from './FieldInput.svelte';
  import { api, message, type HojaDocument, type TypeStatus } from './lib/api';
  import { authorText, formOf, href, patch, titleOf, when } from './lib/views';

  let { types, onchange }: { types: TypeStatus[]; onchange: () => void } = $props();

  const usable = $derived(types.filter(t => t.config));
  let documents = $state<HojaDocument[]>([]);
  let error = $state('');
  let creating = $state(false);
  let newType = $state('');
  let newTitle = $state('');
  let form = $state<Record<string, string | boolean>>({});
  let busy = $state(false);

  const newConfig = $derived(usable.find(t => t.name === newType)?.config ?? null);

  async function load() {
    try {
      documents = await api.documents();
      error = '';
    } catch (e) { error = message(e, 'Could not load documents.'); }
  }

  function startNew() {
    creating = true;
    newType = usable[0]?.name ?? '';
    newTitle = '';
  }

  $effect(() => { form = newConfig ? formOf(newConfig.document.fields, {}) : {}; });

  async function create(event: SubmitEvent) {
    event.preventDefault();
    if (!newConfig) return;
    busy = true;
    error = '';
    try {
      const doc = await api.createDocument(newType, newTitle, patch(newConfig.document.fields, {}, form));
      onchange();
      location.hash = href({ view: 'document', id: doc.id });
    } catch (e) { error = message(e, 'Could not create the document.'); }
    finally { busy = false; }
  }

  $effect(() => { load(); });
</script>

<header class="page-head">
  <div>
    <p class="eyebrow">Workspace</p>
    <h1>Documents</h1>
  </div>
  <div class="actions">
    <button class="primary" onclick={startNew} disabled={!usable.length}>New document</button>
  </div>
</header>

{#if error}<p class="error" role="alert">{error}</p>{/if}
{#if !usable.length}
  <p class="empty">No document types are accepted yet. Review them under <a href={href({ view: 'types' })}>Types</a>.</p>
{/if}

{#if creating && newConfig}
  <form class="card form" onsubmit={create}>
    <h2>New document</h2>
    <div class="fields">
      <div class="field">
        <label for="new-type">Type</label>
        <select id="new-type" bind:value={newType}>
          {#each usable as t (t.name)}<option value={t.name}>{titleOf(t.name, t.config ?? undefined)}</option>{/each}
        </select>
      </div>
      <div class="field">
        <label for="new-title">Title <span class="required">*</span></label>
        <input id="new-title" bind:value={newTitle} />
      </div>
      {#each Object.entries(newConfig.document.fields) as [name, field] (name)}
        <FieldInput {name} {field} config={newConfig} bind:value={form[name]} disabled={busy} />
      {/each}
    </div>
    <div class="form-actions">
      <button class="primary" type="submit" disabled={busy || !newTitle.trim()}>Create</button>
      <button type="button" onclick={() => creating = false}>Cancel</button>
    </div>
  </form>
{/if}

<ul class="doc-list">
  {#each documents as doc (doc.id)}
    <li>
      <a class="card" href={href({ view: 'document', id: doc.id })}>
        <span class="label">{doc.title}</span>
        <span class="meta">
          <span class="muted">{titleOf(doc.type, types.find(t => t.name === doc.type)?.config ?? undefined)}</span>
          {#if doc.approved}<span class="badge ok">approved</span>{:else if doc.approved_at}<span class="badge warn">changed since approval</span>{:else}<span class="badge">draft</span>{/if}
          <span class="badge author {doc.created_by?.author}">{authorText(doc.created_by)}</span>
          <span class="muted">updated {when(doc.updated_at)}</span>
        </span>
      </a>
    </li>
  {:else}
    <li class="empty">No documents yet.</li>
  {/each}
</ul>
