<script lang="ts">
  import { Banner, Empty, Row } from '@tracon/ui/svelte';
  import FieldInput from './FieldInput.svelte';
  import Revisions from './Revisions.svelte';
  import { api, message, type HojaRecord, type Revision, type TypeConfig } from './lib/api';
  import { authorText, filterRecords, formOf, href, lastReviewed, patch, recordLabel, titleOf, valueDiff, when } from './lib/views';

  let { typeName, kind, config, selectedId, onchange }: {
    typeName: string;
    kind: string;
    config: TypeConfig;
    selectedId?: string;
    onchange: () => void;
  } = $props();

  const kindConfig = $derived(config.kinds[kind]);
  let all = $state<HojaRecord[]>([]);
  let query = $state('');
  let error = $state('');
  let busy = $state(false);
  let current = $state<HojaRecord | null>(null);
  let revisions = $state<Revision[]>([]);
  let form = $state<Record<string, string | boolean>>({});
  let creating = $state(false);

  const records = $derived(filterRecords(all.filter(r => r.kind === kind), query));
  // What changed since the person last marked this record reviewed: the
  // small diff to review, instead of rereading the whole record.
  const sinceReview = $derived.by(() => {
    if (!current || current.reviewed) return null;
    const base = lastReviewed(revisions);
    if (!base) return null;
    const before = base.snapshot.fields ?? {};
    const after = current.fields;
    const names = [...new Set([...Object.keys(kindConfig?.fields ?? {}), ...Object.keys(before), ...Object.keys(after)])];
    const changes = names
      .map(name => ({ name, parts: valueDiff(before[name], after[name]) }))
      .filter(c => c.parts.some(p => p.kind !== 'same'));
    return { version: base.version, changes };
  });
  const dirty = $derived(kindConfig && (creating || current) ? Object.keys(patch(kindConfig.fields, current?.fields ?? {}, form)).length > 0 : false);

  async function load() {
    try {
      all = await api.records(typeName);
      error = '';
    } catch (e) { error = message(e, 'Could not load records.'); }
  }

  async function open(id: string | undefined) {
    creating = false;
    if (!id) { current = null; revisions = []; return; }
    try {
      const view = await api.record(id);
      current = view.record;
      revisions = view.revisions;
      form = formOf(kindConfig.fields, current.fields);
      error = '';
    } catch (e) { error = message(e, 'Could not load the record.'); }
  }

  function startNew() {
    current = null;
    revisions = [];
    creating = true;
    form = formOf(kindConfig.fields, {});
    location.hash = href({ view: 'records', type: typeName, kind });
  }

  async function act(operation: () => Promise<void>) {
    busy = true;
    error = '';
    try {
      await operation();
      await load();
      onchange();
    } catch (e) {
      error = message(e, 'That did not work.');
    } finally { busy = false; }
  }

  function save(event: SubmitEvent) {
    event.preventDefault();
    act(async () => {
      if (creating) {
        const record = await api.createRecord(typeName, kind, patch(kindConfig.fields, {}, form));
        creating = false;
        location.hash = href({ view: 'records', type: typeName, kind, id: record.id });
      } else if (current) {
        await api.updateRecord(current.id, current.version, patch(kindConfig.fields, current.fields, form));
        await open(current.id);
      }
    });
  }

  function toggleReviewed() {
    if (!current) return;
    const record = current;
    act(async () => { await api.setReviewed(record.id, record.version, !record.reviewed); await open(record.id); });
  }

  function remove() {
    if (!current) return;
    const record = current;
    act(async () => {
      await api.deleteRecord(record.id, record.version);
      location.hash = href({ view: 'records', type: typeName, kind });
    });
  }

  $effect(() => { void typeName; load(); });
  $effect(() => { void kind; open(selectedId); });
</script>

<header class="page-head">
  <div>
    <p class="eyebrow">{titleOf(typeName, config)}</p>
    <h1>{titleOf(kind, kindConfig)}</h1>
  </div>
  <div class="actions">
    {#if kindConfig?.proposal_source}<span class="hint">Agents propose new {titleOf(kind, kindConfig).toLowerCase()} records as cards, citing their source in {titleOf(kindConfig.proposal_source, kindConfig.fields[kindConfig.proposal_source]).toLowerCase()}.</span>
    {:else if config.first_draft === 'human'}<span class="hint">First drafts are yours; agents can only suggest changes.</span>
    {:else if kindConfig?.agent_direct === false}<span class="hint">Agents' new and changed {titleOf(kind, kindConfig).toLowerCase()} records come to you as cards.</span>{/if}
    <button class="btn" onclick={startNew}>New {titleOf(kind, kindConfig).toLowerCase()}</button>
  </div>
</header>

{#if error}<Banner tone="crit" role="alert">{error}</Banner>{/if}

<div class="split">
  <section class="list-pane">
    <input class="search" type="search" placeholder="Search" bind:value={query} />
    {#if records.length === 0}
      <Empty>No {titleOf(kind, kindConfig).toLowerCase()} records yet.</Empty>
    {/if}
    <ul class="record-list">
      {#each records as record (record.id)}
        <li class:active={record.id === current?.id}>
          <Row href={href({ view: 'records', type: typeName, kind, id: record.id })} gap={0}>
            {#snippet lead()}<span class="bar" aria-hidden="true"></span>{/snippet}
            <span class="rec">
              <span class="label">{recordLabel(config, record)}</span>
              <span class="meta">
                {#if record.reviewed}<span class="badge ok">reviewed</span>{/if}
                <span class="badge author {record.created_by?.author}">{authorText(record.created_by)}</span>
                {#each (record.fields.tags as string[] | undefined) ?? [] as tag (tag)}<span class="tag">{tag}</span>{/each}
              </span>
            </span>
          </Row>
        </li>
      {/each}
    </ul>
  </section>

  <section class="detail-pane">
    {#if sinceReview && sinceReview.changes.length}
      <section class="card reviewed-diff">
        <h3>Changed since you reviewed it (v{sinceReview.version})</h3>
        <dl>
          {#each sinceReview.changes as change (change.name)}
            <dt>{titleOf(change.name, kindConfig.fields[change.name])}</dt>
            <dd class="diff">{#each change.parts as part, i (i)}<span class={part.kind}>{part.text}</span>{/each}</dd>
          {/each}
        </dl>
      </section>
    {/if}
    {#if creating || current}
      <form class="card form" onsubmit={save}>
        <div class="form-head">
          <h2>{creating ? `New ${titleOf(kind, kindConfig).toLowerCase()}` : recordLabel(config, current!)}</h2>
          {#if current}
            <div class="status-line">
              {#if current.reviewed}<span class="badge ok">reviewed {when(current.reviewed_at)}</span>{:else}<span class="badge">not reviewed</span>{/if}
              <span class="muted">v{current.version} · first draft by {authorText(current.created_by)} · last change by {authorText(current.updated_by)}</span>
            </div>
          {/if}
        </div>
        <div class="fields">
          {#each Object.entries(kindConfig.fields) as [name, field] (name)}
            <FieldInput {name} {field} {config} records={all} bind:value={form[name]} disabled={busy} />
          {/each}
        </div>
        <div class="form-actions">
          <button class="btn p" type="submit" disabled={busy || !dirty}>{creating ? 'Create' : 'Save'}</button>
          {#if current}
            <button class="btn" type="button" onclick={toggleReviewed} disabled={busy || dirty} title={dirty ? 'Save first' : ''}>
              {current.reviewed ? 'Mark not reviewed' : 'Mark reviewed'}
            </button>
            <button type="button" class="lnk d" onclick={remove} disabled={busy}>Delete</button>
          {/if}
          {#if current && dirty}<span class="hint">Saving clears the reviewed mark.</span>{/if}
        </div>
      </form>
      {#if current}
        <Revisions {revisions} />
      {/if}
    {:else}
      <p class="empty">Pick a record, or create one. Fields: {Object.keys(kindConfig.fields).map(n => titleOf(n, kindConfig.fields[n])).join(', ')}.</p>
    {/if}
  </section>
</div>
