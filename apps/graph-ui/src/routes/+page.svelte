<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import GraphCanvas from '$lib/GraphCanvas.svelte';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import {
    MAX_VISIBLE_LINKS,
    MAX_VISIBLE_NODES,
    EXTERNAL_REPOSITORY,
    ORIGINS,
    RELATION_KINDS,
    projectGraph,
    type EntityOrigin,
    type GraphNode,
    type GraphSnapshot,
    type Projection,
    type RelationKind,
    type WorkspaceSummary,
    type ZoomBand
  } from '$lib/graph';

  const emptyProjection: Projection = {
    nodes: [],
    links: [],
    rawNodeCount: 0,
    rawLinkCount: 0,
    omittedNodes: 0,
    omittedLinks: 0,
    omittedAnimations: 0,
    truncated: false
  };

  let workspaces: WorkspaceSummary[] = [];
  let selectedWorkspace = '';
  let snapshot: GraphSnapshot | null = null;
  let repository = '';
  let relationKinds: RelationKind[] = [...RELATION_KINDS];
  let origins: EntityOrigin[] = [...ORIGINS];
  let includeTests = true;
  let band: ZoomBand = 'repository';
  let rootId: string | null = null;
  let selectedId: string | null = null;
  let loading = true;
  let error = '';

  $: projection = snapshot
    ? projectGraph(snapshot, {
        band,
        repository: repository || null,
        relationKinds,
        includeTests,
        origins,
        rootId,
        selectedId
      })
    : emptyProjection;
  $: selectedEntity = snapshot?.nodes.find((node) => node.id === rootId) ?? null;
  $: selectedEdges = snapshot && rootId
    ? snapshot.edges.filter((edge) => edge.from === rootId || edge.to === rootId)
    : [];

  onMount(async () => {
    try {
      workspaces = await invoke<WorkspaceSummary[]>('list_workspaces');
      selectedWorkspace = workspaces[0]?.name ?? '';
      if (selectedWorkspace) await loadWorkspace();
    } catch (cause) {
      error = String(cause);
    } finally {
      loading = false;
    }
  });

  async function loadWorkspace() {
    if (!selectedWorkspace) return;
    loading = true;
    error = '';
    try {
      snapshot = await invoke<GraphSnapshot>('load_graph', {
        request: { workspace: selectedWorkspace }
      });
      returnToWorkspace();
    } catch (cause) {
      error = String(cause);
    } finally {
      loading = false;
    }
  }

  function changeWorkspace(event: Event) {
    selectedWorkspace = (event.currentTarget as HTMLSelectElement).value;
    void loadWorkspace();
  }

  function selectNode(node: GraphNode | null) {
    if (!node) {
      selectedId = null;
      return;
    }
    selectedId = node.id;
    if (node.level === 'repository') {
      const raw = snapshot?.nodes.find((entity) => node.rawEntityIds.includes(entity.id));
      repository = raw?.repository ?? EXTERNAL_REPOSITORY;
      rootId = null;
      band = 'module';
    } else if (node.level === 'entity' && node.rawEntityIds.length === 1) {
      rootId = node.rawEntityIds[0];
      repository = '';
    }
  }

  function clearFocus() {
    rootId = null;
    selectedId = null;
  }

  function returnToWorkspace() {
    repository = '';
    clearFocus();
    band = 'repository';
  }

  function returnToRepository() {
    clearFocus();
    band = 'module';
  }

  function selectAllRelations() {
    relationKinds = [...RELATION_KINDS];
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
      {#if repository}
        <span aria-hidden="true">/</span>
        <Button variant="ghost" onclick={returnToRepository}>
          {snapshot?.workspace.repositories.find((item) => item.identity === repository)?.displayName ?? repository}
        </Button>
      {/if}
      {#if selectedEntity}
        <span aria-hidden="true">/</span>
        <span class="current">{selectedEntity.name}</span>
      {/if}
    </nav>

    <div class="header-meta">
      <Badge>fixture · rev {snapshot?.metadata.revision ?? '—'}</Badge>
      <span class:healthy={Boolean(snapshot && !snapshot.metadata.freshness.stale)} class="status-dot"></span>
      <span>{snapshot?.metadata.freshness.stale ? 'stale' : 'snapshot ready'}</span>
    </div>
  </header>

  <main>
    <aside class="filters" aria-label="Graph filters">
      <div class="panel-heading">
        <div><span class="eyebrow">Scope</span><h2>Workspace view</h2></div>
        <Badge>{band}</Badge>
      </div>

      <label>
        <span>Workspace</span>
        <select value={selectedWorkspace} onchange={changeWorkspace}>
          {#each workspaces as workspace}
            <option value={workspace.name}>{workspace.name}</option>
          {/each}
        </select>
      </label>

      <label>
        <span>Repository</span>
        <select bind:value={repository} onchange={clearFocus}>
          <option value="">All repositories</option>
          {#each snapshot?.workspace.repositories ?? [] as item}
            <option value={item.identity}>{item.displayName}</option>
          {/each}
          {#if snapshot?.nodes.some((node) => node.repository === null)}
            <option value={EXTERNAL_REPOSITORY}>External contracts</option>
          {/if}
        </select>
      </label>

      <div class="field">
        <div class="field-title">
          <span>Relationship kind</span>
          <button class="text-button" onclick={selectAllRelations}>Select all</button>
        </div>
        <select multiple size="7" bind:value={relationKinds} aria-label="Relationship kinds">
          {#each RELATION_KINDS as kind}
            <option value={kind}>{kind.replaceAll('_', ' ')}</option>
          {/each}
        </select>
      </div>

      <label>
        <span>Origin</span>
        <select multiple size="3" bind:value={origins} aria-label="Entity origins">
          {#each ORIGINS as origin}
            <option value={origin}>{origin.replaceAll('_', ' ')}</option>
          {/each}
        </select>
      </label>

      <label class="check-row">
        <input type="checkbox" bind:checked={includeTests} />
        <span>Include tests</span>
      </label>

      <div class="guard-note">
        <span class="eyebrow">Render guard</span>
        <strong>{MAX_VISIBLE_NODES.toLocaleString()} nodes · {MAX_VISIBLE_LINKS.toLocaleString()} links</strong>
        <p>The full fixture stays loaded. Oversized projections stop here and ask for narrower filters.</p>
      </div>
    </aside>

    <section class="workspace" aria-busy={loading}>
      <div class="graph-toolbar">
        <div>
          <span class="eyebrow">{rootId ? 'Reachable topology' : 'Aggregated topology'}</span>
          <h1>{selectedEntity?.name ?? snapshot?.workspace.name ?? 'Loading graph'}</h1>
        </div>
        <div class="legend" aria-label="Path colors">
          <span><i class="upstream"></i>Upstream</span>
          <span><i class="downstream"></i>Downstream</span>
          <span><i class="neutral"></i>Unfocused</span>
        </div>
        <div class="toolbar-actions">
          {#if rootId}<Button variant="outline" onclick={clearFocus}>Clear focus</Button>{/if}
          <Badge>{projection.nodes.length} nodes</Badge>
          <Badge>{projection.links.length} links</Badge>
        </div>
      </div>

      {#if projection.truncated}
        <div class="warning" role="alert">
          {#if snapshot?.traversal.truncated}
            <strong>The loaded traversal is incomplete.</strong> Narrow the backend query before treating this view as complete.
          {:else}
            <strong>Visible graph truncated.</strong>
            Omitted {projection.omittedNodes} nodes, {projection.omittedLinks} links, and
            {projection.omittedAnimations} animations. Narrow repository, relationship, origin, tests, or zoom before continuing.
          {/if}
        </div>
      {/if}

      <div class="canvas-wrap">
        {#if error}
          <div class="empty"><strong>Could not load the Tauri fixture.</strong><span>{error}</span></div>
        {:else if loading}
          <div class="empty"><span class="loader"></span><strong>Loading typed graph…</strong></div>
        {:else}
          <GraphCanvas {projection} {band} onSelect={selectNode} onBandChange={(next) => (band = next)} />
          <div class="zoom-hint">Zoom to expand · hover to inspect neighbours · click an entity to re-root</div>
        {/if}
      </div>
    </section>

    <aside class="inspector" aria-label="Entity inspector">
      <div class="panel-heading">
        <div><span class="eyebrow">Inspector</span><h2>{selectedEntity ? 'Entity detail' : 'How to explore'}</h2></div>
      </div>

      {#if selectedEntity}
        <div class="entity-icon">{selectedEntity.kind.slice(0, 2).toUpperCase()}</div>
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
          <div><span>01</span><p>Start with repositories bundled across the fixture workspace.</p></div>
          <div><span>02</span><p>Zoom to reconnect topology through modules, files, types, and functions.</p></div>
          <div><span>03</span><p>Select an entity to show every reachable upstream and downstream path.</p></div>
        </div>
        <div class="interaction-key">
          <p><strong>Hover</strong> highlights direct neighbours and incident links.</p>
          <p><strong>Select</strong> persists the outline and animates only emphasized links.</p>
        </div>
      {/if}
    </aside>
  </main>
</div>
