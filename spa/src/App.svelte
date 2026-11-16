<script lang="ts">
  import { onMount } from 'svelte';
  import ApprovalsView from './ApprovalsView.svelte';
  import DocumentView from './DocumentView.svelte';
  import DocumentsView from './DocumentsView.svelte';
  import RecordsView from './RecordsView.svelte';
  import TypesView from './TypesView.svelte';
  import { api, message, type AppState } from './lib/api';
  import { href, parseRoute, titleOf, type Route } from './lib/views';

  let app = $state<AppState | null>(null);
  let route = $state<Route>(parseRoute(location.hash));
  let loadError = $state('');
  let documentType = $state<string | null>(null);

  const accepted = $derived((app?.types ?? []).filter(t => t.config));
  const pendingTypes = $derived([...(app?.types ?? []), ...(app?.templates ?? [])].filter(t => t.state === 'new' || t.state === 'changed').length);

  async function refresh() {
    try {
      app = await api.state();
      loadError = '';
    } catch (e) { loadError = message(e, 'Could not reach hoja.'); }
  }

  function configOf(name: string | null) {
    return accepted.find(t => t.name === name)?.config ?? null;
  }

  $effect(() => {
    const r = route;
    if (r.view !== 'document') { documentType = null; return; }
    api.document(r.id).then(v => { documentType = v.document.type; }).catch(() => { documentType = null; });
  });

  onMount(() => {
    const onHash = () => { route = parseRoute(location.hash); };
    window.addEventListener('hashchange', onHash);
    refresh();
    // Agents open cards while the page is open; keep the count current.
    const timer = window.setInterval(refresh, 15000);
    return () => { window.removeEventListener('hashchange', onHash); window.clearInterval(timer); };
  });

  function active(test: Partial<Route> & { view: Route['view'] }): boolean {
    if (test.view === 'documents') return route.view === 'documents' || route.view === 'document';
    if (route.view !== test.view) return false;
    if (test.view === 'records' && route.view === 'records') {
      const t = test as { type?: string; kind?: string };
      return route.type === t.type && route.kind === t.kind;
    }
    return true;
  }
</script>

<div class="app-shell">
  <nav class="sidebar" aria-label="Sections">
    <a class="brand" href={href({ view: 'documents' })}><img src="/hoja.svg" alt="" width="26" height="26" /> hoja</a>
    <a class="nav" class:active={active({ view: 'documents' })} href={href({ view: 'documents' })}>Documents</a>
    <a class="nav" class:active={active({ view: 'approvals' })} href={href({ view: 'approvals' })}>
      Approvals {#if app?.pending_approvals}<span class="count">{app.pending_approvals}</span>{/if}
    </a>
    {#each accepted as t (t.name)}
      <p class="nav-group">{titleOf(t.name, t.config ?? undefined)}</p>
      {#each Object.entries(t.config?.kinds ?? {}) as [kind, kindConfig] (kind)}
        <a class="nav sub" class:active={active({ view: 'records', type: t.name, kind } as Route)} href={href({ view: 'records', type: t.name, kind })}>{titleOf(kind, kindConfig)}</a>
      {/each}
    {/each}
    <a class="nav settings" class:active={active({ view: 'types' })} href={href({ view: 'types' })}>
      Types {#if pendingTypes}<span class="count warn">{pendingTypes}</span>{/if}
    </a>
    {#if app}<p class="workspace" title={app.workspace}>{app.owner ?? 'Workspace'}<br /><span>{app.workspace}</span></p>{/if}
  </nav>

  <main class="content">
    {#if loadError}
      <p class="error" role="alert">{loadError}</p>
    {:else if !app}
      <p class="empty">Loading…</p>
    {:else if route.view === 'documents'}
      <DocumentsView types={app.types} onchange={refresh} />
    {:else if route.view === 'document'}
      {@const config = configOf(documentType)}
      {#if config}
        {#key route.id}<DocumentView id={route.id} {config} configs={Object.fromEntries(accepted.map(t => [t.name, t.config!]))} onchange={refresh} />{/key}
      {:else}
        <p class="empty">Loading…</p>
      {/if}
    {:else if route.view === 'records'}
      {@const config = configOf(route.type)}
      {#if config?.kinds[route.kind]}
        <RecordsView typeName={route.type} kind={route.kind} {config} selectedId={route.id} onchange={refresh} />
      {:else}
        <p class="empty">That record kind isn't in an accepted type.</p>
      {/if}
    {:else if route.view === 'approvals'}
      <ApprovalsView types={app.types} onchange={refresh} />
    {:else if route.view === 'types'}
      <TypesView types={app.types} typesError={app.types_error} templates={app.templates} templatesError={app.templates_error} onchange={refresh} />
    {/if}
  </main>
</div>
