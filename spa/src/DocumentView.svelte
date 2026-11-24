<script lang="ts">
  import { Banner, Empty, Row, SideButton } from '@tracon/ui/svelte';
  import { onDestroy } from 'svelte';
  import FieldInput from './FieldInput.svelte';
  import Provenance from './Provenance.svelte';
  import Revisions from './Revisions.svelte';
  import { ApiError, api, message, type DocumentView, type Format, type HojaRecord, type SectionConfig, type TypeConfig } from './lib/api';
  import { eligible, formOf, href, included, move, patch, recordLabel, titleOf, when } from './lib/views';

  let { id, config, configs, onchange }: { id: string; config: TypeConfig; configs: Record<string, TypeConfig>; onchange: () => void } = $props();

  // The type a section's records come from, and its config.
  function sourceType(section: SectionConfig): string { return section.type ?? doc?.type ?? ''; }
  function sourceConfig(section: SectionConfig): TypeConfig | undefined { return configs[sourceType(section)]; }
  function labelOf(record: HojaRecord): string { return recordLabel(configs[record.type] ?? config, record); }

  let view = $state<DocumentView | null>(null);
  let records = $state<HojaRecord[]>([]);
  let title = $state('');
  let form = $state<Record<string, string | boolean>>({});
  let selection = $state<Record<string, string[]>>({});
  let error = $state('');
  let notice = $state('');
  let busy = $state(false);
  let previewUrl = $state('');
  let previewError = $state('');
  let previewing = $state(false);
  let tab = $state<'provenance' | 'pdf'>('provenance');

  const doc = $derived(view?.document ?? null);
  const byId = $derived(new Map(records.map(r => [r.id, r])));
  const fieldsDirty = $derived(doc ? title !== doc.title || Object.keys(patch(config.document.fields, doc.fields, form)).length > 0 : false);

  function saved(section: string): string[] {
    return doc?.sections.find(s => s.name === section)?.record_ids ?? [];
  }
  function sectionDirty(section: string): boolean {
    return JSON.stringify(selection[section] ?? []) !== JSON.stringify(saved(section));
  }

  async function load(refreshPreview = true) {
    try {
      const [next, all] = await Promise.all([api.document(id), api.records(undefined)]);
      view = next;
      const types = new Set([next.document.type, ...config.document.sections.map(s => s.type).filter(Boolean)]);
      records = all.filter(r => types.has(r.type));
      title = next.document.title;
      form = formOf(config.document.fields, next.document.fields);
      selection = Object.fromEntries(config.document.sections.map(s => [s.name, [...(next.document.sections.find(x => x.name === s.name)?.record_ids ?? [])]]));
      error = '';
      if (refreshPreview && tab === 'pdf' && next.has_template) preview();
    } catch (e) { error = message(e, 'Could not load the document.'); }
  }

  // Runs a change, then reloads. A string result is shown as a notice. A
  // refused approval also reloads, so the person sees what changed.
  async function act(operation: () => Promise<unknown>, reloadOnError = false) {
    busy = true;
    error = '';
    notice = '';
    try {
      const result = await operation();
      notice = typeof result === 'string' ? result : '';
      await load();
      onchange();
    } catch (e) {
      error = message(e, 'That did not work.');
      if (reloadOnError) {
        if (e instanceof ApiError && e.code === 'conflict') error = 'This changed since the page loaded, so it was not approved. Review it again below.';
        await load();
      }
    }
    finally { busy = false; }
  }

  async function preview() {
    previewing = true;
    try {
      const blob = await api.preview(id);
      if (previewUrl) URL.revokeObjectURL(previewUrl);
      previewUrl = URL.createObjectURL(blob);
      previewError = '';
    } catch (e) { previewError = message(e, 'Could not render the preview.'); }
    finally { previewing = false; }
  }

  function saveFields(event: SubmitEvent) {
    event.preventDefault();
    if (!doc) return;
    const d = doc;
    act(() => api.updateDocument(d.id, d.version, title, patch(config.document.fields, d.fields, form)));
  }

  function saveSection(section: string) {
    if (!doc) return;
    const d = doc;
    act(() => api.select(d.id, section, d.version, selection[section] ?? []));
  }

  // Approves exactly what this page shows: the server compares the content
  // hash it was loaded with and refuses if anything changed since.
  function approve() {
    if (!doc || !view) return;
    const d = doc;
    const hash = view.content_hash;
    act(async () => {
      await api.approve(d.id, d.version, hash);
      return 'Approved. Any later change returns it to draft.';
    }, true);
  }

  function exportAs(format: Format) {
    act(async () => `Wrote ${(await api.exportDocument(id, format)).path}`);
  }

  function add(section: string, recordId: string) {
    selection[section] = [...(selection[section] ?? []), recordId];
  }
  function remove(section: string, recordId: string) {
    selection[section] = (selection[section] ?? []).filter(r => r !== recordId);
  }
  function shift(section: string, index: number, delta: number) {
    selection[section] = move(selection[section] ?? [], index, delta);
  }

  $effect(() => { void id; load(); });
  onDestroy(() => { if (previewUrl) URL.revokeObjectURL(previewUrl); });
</script>

{#if doc && view}
  <header class="page-head">
    <div>
      <p class="eyebrow"><a href={href({ view: 'documents' })}>Documents</a> · {titleOf(doc.type, config)}</p>
      <h1>{doc.title}</h1>
      <div class="status-line">
        {#if doc.approved}<span class="badge ok">approved {when(doc.approved_at)}</span>
        {:else if doc.approved_at}<span class="badge warn">changed since you approved it {when(doc.approved_at)}</span>
        {:else}<span class="badge">draft</span>{/if}
        <span class="muted">v{doc.version}</span>
      </div>
    </div>
    <div class="actions">
      <button class="btn p" onclick={approve} disabled={busy || doc.approved || view.blocking > 0 || fieldsDirty}
        title={view.blocking > 0 ? 'Fix the lint errors first' : fieldsDirty ? 'Save first' : ''}>Approve</button>
      {#each view.exports as file (file.format)}
        <button class="btn" onclick={() => exportAs(file.format)} disabled={busy} title={file.filename}>Export {file.format.toUpperCase()}</button>
      {/each}
    </div>
  </header>

  {#if error}<Banner tone="crit" role="alert">{error}</Banner>{/if}
  {#if notice}<Banner tone="ok" role="status">{notice}</Banner>{/if}

  <div class="doc-layout">
    <div class="doc-editor">
      {#if view.lints.length}
        <section class="card lints">
          <h3>Checks</h3>
          <ul>
            {#each view.lints as finding, i (i)}
              <li class={finding.level}><span class="badge {finding.level === 'error' ? 'bad' : ''}">{finding.level}</span> {finding.message}</li>
            {/each}
          </ul>
        </section>
      {/if}

      <form class="card form" onsubmit={saveFields}>
        <h2>Document</h2>
        <div class="fields">
          <div class="field">
            <label for="doc-title">Title <span class="required">*</span></label>
            <input id="doc-title" bind:value={title} disabled={busy} />
          </div>
          {#each Object.entries(config.document.fields) as [name, field] (name)}
            <FieldInput {name} {field} {config} {records} bind:value={form[name]} disabled={busy} />
          {/each}
        </div>
        <div class="form-actions">
          <button class="btn" type="submit" disabled={busy || !fieldsDirty}>Save</button>
        </div>
      </form>

      {#each config.document.sections as section (section.name)}
        {@const chosen = selection[section.name] ?? []}
        {@const available = eligible(records, section, doc.type).filter(r => !chosen.includes(r.id))}
        {@const source = sourceConfig(section)}
        <section class="card section">
          <div class="form-head">
            <h2>{titleOf(section.name, section)}</h2>
            <span class="muted">{titleOf(section.kind, source?.kinds[section.kind])} records{#if section.type} from the {titleOf(section.type, source)}{/if}{#if section.where}, where {Object.entries(section.where).map(([f, v]) => `${f} is ${v}`).join(' and ')}{/if}</span>
          </div>
          {#if section.include === 'all'}
            {@const every = included(records, section, doc.type)}
            <p class="muted">Holds every entry{#if section.sort}, ordered by {titleOf(section.sort, source?.kinds[section.kind]?.fields[section.sort])}{/if}. Add, edit or close entries on the records page.</p>
            <ol class="chosen">
              {#each every as record (record.id)}
                <li>
                  <Row>
                    <span class="label">{labelOf(record)}</span>
                    {#if !record.reviewed}<span class="badge">not reviewed</span>{/if}
                  </Row>
                </li>
              {:else}
                <li class="empty">No entries yet.</li>
              {/each}
            </ol>
            <div class="form-actions">
              <a class="lnk" href={href({ view: 'records', type: sourceType(section), kind: section.kind })}>Edit {titleOf(section.kind, source?.kinds[section.kind]).toLowerCase()} records</a>
            </div>
          {:else}
          <ol class="chosen">
            {#each chosen as recordId, index (recordId)}
              {@const record = byId.get(recordId)}
              <li>
                <Row>
                  <span class="label">{record ? labelOf(record) : 'Deleted record'}</span>
                  {#if record && !record.reviewed}<span class="badge">not reviewed</span>{/if}
                  {#snippet actions()}
                    <SideButton label="Move up" onclick={() => shift(section.name, index, -1)} disabled={index === 0}>↑</SideButton>
                    <SideButton label="Move down" onclick={() => shift(section.name, index, 1)} disabled={index === chosen.length - 1}>↓</SideButton>
                    <SideButton label="Remove" onclick={() => remove(section.name, recordId)}>✕</SideButton>
                  {/snippet}
                </Row>
              </li>
            {:else}
              <li class="empty">Nothing selected.</li>
            {/each}
          </ol>
          {#if available.length}
            <details>
              <summary>Add ({available.length} available)</summary>
              <ul class="available">
                {#each available as record (record.id)}
                  <li><button type="button" class="lnk" onclick={() => add(section.name, record.id)}>+ {labelOf(record)}</button></li>
                {/each}
              </ul>
            </details>
          {/if}
          <div class="form-actions">
            <button class="btn" onclick={() => saveSection(section.name)} disabled={busy || !sectionDirty(section.name)}>Save selection</button>
            {#if sectionDirty(section.name)}<button class="btn" type="button" onclick={() => selection[section.name] = [...saved(section.name)]}>Undo</button>{/if}
            <a class="lnk" href={href({ view: 'records', type: sourceType(section), kind: section.kind })}>Edit {titleOf(section.kind, source?.kinds[section.kind]).toLowerCase()} records</a>
          </div>
          {/if}
        </section>
      {/each}

      <Revisions revisions={view.revisions} />
    </div>

    <aside class="preview card">
      <div class="form-head">
        <div class="segmented" role="tablist">
          <button role="tab" aria-selected={tab === 'provenance'} class:on={tab === 'provenance'} onclick={() => tab = 'provenance'}>Provenance</button>
          {#if view.has_template}
            <button role="tab" aria-selected={tab === 'pdf'} class:on={tab === 'pdf'} onclick={() => { tab = 'pdf'; if (!previewUrl) preview(); }}>PDF</button>
          {/if}
        </div>
        {#if tab === 'pdf'}<button class="btn" onclick={preview} disabled={previewing}>{previewing ? 'Rendering…' : 'Refresh'}</button>{/if}
      </div>
      {#if tab === 'provenance'}
        <Provenance provenance={view.provenance} approvedAt={doc.approved_at} />
      {:else}
        <p class="hint">A layout check. Read the words in Provenance.</p>
        {#if previewError}<pre class="error">{previewError}</pre>{/if}
        {#if previewUrl}
          <iframe title="PDF preview" src={previewUrl}></iframe>
          <!-- Inside Tracon the page is framed and may not show PDFs; the file still downloads. -->
          <a class="lnk" href={previewUrl} download={view.exports.find(f => f.format === 'pdf')?.filename ?? 'preview.pdf'}>Download this PDF</a>
        {/if}
      {/if}
    </aside>
  </div>
{:else if error}
  <Banner tone="crit" role="alert">{error}</Banner>
{:else}
  <Empty loading>Loading…</Empty>
{/if}
