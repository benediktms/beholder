<script lang="ts">
  import ForceGraph from 'force-graph';
  import { onMount } from 'svelte';
  import {
    endpointId,
    zoomBand,
    type GraphLink,
    type GraphNode,
    type Projection,
    type ZoomBand
  } from './graph';

  let {
    projection,
    band,
    onSelect,
    onBandChange
  }: {
    projection: Projection;
    band: ZoomBand;
    onSelect: (node: GraphNode | null) => void;
    onBandChange: (band: ZoomBand) => void;
  } = $props();

  let container: HTMLDivElement;
  let graph: ForceGraph<GraphNode, GraphLink> | null = null;
  let hoveredId = $state<string | null>(null);
  let hoveredNodes = new Set<string>();
  let hoveredLinks = new Set<string>();
  let keyboardIndex = $state(-1);

  onMount(() => {
    graph = new ForceGraph<GraphNode, GraphLink>(container)
      .backgroundColor('#080b12')
      .nodeId('id')
      .nodeVal((node) => Math.max(1.5, Math.log2(node.count + 1) * 2.2))
      .nodeLabel((node) => `${node.label} · ${node.count} ${node.count === 1 ? 'entity' : 'entities'}`)
      .nodeColor(nodeColor)
      .nodeCanvasObjectMode(() => 'after')
      .nodeCanvasObject(drawNodeOverlay)
      .linkLabel((link) => `${link.kind} · ${link.count} relationship${link.count === 1 ? '' : 's'}`)
      .linkColor(linkColor)
      .linkWidth((link) => (emphasized(link) ? 2.2 : Math.min(1.6, 0.5 + link.count * 0.18)))
      .linkLineDash((link) => (link.direction === 'neutral' ? null : [2, 5]))
      .linkDirectionalArrowLength(3.5)
      .linkDirectionalArrowRelPos(0.88)
      .linkDirectionalArrowColor(linkColor)
      .linkDirectionalParticles((link) => (emphasized(link) ? 2 : 0))
      .linkDirectionalParticleColor(linkColor)
      .linkDirectionalParticleSpeed(0.006)
      .linkDirectionalParticleWidth(2)
      .onNodeHover(setHover)
      .onNodeClick((node) => {
        onSelect(node);
        if (node.x !== undefined && node.y !== undefined) graph?.centerAt(node.x, node.y, 380);
      })
      .onBackgroundClick(() => onSelect(null))
      .onZoomEnd(({ k }) => {
        const next = zoomBand(k);
        if (next !== band) onBandChange(next);
      })
      .cooldownTicks(140);

    const resize = new ResizeObserver(([entry]) => {
      graph?.width(entry.contentRect.width).height(entry.contentRect.height);
    });
    resize.observe(container);
    updateData(projection);
    graph.zoom(0.55);
    return () => {
      resize.disconnect();
      graph?._destructor();
      graph = null;
    };
  });

  $effect(() => {
    const next = projection;
    if (graph) updateData(next);
  });

  $effect(() => {
    const requestedBand = band;
    if (graph && zoomBand(graph.zoom()) !== requestedBand) {
      graph.zoom({ repository: 0.55, module: 1, file: 2, entity: 3.5 }[requestedBand], 350);
    }
  });

  function updateData(next: Projection) {
    if (!graph) return;
    const previousByEntity = new Map<string, GraphNode>();
    for (const node of graph.graphData().nodes) {
      for (const rawId of node.rawEntityIds) previousByEntity.set(rawId, node);
    }
    for (const node of next.nodes) {
      const previous = node.rawEntityIds.map((id) => previousByEntity.get(id)).find(Boolean);
      if (previous?.x !== undefined && previous.y !== undefined) {
        node.x = previous.x;
        node.y = previous.y;
      }
    }
    hoveredId = null;
    keyboardIndex = -1;
    hoveredNodes = new Set();
    hoveredLinks = new Set();
    graph.graphData({ nodes: next.nodes, links: next.links }).d3ReheatSimulation();
  }

  function setHover(node: GraphNode | null) {
    hoveredId = node?.id ?? null;
    hoveredNodes = new Set(node ? [node.id] : []);
    hoveredLinks = new Set();
    if (node) {
      for (const link of projection.links) {
        const source = endpointId(link.source);
        const target = endpointId(link.target);
        if (source === node.id || target === node.id) {
          hoveredLinks.add(link.id);
          hoveredNodes.add(source);
          hoveredNodes.add(target);
        }
      }
    }
    refreshStyles();
  }

  function handleKeydown(event: KeyboardEvent) {
    if (!projection.nodes.length) return;
    if (['ArrowRight', 'ArrowDown', 'ArrowLeft', 'ArrowUp'].includes(event.key)) {
      event.preventDefault();
      const step = event.key === 'ArrowRight' || event.key === 'ArrowDown' ? 1 : -1;
      keyboardIndex = (keyboardIndex + step + projection.nodes.length) % projection.nodes.length;
      setHover(projection.nodes[keyboardIndex]);
    } else if (event.key === 'Enter' && keyboardIndex >= 0) {
      onSelect(projection.nodes[keyboardIndex]);
    } else if (event.key === 'Escape') {
      keyboardIndex = -1;
      setHover(null);
    }
  }

  function refreshStyles() {
    graph
      ?.nodeColor(nodeColor)
      .linkColor(linkColor)
      .linkWidth((link) => (emphasized(link) ? 2.2 : Math.min(1.6, 0.5 + link.count * 0.18)))
      .linkDirectionalParticles((link) => (emphasized(link) ? 2 : 0));
  }

  function emphasized(link: GraphLink): boolean {
    return hoveredId ? hoveredLinks.has(link.id) : link.animated;
  }

  function nodeColor(node: GraphNode): string {
    if (hoveredId && !hoveredNodes.has(node.id)) return 'rgba(51, 65, 85, 0.25)';
    if (node.direction === 'upstream') return '#5eead4';
    if (node.direction === 'downstream') return '#fb7185';
    if (node.direction === 'both') return '#f8fafc';
    if (node.level === 'repository') return '#7dd3fc';
    if (node.level === 'module') return '#fbbf24';
    if (node.level === 'file') return '#c4b5fd';
    return '#94a3b8';
  }

  function linkColor(link: GraphLink): string {
    if (hoveredId && !hoveredLinks.has(link.id)) return 'rgba(51, 65, 85, 0.16)';
    if (link.direction === 'upstream') return '#2dd4bf';
    if (link.direction === 'downstream') return '#fb7185';
    if (link.direction === 'both') return '#e2e8f0';
    if (link.selected) return '#f8fafc';
    return 'rgba(100, 116, 139, 0.58)';
  }

  function drawNodeOverlay(
    node: GraphNode,
    context: CanvasRenderingContext2D,
    scale: number
  ) {
    if (node.x === undefined || node.y === undefined) return;
    const radius = Math.sqrt(Math.max(1.5, Math.log2(node.count + 1) * 2.2)) * 4;
    if (node.selected) {
      context.beginPath();
      context.arc(node.x, node.y, radius + 3 / scale, 0, 2 * Math.PI);
      context.strokeStyle = '#ffffff';
      context.lineWidth = 2.4 / scale;
      context.stroke();
    }
    if (node.level === 'entity' && !node.selected && hoveredId !== node.id) return;
    const fontSize = 11 / scale;
    context.font = `600 ${fontSize}px Inter, ui-sans-serif, system-ui`;
    context.textAlign = 'center';
    context.textBaseline = 'top';
    context.fillStyle = hoveredId && !hoveredNodes.has(node.id) ? '#475569' : '#e2e8f0';
    context.fillText(node.label, node.x, node.y + radius + 4 / scale);
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex (canvas-backed composite widget exposes its keyboard model here) -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions (canvas-backed composite widget exposes its keyboard model here) -->
<div
  class="graph"
  bind:this={container}
  role="application"
  tabindex="0"
  onkeydown={handleKeydown}
  aria-label="Interactive semantic graph with {projection.nodes.length} visible nodes and {projection.links.length} visible links. Use arrow keys to inspect nodes and Enter to select."
></div>
<span class="sr-only" aria-live="polite">
  {hoveredId ? projection.nodes.find((node) => node.id === hoveredId)?.label : ''}
</span>

<style>
  .graph {
    height: 100%;
    min-height: 26rem;
    overflow: hidden;
    width: 100%;
  }

  .graph:focus-visible { outline: 2px solid var(--ring); outline-offset: -2px; }
  .sr-only { height: 1px; margin: -1px; overflow: hidden; position: absolute; width: 1px; clip: rect(0, 0, 0, 0); }
</style>
