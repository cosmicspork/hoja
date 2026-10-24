<script lang="ts">
  import { api, message, type TypeChange, type TypeStatus } from './lib/api';
  import { collapse, lineDiff, titleOf, when } from './lib/views';

  let { types, typesError, onchange }: { types: TypeStatus[]; typesError: string | null; onchange: () => void } = $props();

  let open = $state<TypeChange | null>(null);
  let error = $state('');
  let busy = $state(false);

  const stateText: Record<TypeStatus['state'], string> = {
    accepted: 'accepted',
    new: 'new file, not accepted',
    changed: 'file changed since you accepted it',
    missing: 'file removed; the accepted copy still applies'
  };

  async function review(name: string) {
    try {
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
</script>

<header class="page-head">
  <div>
    <p class="eyebrow">types/*.toml</p>
    <h1>Types</h1>
  </div>
</header>

<p class="hint">A type file takes effect only after you accept its exact contents here. Until then hoja keeps using the copy you accepted last.</p>
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
