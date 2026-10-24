<script lang="ts">
  import type { Revision } from './lib/api';
  import { authorText, display, when } from './lib/views';

  let { revisions }: { revisions: Revision[] } = $props();

  function changeText(revision: Revision): string {
    if (revision.change.startsWith('select:')) return `selected ${revision.change.slice(7)}`;
    return revision.change;
  }

  // What this revision changed relative to the one before it.
  function changed(index: number): string[] {
    const after = revisions[index].snapshot;
    const before = revisions[index + 1]?.snapshot;
    if (!before) return [];
    const names = new Set([...Object.keys(after.fields ?? {}), ...Object.keys(before.fields ?? {})]);
    const out = [...names].filter(n => display(after.fields?.[n]) !== display(before.fields?.[n]));
    if (after.title !== undefined && after.title !== before.title) out.unshift('title');
    return out;
  }
</script>

<section class="card revisions">
  <h3>History</h3>
  <ol>
    {#each revisions as revision, index (revision.version)}
      <li>
        <span class="badge author {revision.author}">{authorText(revision)}</span>
        <span>{changeText(revision)}</span>
        {#if changed(index).length}<span class="muted">({changed(index).join(', ')})</span>{/if}
        {#if revision.edited}<span class="badge">edited before accepting</span>{/if}
        <span class="muted right">v{revision.version} · {when(revision.created_at)}</span>
      </li>
    {/each}
  </ol>
</section>
