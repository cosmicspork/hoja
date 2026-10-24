<script lang="ts">
  import FieldInput from './FieldInput.svelte';
  import { api, message, type Approval, type FieldConfig, type HojaRecord, type TypeStatus, type Value } from './lib/api';
  import { display, fieldChanges, fromForm, href, recordLabel, titleOf, toForm, when } from './lib/views';

  let { types, onchange }: { types: TypeStatus[]; onchange: () => void } = $props();

  let approvals = $state<Approval[]>([]);
  let records = $state<HojaRecord[]>([]);
  let showDecided = $state(false);
  let error = $state('');
  let busy = $state('');
  let editing = $state<Record<string, Record<string, string | boolean>>>({});
  let reasons = $state<Record<string, string>>({});

  const pending = $derived(approvals.filter(a => a.state === 'pending'));
  const decided = $derived(approvals.filter(a => a.state !== 'pending').reverse().slice(0, 30));
  const byId = $derived(new Map(records.map(r => [r.id, r])));

  const actionText: Record<Approval['action'], string> = {
    'record.create': 'wants to create',
    'record.update': 'suggests a change to',
    'document.create': 'wants to create a document',
    'document.update': 'suggests a change to a document',
    'document.select': 'suggests a selection for'
  };

  function config(approval: Approval) {
    return types.find(t => t.name === approval.type)?.config ?? null;
  }

  function fieldConfigs(approval: Approval): Record<string, FieldConfig> {
    const c = config(approval);
    if (!c) return {};
    if (approval.action.startsWith('document')) return c.document.fields;
    const kind = approval.payload.kind ?? approval.current?.kind ?? '';
    return c.kinds[kind]?.fields ?? {};
  }

  function startEdit(approval: Approval) {
    const fields = fieldConfigs(approval);
    const form: Record<string, string | boolean> = {};
    for (const [name, value] of Object.entries(approval.payload.fields ?? {})) {
      if (fields[name]) form[name] = toForm(fields[name], value ?? undefined);
    }
    if (approval.payload.title !== undefined) form.__title = approval.payload.title;
    editing[approval.id] = form;
  }

  function editedPayload(approval: Approval): Approval['payload'] | undefined {
    const form = editing[approval.id];
    if (!form) return undefined;
    const fields = fieldConfigs(approval);
    const out: Approval['payload'] = { ...approval.payload };
    if (approval.payload.fields) {
      out.fields = Object.fromEntries(Object.keys(approval.payload.fields).map(name => [name, fromForm(fields[name], form[name] ?? '')])) as Record<string, Value>;
    }
    if (typeof form.__title === 'string') out.title = form.__title;
    return out;
  }

  async function load() {
    try {
      [approvals, records] = await Promise.all([api.approvals(), api.records()]);
      error = '';
    } catch (e) { error = message(e, 'Could not load cards.'); }
  }

  async function decide(approval: Approval, decision: 'accept' | 'reject' | 'request_changes') {
    busy = approval.id;
    error = '';
    try {
      const reason = reasons[approval.id]?.trim() || null;
      await api.decide(approval.id, decision, reason, decision === 'accept' ? editedPayload(approval) : undefined);
      delete editing[approval.id];
      await load();
      onchange();
    } catch (e) { error = message(e, 'Could not decide the card.'); }
    finally { busy = ''; }
  }

  function targetLabel(approval: Approval): string {
    const c = config(approval);
    if (approval.action.startsWith('record') && approval.target_id) {
      const record = byId.get(approval.target_id);
      return record ? recordLabel(c, record) : 'a deleted record';
    }
    if (approval.action === 'record.create') return `a ${approval.payload.kind} record`;
    return approval.current?.title ?? approval.payload.title ?? '';
  }

  function selectionLabels(ids: string[] | undefined): string[] {
    const c = (id: string) => byId.get(id);
    return (ids ?? []).map(id => { const r = c(id); return r ? recordLabel(types.find(t => t.name === r.type)?.config, r) : id; });
  }

  $effect(() => { load(); });
</script>

<header class="page-head">
  <div>
    <p class="eyebrow">Cards from agents</p>
    <h1>Approvals</h1>
  </div>
  <div class="actions">
    <button onclick={load}>Reload</button>
    <label class="toggle"><input type="checkbox" bind:checked={showDecided} /> Show decided</label>
  </div>
</header>

{#if error}<p class="error" role="alert">{error}</p>{/if}

{#each pending as approval (approval.id)}
  {@const form = editing[approval.id]}
  {@const fields = fieldConfigs(approval)}
  {@const c = config(approval)}
  <article class="card approval" class:stale={approval.stale}>
    <div class="form-head">
      <h2><span class="badge author agent">{approval.agent}</span> {actionText[approval.action]} {targetLabel(approval)}</h2>
      <span class="muted">{titleOf(approval.type, c ?? undefined)} · {when(approval.created_at)}</span>
    </div>
    {#if approval.note}<p class="note">“{approval.note}”</p>{/if}
    {#if approval.stale}<p class="error">This changed since the agent read it, so it can't be accepted. Reject it or request changes; the agent resubmits against the current version.</p>{/if}

    {#if approval.action === 'document.select'}
      <div class="compare">
        <div><h4>Now: {approval.payload.section}</h4><ol>{#each selectionLabels(approval.current?.sections?.find(s => s.name === approval.payload.section)?.record_ids) as l, i (i)}<li>{l}</li>{:else}<li class="muted">empty</li>{/each}</ol></div>
        <div><h4>Proposed</h4><ol>{#each selectionLabels(approval.payload.record_ids) as l, i (i)}<li>{l}</li>{:else}<li class="muted">empty</li>{/each}</ol></div>
      </div>
    {:else if form && c}
      <div class="fields">
        {#if form.__title !== undefined}
          <div class="field"><label for="t-{approval.id}">Title</label><input id="t-{approval.id}" bind:value={editing[approval.id].__title} /></div>
        {/if}
        {#each Object.keys(approval.payload.fields ?? {}) as name (name)}
          {#if fields[name]}<FieldInput {name} field={fields[name]} config={c} {records} bind:value={editing[approval.id][name]} />{/if}
        {/each}
      </div>
    {:else}
      <table class="changes">
        <thead><tr><th>Field</th><th>Now</th><th>Proposed</th></tr></thead>
        <tbody>
          {#if approval.payload.title !== undefined}
            <tr><td>Title</td><td>{approval.current?.title ?? ''}</td><td class="add">{approval.payload.title}</td></tr>
          {/if}
          {#each fieldChanges(approval.current?.fields, approval.payload.fields) as change (change.name)}
            <tr><td>{titleOf(change.name, fields[change.name])}</td><td class="remove">{change.before}</td><td class="add">{change.after || '(cleared)'}</td></tr>
          {/each}
          {#each Object.entries(approval.payload.fields ?? {}).filter(([n, v]) => display(v ?? undefined) === display(approval.current?.fields?.[n])) as [name] (name)}
            <tr><td>{titleOf(name, fields[name])}</td><td colspan="2" class="muted">unchanged</td></tr>
          {/each}
        </tbody>
      </table>
    {/if}

    <div class="form-actions">
      <button class="primary" onclick={() => decide(approval, 'accept')} disabled={busy === approval.id || approval.stale}>{form ? 'Accept with edits' : 'Accept'}</button>
      {#if approval.action !== 'document.select'}
        {#if form}<button onclick={() => delete editing[approval.id]}>Discard edits</button>{:else}<button onclick={() => startEdit(approval)} disabled={approval.stale}>Edit</button>{/if}
      {/if}
      <input class="reason" placeholder="Reason or notes for the agent" bind:value={reasons[approval.id]} />
      <button onclick={() => decide(approval, 'request_changes')} disabled={busy === approval.id}>Request changes</button>
      <button class="danger" onclick={() => decide(approval, 'reject')} disabled={busy === approval.id}>Reject</button>
      {#if approval.target_id && approval.action.startsWith('document')}<a class="muted" href={href({ view: 'document', id: approval.target_id })}>Open document</a>{/if}
    </div>
  </article>
{:else}
  <p class="empty">No cards waiting. Agents' suggestions show up here.</p>
{/each}

{#if showDecided}
  <h2 class="subhead">Decided</h2>
  <ul class="decided">
    {#each decided as approval (approval.id)}
      <li>
        <span class="badge {approval.state === 'accepted' ? 'ok' : approval.state === 'rejected' ? 'bad' : 'warn'}">{approval.state.replace('_', ' ')}</span>
        <span class="badge author agent">{approval.agent}</span> {actionText[approval.action]} {targetLabel(approval)}
        {#if approval.edited}<span class="badge">edited</span>{/if}
        {#if approval.reason}<span class="muted">“{approval.reason}”</span>{/if}
        <span class="muted right">{when(approval.decided_at)}</span>
      </li>
    {:else}
      <li class="empty">Nothing decided yet.</li>
    {/each}
  </ul>
{/if}
