<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { QueryClient } from '@tanstack/svelte-query';
  import { onMount } from 'svelte';
  import GraphCanvas from '$lib/GraphCanvas.svelte';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import {
    EXTERNAL_REPOSITORY,
    ORIGINS,
    RELATION_KINDS,
    extendTrail,
    findEntity,
    projectGraph,
    type EntityOrigin,
    type GraphNode,
    type GraphSnapshot,
    type Projection,
    type RelationKind, type QueryMetadata,
    type WorkspaceSummary
  } from '$lib/graph';

  const emptyProjection: Projection = {
    nodes: [],
    links: [],
    rawNodeCount: 0,
    rawLinkCount: 0,
    omittedNodes: 0,
    omittedLinks: 0,
    truncated: false
  };
  const MIN_DRAWER_WIDTH = 192;
  const MAX_DRAWER_WIDTH = 512;
  const queryClient = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity, gcTime: 30 * 60 * 1000 } }
  });

  let workspaces: WorkspaceSummary[] = [];
  let selectedWorkspace = '';
  let snapshot: GraphSnapshot | null = null;
  let repositories: string[] = [];
  let relationKinds: RelationKind[] = [...RELATION_KINDS];
  let origins: EntityOrigin[] = [...ORIGINS];
  let includeTests = true;
  let rootId: string | null = null;
  let trail: string[] = [];
  let filtersOpen = true;
  let inspectorOpen = true;
  let filtersWidth = 240;
  let inspectorWidth = 288;
  let loading = true;
  let error = '';
  let statusError = '';
  let status: QueryMetadata | null = null;
  let search = '';
  let loadRequest = 0;
  let pollingStatus = false;

  $: projection = snapshot
    ? projectGraph(snapshot, {
        repositories,
        relationKinds,
        includeTests,
        origins
      })
    : emptyProjection;
  $: repositoryOptions = [
    ...(snapshot?.workspace.repositories ?? []),
    ...(snapshot?.nodes.some((node) => node.repository === null)
      ? [{ identity: EXTERNAL_REPOSITORY, displayName: 'External contracts' }]
      : [])
  ];
  $: repositoryLabel = repositories.length === 0
    ? 'All repositories'
    : repositories.length === 1
      ? repositoryOptions.find((item) => item.identity === repositories[0])?.displayName ?? repositories[0]
      : `${repositories.length} repositories`;
  $: selectedEntity = snapshot?.nodes.find((node) => node.id === rootId) ?? null;
  $: selectedEdges = snapshot && rootId
    ? snapshot.edges.filter((edge) => edge.from === rootId || edge.to === rootId)
    : [];
  $: pinnedAnalysis = snapshot?.metadata.analysis ?? { completeness: 'complete' as const, diagnostics: [] };
  $: currentFreshness = status?.freshness ?? snapshot?.metadata.freshness ?? null;

  onMount(() => {
    void loadWorkspaces();
    const poll = window.setInterval(() => void pollStatus(), 3000);
    return () => window.clearInterval(poll);
  });

  async function loadWorkspaces() {
    const request = ++loadRequest;
    loading = true;
    error = '';
    try {
      const nextWorkspaces = await invoke<WorkspaceSummary[]>('list_workspaces');
      if (request !== loadRequest) return;
      workspaces = nextWorkspaces;
      selectedWorkspace = workspaces.some(({ name }) => name === selectedWorkspace)
        ? selectedWorkspace
        : workspaces[0]?.name ?? '';
      if (selectedWorkspace) await loadWorkspace();
      else {
        snapshot = null;
        status = null;
      }
    } catch (cause) {
      if (request === loadRequest) error = String(cause);
    } finally {
      if (request === loadRequest) loading = false;
    }
  }

  async function pollStatus() {
    if (!selectedWorkspace || !snapshot || pollingStatus) return;
    const generation = loadRequest;
    const workspace = selectedWorkspace;
    pollingStatus = true;
    try {
      const nextStatus = await invoke<QueryMetadata>('topology_status', { request: { workspace } });
      if (generation !== loadRequest) return;
      status = nextStatus;
      statusError = '';
    } catch (cause) {
      if (generation === loadRequest) statusError = String(cause);
    } finally {
      pollingStatus = false;
    }
  }

  async function loadWorkspace() {
    if (!selectedWorkspace) return;
    const request = ++loadRequest;
    const workspace = selectedWorkspace;
    loading = true;
    error = '';
    try {
      const nextStatus = await invoke<QueryMetadata>('topology_status', { request: { workspace } });
      const nextSnapshot = await queryClient.fetchQuery({
        queryKey: ['topology', workspace, nextStatus.revision],
        queryFn: () => invoke<GraphSnapshot>('load_graph', { request: { workspace } })
      });
      if (request !== loadRequest) return;
      snapshot = nextSnapshot;
      status = snapshot.metadata;
      returnToWorkspace();
    } catch (cause) {
      if (request === loadRequest) error = String(cause);
    } finally {
      if (request === loadRequest) loading = false;
    }
  }

  function selectSearch() {
    const entity = snapshot && findEntity(snapshot.nodes, search);
    const id = entity?.id ?? search.trim();
    if (id && snapshot?.nodes.some((node) => node.id === id)) selectEntity(id);
  }

  function changeWorkspace(event: Event) {
    selectedWorkspace = (event.currentTarget as HTMLSelectElement).value;
    void loadWorkspace();
  }

  function selectNode(node: GraphNode | null) {
    if (!node) {
      clearFocus();
      return;
    }
    selectEntity(node.id);
  }

  function selectEntity(entity: string) {
    if (!snapshot) return;
    const connected = rootId !== null && snapshot?.edges.some(
      (edge) =>
        (edge.from === rootId && edge.to === entity) ||
        (edge.to === rootId && edge.from === entity)
    );
    trail = extendTrail({ trail, next: entity, connected: Boolean(connected) });
    rootId = entity;
  }

  function clearFocus() {
    rootId = null;
    trail = [];
  }

  function stepBack() {
    if (trail.length < 2) return;
    trail = trail.slice(0, -1);
    rootId = trail.at(-1) ?? null;
  }

  function returnToWorkspace() {
    repositories = [];
    clearFocus();
  }

  function returnToRepository() {
    clearFocus();
  }

  function selectAllRelations() {
    relationKinds = [...RELATION_KINDS];
  }

  function selectAllRepositories() {
    repositories = [];
    clearFocus();
  }

  function toggleRepository(identity: string) {
    const next = repositories.includes(identity)
      ? repositories.filter((repository) => repository !== identity)
      : [...repositories, identity];
    repositories = next.length === repositoryOptions.length ? [] : next;
    clearFocus();
  }

  function toggleRelationKind(kind: RelationKind) {
    relationKinds = relationKinds.includes(kind)
      ? relationKinds.filter((selected) => selected !== kind)
      : [...relationKinds, kind];
  }

  function toggleOrigin(origin: EntityOrigin) {
    origins = origins.includes(origin)
      ? origins.filter((selected) => selected !== origin)
      : [...origins, origin];
    clearFocus();
  }

  function toggleTests() {
    includeTests = !includeTests;
    clearFocus();
  }

  function setDrawerWidth(side: 'filters' | 'inspector', width: number) {
    const next = Math.min(MAX_DRAWER_WIDTH, Math.max(MIN_DRAWER_WIDTH, width));
    if (side === 'filters') filtersWidth = next;
    else inspectorWidth = next;
  }

  function startDrawerResize(event: PointerEvent, side: 'filters' | 'inspector') {
    event.preventDefault();
    const handle = event.currentTarget as HTMLButtonElement;
    const startX = event.clientX;
    const startWidth = side === 'filters' ? filtersWidth : inspectorWidth;
    handle.setPointerCapture(event.pointerId);

    const move = (next: PointerEvent) => {
      const delta = side === 'filters' ? next.clientX - startX : startX - next.clientX;
      setDrawerWidth(side, startWidth + delta);
    };
    const stop = (end: PointerEvent) => {
      if (handle.hasPointerCapture(end.pointerId)) handle.releasePointerCapture(end.pointerId);
      handle.removeEventListener('pointermove', move);
      handle.removeEventListener('pointerup', stop);
      handle.removeEventListener('pointercancel', stop);
    };

    handle.addEventListener('pointermove', move);
    handle.addEventListener('pointerup', stop);
    handle.addEventListener('pointercancel', stop);
  }

  function resizeDrawerWithKeyboard(event: KeyboardEvent, side: 'filters' | 'inspector') {
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
    event.preventDefault();
    const direction = event.key === 'ArrowRight' ? 1 : -1;
    const current = side === 'filters' ? filtersWidth : inspectorWidth;
    setDrawerWidth(side, current + direction * (side === 'filters' ? 16 : -16));
  }
</script>

<svelte:head><title>Beholder Graph</title></svelte:head>

<div class="shell">
  <header class="topbar">
    <div class="brand">
      <div class="mark" aria-hidden="true">B</div>
      <div>
        <strong>Beholder</strong>
        <span>Architecture graph</span>
      </div>
    </div>

    <nav class="breadcrumbs" aria-label="Graph location">
      <Button variant="ghost" onclick={returnToWorkspace}>Workspace</Button>
      {#if repositories.length}
        <span aria-hidden="true">/</span>
        <Button variant="ghost" onclick={returnToRepository}>
          {repositoryLabel}
        </Button>
      {/if}
      {#if selectedEntity}
        <span aria-hidden="true">/</span>
        <span class="current">{selectedEntity.name}</span>
      {/if}
    </nav>

    <div class="header-meta">
      <Badge>rev {snapshot?.metadata.revision ?? '—'}</Badge>
      <span class:healthy={Boolean(snapshot && currentFreshness && !currentFreshness.stale && pinnedAnalysis.completeness === 'complete')} class="status-dot"></span>
      <span>{!snapshot ? 'no snapshot' : pinnedAnalysis.completeness === 'incomplete' ? 'analysis incomplete' : currentFreshness?.stale ? 'stale' : 'snapshot ready'}</span>
      {#if pinnedAnalysis.completeness === 'incomplete'}
        <details class="analysis-diagnostics">
          <summary>{pinnedAnalysis.diagnostics.length} diagnostics</summary>
          <ul>
            {#each pinnedAnalysis.diagnostics as diagnostic}
              <li>
                <strong>{diagnostic.code}</strong>
                <span>{diagnostic.repository}/{diagnostic.path}:{diagnostic.line ?? '—'}</span>
                {#if diagnostic.detail}<span>{diagnostic.detail}</span>{/if}
              </li>
            {/each}
          </ul>
        </details>
      {/if}
      {#if statusError}<span>{statusError}</span>{/if}
      {#if status && snapshot && status.revision > snapshot.metadata.revision}
        <Button size="sm" variant="outline" onclick={loadWorkspace}>Revision {status.revision} available · Refresh</Button>
      {/if}
    </div>
  </header>

  <main>
    <details class="drawer filters" bind:open={filtersOpen} style:width={`${filtersWidth}px`}>
      <summary>
        <span class="drawer-heading"><span class="eyebrow">Filters</span><strong>Workspace view</strong></span>
        <span class="drawer-label">Filters</span>
        <i aria-hidden="true"></i>
      </summary>
      <div class="drawer-content">
        <details class="filter-section" open>
          <summary><span>Scope</span><i aria-hidden="true"></i></summary>
          <div class="filter-section-content">
            <label>
              <span>Workspace</span>
              <select value={selectedWorkspace} onchange={changeWorkspace}>
                {#each workspaces as workspace}
                  <option value={workspace.name}>{workspace.name}</option>
                {/each}
              </select>
            </label>

            <div class="repo-field">
              <span>Repository</span>
              <details class="multi-select">
                <summary>{repositoryLabel}</summary>
                <div class="multi-options">
                  <label>
                    <input type="checkbox" checked={repositories.length === 0} onchange={selectAllRepositories} />
                    <span>All repositories</span>
                  </label>
                  {#each repositoryOptions as item}
                    <label>
                      <input
                        type="checkbox"
                        checked={repositories.includes(item.identity)}
                        onchange={() => toggleRepository(item.identity)}
                      />
                      <span>{item.displayName}</span>
                    </label>
                  {/each}
                </div>
              </details>
            </div>
          </div>
        </details>

        <details class="filter-section" open>
          <summary><span>Relationship kind</span><i aria-hidden="true"></i></summary>
          <div class="filter-section-content">
            <button class="text-button select-all" onclick={selectAllRelations}>Select all</button>
            <div class="filter-options" aria-label="Relationship kinds">
              {#each RELATION_KINDS as kind}
                <Button
                  size="sm"
                  variant="outline"
                  aria-pressed={relationKinds.includes(kind)}
                  onclick={() => toggleRelationKind(kind)}
                >
                  {kind.replaceAll('_', ' ')}
                </Button>
              {/each}
            </div>
          </div>
        </details>

        <details class="filter-section" open>
          <summary><span>Origin</span><i aria-hidden="true"></i></summary>
          <div class="filter-section-content">
            <div class="filter-options" aria-label="Entity origins">
              {#each ORIGINS as origin}
                <Button
                  size="sm"
                  variant="outline"
                  aria-pressed={origins.includes(origin)}
                  onclick={() => toggleOrigin(origin)}
                >
                  {origin.replaceAll('_', ' ')}
                </Button>
              {/each}
            </div>
          </div>
        </details>

        <Button
          class="switch-button"
          variant="outline"
          role="switch"
          aria-checked={includeTests}
          onclick={toggleTests}
        >
          <span class:enabled={includeTests} class="switch-track" aria-hidden="true"><span></span></span>
          <span>Include tests</span>
        </Button>

      </div>
    </details>
    {#if filtersOpen}
      <button
        class="drawer-resizer"
        type="button"
        aria-label="Resize filters panel"
        title="Drag or use arrow keys to resize filters"
        onpointerdown={(event) => startDrawerResize(event, 'filters')}
        onkeydown={(event) => resizeDrawerWithKeyboard(event, 'filters')}
      ></button>
    {/if}

    <section class="workspace" aria-busy={loading}>
      <div class="graph-toolbar">
        <div>
          <span class="eyebrow">{rootId ? 'Relationship focus' : 'Workspace topology'}</span>
          <h1>{selectedEntity?.name ?? snapshot?.workspace.name ?? 'Loading graph'}</h1>
        </div>
        <div class="legend" aria-label="Highlight colors">
          <span><i class="selected"></i>Selected</span>
          <span><i class="upstream"></i>Upstream</span>
          <span><i class="downstream"></i>Downstream</span>
          <span><i class="path"></i>Path</span>
        </div>
        <div class="toolbar-actions">
          <input aria-label="Search entities" placeholder="Canonical ID or visible name" bind:value={search} onkeydown={(event) => event.key === 'Enter' && selectSearch()} />
          <Button variant="outline" onclick={selectSearch}>Find</Button>
          {#if trail.length > 1}<Button variant="outline" onclick={stepBack}>Back</Button>{/if}
          {#if rootId}<Button variant="outline" onclick={clearFocus}>Clear focus</Button>{/if}
          {#if trail.length > 1}<Badge>{trail.length} nodes in path</Badge>{/if}
          <Badge>{projection.nodes.length}{projection.omittedNodes ? ` / ${projection.rawNodeCount}` : ''} nodes</Badge>
          <Badge>{projection.links.length}{projection.omittedLinks ? ` / ${projection.rawLinkCount}` : ''} links</Badge>
        </div>
      </div>

      <div class="canvas-wrap">
        {#if error}
          <div class="empty">
            <strong>Could not load the workspace graph.</strong>
            <span>{error}</span>
            <Button variant="outline" onclick={loadWorkspaces}>Retry</Button>
          </div>
        {:else if loading}
          <div class="empty"><span class="loader"></span><strong>Loading typed graph…</strong></div>
        {:else if workspaces.length === 0}
          <div class="empty">
            <strong>No workspaces registered.</strong>
            <span>Register a workspace with the Beholder CLI, then retry.</span>
            <Button variant="outline" onclick={loadWorkspaces}>Retry</Button>
          </div>
        {:else}
          <GraphCanvas
            {projection}
            selectedId={rootId}
            viewKey={selectedWorkspace}
            {trail}
            onSelect={selectNode}
          />
          <div class="zoom-hint">Scroll to zoom · click an entity to highlight its immediate context</div>
        {/if}
      </div>
    </section>

    {#if inspectorOpen}
      <button
        class="drawer-resizer"
        type="button"
        aria-label="Resize inspector panel"
        title="Drag or use arrow keys to resize inspector"
        onpointerdown={(event) => startDrawerResize(event, 'inspector')}
        onkeydown={(event) => resizeDrawerWithKeyboard(event, 'inspector')}
      ></button>
    {/if}
    <details class="drawer inspector" bind:open={inspectorOpen} style:width={`${inspectorWidth}px`}>
      <summary>
        <span class="drawer-heading"><span class="eyebrow">Inspector</span><strong>{selectedEntity ? 'Entity detail' : 'How to explore'}</strong></span>
        <span class="drawer-label">Inspector</span>
        <i aria-hidden="true"></i>
      </summary>
      <div class="drawer-content">
        {#if selectedEntity}
          <h3>{selectedEntity.name}</h3>
          <code>{selectedEntity.id}</code>
          <dl>
            <div><dt>Kind</dt><dd>{selectedEntity.kind}</dd></div>
            <div><dt>Origin</dt><dd>{selectedEntity.origin.replaceAll('_', ' ')}</dd></div>
            <div><dt>Repository</dt><dd>{selectedEntity.repository ?? 'external'}</dd></div>
            <div><dt>Test</dt><dd>{selectedEntity.test ? 'yes' : 'no'}</dd></div>
          </dl>
          <div class="relationships">
            <span class="eyebrow">Incident evidence</span>
            {#each selectedEdges as edge}
              <article>
                <div><Badge>{edge.kind}</Badge><span>{edge.from === rootId ? 'downstream' : 'upstream'}</span></div>
                <strong>{snapshot?.nodes.find((node) => node.id === (edge.from === rootId ? edge.to : edge.from))?.name}</strong>
                {#if edge.evidence[0]?.path}
                  <small>{edge.evidence[0].path}:{edge.evidence[0].line ?? '—'}</small>
                {/if}
              </article>
            {/each}
          </div>
        {:else}
          <div class="instructions">
            <div><span>01</span><p>Explore the complete typed topology for the selected workspace.</p></div>
            <div><span>02</span><p>Use repository, relationship, origin, and test filters to narrow the view.</p></div>
            <div><span>03</span><p>Select connected entities to preserve a highlighted traversal path.</p></div>
          </div>
          <div class="interaction-key">
            <p><strong>Hover</strong> highlights direct neighbours and incident links.</p>
            <p><strong>Select</strong> highlights immediate context and extends the visible path.</p>
          </div>
        {/if}
      </div>
    </details>
  </main>
</div>
