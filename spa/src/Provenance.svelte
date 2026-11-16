<script lang="ts">
  import type { Provenance, Span } from './lib/api';

  // The skim view: every word marked by where it came from. Rendered here,
  // from the server's attribution, never by a template.
  let { provenance, approvedAt }: { provenance: Provenance; approvedAt: string | null } = $props();

  const s = $derived(provenance.summary);

  function tip(span: Span): string {
    const who = span.agent ?? 'an agent';
    const at = span.version ? ` (v${span.version})` : '';
    const what = {
      person: 'Yours',
      reviewed: `Written by ${who}${at}, reviewed by you`,
      accepted: `Worded by ${who}${at}, accepted by you on a card`,
      agent: `Written by ${who}${at}, not reviewed`,
      layout: ''
    }[span.origin];
    if (span.removed) return 'Removed since you approved it';
    return span.changed ? `${what}; changed since you approved it` : what;
  }
</script>

{#snippet spans(list: Span[])}{#each list as span, i (i)}<span class="pv {span.origin}" class:changed={span.changed} class:removed={span.removed} title={tip(span) || undefined}>{span.text}</span>{/each}{/snippet}

<div class="provenance">
  <div class="legend">
    <span>Unmarked: yours, or reviewed</span>
    <span class="pv accepted">agent wording you accepted ({s.accepted_words})</span>
    <span class="pv agent">agent text, not reviewed ({s.agent_words})</span>
    {#if provenance.baseline}
      <span class="pv changed">changed since approval ({s.changed_words})</span>
      <span class="pv removed">removed ({s.removed_words})</span>
    {:else if approvedAt}
      <span>Approved before hoja kept what you approved, so changes aren't marked.</span>
    {/if}
  </div>

  <h1>{@render spans(provenance.title)}</h1>
  {#each provenance.fields as field (field.name)}
    {#if field.long}
      <h2>{field.title}</h2>
      <p class="prose">{@render spans(field.spans)}</p>
    {:else}
      <p class="meta-field"><b>{field.title}:</b> {@render spans(field.spans)}</p>
    {/if}
  {/each}

  {#each provenance.sections as section (section.name)}
    {#if section.groups.some(g => g.items.length) || section.removed.length}
      <h2>{section.title}</h2>
      {#each section.groups as group, gi (gi)}
        {#if group.heading}<h3>{@render spans(group.heading)}</h3>{/if}
        <ul>
          {#each group.items as item (item.id)}
            <li class:added={item.added} title={item.added ? 'Selected since you approved it' : undefined}>
              <span class="prose">{@render spans(item.label)}</span>
              {#each item.fields as field (field.name)}
                <div class="meta-field"><b>{field.title}:</b> <span class="prose">{@render spans(field.spans)}</span></div>
              {/each}
            </li>
          {/each}
        </ul>
      {/each}
      {#if section.removed.length}
        <p class="removed-items">Removed since approval: {section.removed.join('; ')}</p>
      {/if}
    {/if}
  {/each}
</div>
