<script lang="ts">
  import { onDestroy } from 'svelte';
  import { api, message, type HojaDocument, type TemplateChange, type TemplateStatus, type TypeChange, type TypeStatus } from './lib/api';
  import { collapse, lineDiff, titleOf, when } from './lib/views';

  let { types, typesError, templates, templatesError, onchange }: {
    types: TypeStatus[];
    typesError: string | null;
    templates: TemplateStatus[];
    templatesError: string | null;
    onchange: () => void;
  } = $props();

  let open = $state<TypeChange | null>(null);
  let openTemplate = $state<TemplateChange | null>(null);
  let error = $state('');
  let busy = $state(false);

  // Template preview: which type renders it, and from which data.
  let previewType = $state('');
  let previewSource = $state('');
  let previewDocs = $state<HojaDocument[]>([]);
  let previewUrl = $state('');
  let previewError = $state('');
  let previewing = $state(false);

  const stateText: Record<TypeStatus['state'], string> = {
    accepted: 'accepted',
    new: 'new file, not accepted',
    changed: 'file changed since you accepted it',
    missing: 'file removed; the accepted copy still applies'
  };

  // Types a template preview can render with: those using it, else any type
  // (a new template usually arrives with a new type file not accepted yet).
  const previewTypes = $derived(openTemplate
    ? (openTemplate.used_by.length ? openTemplate.used_by : types.map(t => t.name))
    : []);

  async function review(name: string) {
    try {
      openTemplate = null;
      open = await api.typeChange(name);
      error = '';
    } catch (e) { error = message(e, 'Could not read the type file.'); }
  }

  async function accept() {
    if (!open?.file_hash) return;
    busy = true;
    try {
      await api.acceptType(open.name, open.file_hash);
      open = null;
      onchange();
    } catch (e) { error = message(e, 'Could not accept the type.'); }
    finally { busy = false; }
  }

  async function reviewTemplate(path: string) {
    try {
      open = null;
      clearPreview();
      openTemplate = await api.templateChange(path);
      previewType = previewTypes[0] ?? '';
      previewSource = '';
      error = '';
      await loadPreviewDocs();
      if (previewType && openTemplate.file_hash && !openTemplate.error) await renderPreview();
    } catch (e) { error = message(e, 'Could not read the template.'); }
  }

  async function loadPreviewDocs() {
    previewDocs = previewType ? await api.documents(previewType).catch(() => []) : [];
  }

  function clearPreview() {
    if (previewUrl) URL.revokeObjectURL(previewUrl);
    previewUrl = '';
    previewError = '';
  }

  async function renderPreview() {
    if (!openTemplate?.file_hash || !previewType) return;
    previewing = true;
    try {
      const blob = await api.templatePreview(openTemplate.path, openTemplate.file_hash, previewType, previewSource || undefined);
      clearPreview();
      previewUrl = URL.createObjectURL(blob);
    } catch (e) {
      clearPreview();
      previewError = message(e, 'Could not render the preview.');
    } finally { previewing = false; }
  }

  async function acceptTemplate() {
    if (!openTemplate?.file_hash) return;
    busy = true;
    try {
      await api.acceptTemplate(openTemplate.path, openTemplate.file_hash);
      openTemplate = null;
      clearPreview();
      onchange();
    } catch (e) { error = message(e, 'Could not accept the template.'); }
    finally { busy = false; }
  }

  function size(bytes: number | null): string {
    if (bytes === null) return 'none';
    return bytes < 1024 ? `${bytes} bytes` : `${(bytes / 1024).toFixed(1)} KiB`;
  }

  onDestroy(clearPreview);
</script>

<header class="page-head">
  <div>
    <p class="eyebrow">types/*.toml and templates/</p>
    <h1>Types</h1>
  </div>
</header>

<p class="hint">Type files and templates take effect only after you accept their exact contents here. Until then hoja keeps using the copies you accepted last. Accepting a template change returns every approved document it renders to draft.</p>
{#if typesError}<p class="error" role="alert">{typesError}</p>{/if}
{#if error}<p class="error" role="alert">{error}</p>{/if}

<ul class="type-list">
  {#each types as t (t.name)}
    <li class="card">
      <div class="form-head">
        <h2>{titleOf(t.name, t.config ?? undefined)} <code>{t.name}</code></h2>
        <span class="badge {t.state === 'accepted' ? 'ok' : t.state === 'missing' ? 'bad' : 'warn'}">{stateText[t.state]}</span>
      </div>
      {#if t.config}
        <p class="muted">
          First draft: {t.config.first_draft === 'human' ? 'yours' : 'an agent’s'} ·
          kinds: {Object.keys(t.config.kinds).join(', ')} ·
          exports: {t.config.exports.join(', ')}
          {#if t.config.template} · template: {t.config.template}{/if}
          {#if t.accepted_at} · accepted {when(t.accepted_at)}{/if}
        </p>
      {/if}
      {#if t.error}<pre class="error">{t.error}</pre>{/if}
      {#if t.state === 'new' || t.state === 'changed'}
        <button onclick={() => review(t.name)}>Review changes</button>
      {/if}
    </li>
  {:else}
    <li class="empty">No type files in this workspace's types/ directory.</li>
  {/each}
</ul>

{#if open}
  <section class="card diff">
    <div class="form-head">
      <h2>{open.name}.toml</h2>
      <div class="actions">
        <button class="primary" onclick={accept} disabled={busy || !!open.error || !open.file_hash}>Accept this file</button>
        <button onclick={() => open = null}>Close</button>
      </div>
    </div>
    {#if open.error}<pre class="error">{open.error}</pre>{/if}
    <pre class="lines">{#each collapse(lineDiff(open.accepted_body ?? '', open.file_body ?? '')) as line, i (i)}<span class={line.kind}>{line.kind === 'add' ? '+ ' : line.kind === 'remove' ? '- ' : line.kind === 'skip' ? '⋯ ' : '  '}{line.text}
</span>{/each}</pre>
  </section>
{/if}

<h2 class="subhead">Templates</h2>
{#if templatesError}<p class="error" role="alert">{templatesError}</p>{/if}
<ul class="type-list">
  {#each templates as t (t.path)}
    <li class="card">
      <div class="form-head">
        <h2><code>templates/{t.path}</code></h2>
        <span class="badge {t.state === 'accepted' ? 'ok' : t.state === 'missing' ? 'bad' : 'warn'}">{stateText[t.state]}</span>
      </div>
      <p class="muted">
        {#if t.used_by.length}Renders {t.used_by.map(n => titleOf(n, types.find(x => x.name === n)?.config ?? undefined)).join(', ')}{:else}Not used by an accepted type{/if}
        {#if t.accepted_at} · accepted {when(t.accepted_at)}{/if}
      </p>
      {#if t.error}<pre class="error">{t.error}</pre>{/if}
      {#if t.state === 'new' || t.state === 'changed'}
        <button onclick={() => reviewTemplate(t.path)}>Review changes</button>
      {/if}
    </li>
  {:else}
    <li class="empty">No files in this workspace's templates/ directory.</li>
  {/each}
</ul>

{#if openTemplate}
  <section class="card diff">
    <div class="form-head">
      <h2>templates/{openTemplate.path}</h2>
      <div class="actions">
        <button class="primary" onclick={acceptTemplate} disabled={busy || !!openTemplate.error || !openTemplate.file_hash}>Accept this file</button>
        <button onclick={() => { openTemplate = null; clearPreview(); }}>Close</button>
      </div>
    </div>
    {#if openTemplate.error}<pre class="error">{openTemplate.error}</pre>{/if}
    <div class="template-review">
      <div>
        {#if openTemplate.binary}
          <p class="muted">Binary file: {size(openTemplate.accepted_size)} accepted, {size(openTemplate.file_size)} on disk.</p>
        {:else}
          <pre class="lines">{#each collapse(lineDiff(openTemplate.accepted_text ?? '', openTemplate.file_text ?? '')) as line, i (i)}<span class={line.kind}>{line.kind === 'add' ? '+ ' : line.kind === 'remove' ? '- ' : line.kind === 'skip' ? '⋯ ' : '  '}{line.text}
</span>{/each}</pre>
        {/if}
      </div>
      <div class="preview">
        <div class="form-head">
          <h3>Preview with this file</h3>
          <div class="actions">
            <select aria-label="Type" bind:value={previewType} onchange={async () => { previewSource = ''; await loadPreviewDocs(); renderPreview(); }}>
              {#each previewTypes as name (name)}<option value={name}>{titleOf(name, types.find(x => x.name === name)?.config ?? undefined)}</option>{/each}
            </select>
            <select aria-label="Data" bind:value={previewSource} onchange={renderPreview}>
              <option value="">Sample data</option>
              {#each previewDocs as d (d.id)}<option value={d.id}>{d.title}</option>{/each}
            </select>
            <button onclick={renderPreview} disabled={previewing || !previewType}>{previewing ? 'Rendering…' : 'Render'}</button>
          </div>
        </div>
        <p class="hint">Rendered with every other template as accepted, and with the type file as it is on disk.</p>
        {#if previewError}<pre class="error">{previewError}</pre>{/if}
        {#if previewUrl}
          <iframe title="Template preview" src={previewUrl}></iframe>
          <a href={previewUrl} download="template-preview.pdf">Download this PDF</a>
        {/if}
      </div>
    </div>
  </section>
{/if}
