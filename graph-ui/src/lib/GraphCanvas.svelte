<script lang="ts">
  import { MultiDirectedGraph } from 'graphology';
  import forceAtlas2 from 'graphology-layout-forceatlas2';
  import FA2Layout from 'graphology-layout-forceatlas2/worker';
  import Sigma from 'sigma';
  import { createEdgeArrowProgram, type NodeHoverDrawingFunction } from 'sigma/rendering';
  import { onMount } from 'svelte';
  import {
    directHighlight,
    endpointId,
    type GraphLink,
    type GraphNode,
    type Projection
  } from './graph';

  type NodeAttributes = Record<string, unknown> & {
    label: string;
    x: number;
    y: number;
    size: number;
    color: string;
  };

  type EdgeAttributes = Record<string, unknown> & {
    label: string;
    size: number;
    color: string;
    type: string;
  };

  let {
    projection,
    selectedId,
    viewKey,
    trail,
    onSelect
  }: {
    projection: Projection;
    selectedId: string | null;
    viewKey: string;
    trail: readonly string[];
    onSelect: (node: GraphNode | null) => void;
  } = $props();

  let container: HTMLDivElement;
  let renderer: Sigma<NodeAttributes, EdgeAttributes> | null = null;
  let layout: FA2Layout<NodeAttributes, EdgeAttributes> | null = null;
  let layoutTimer: number | null = null;
  let resizeObserver: ResizeObserver | null = null;
  let renderedProjection: Projection | null = null;
  let renderedViewKey = '';
  let hoveredId = $state<string | null>(null);
  let keyboardIndex = $state(-1);
  let upstreamNodeIds = new Set<string>();
  let downstreamNodeIds = new Set<string>();
  let upstreamLinkIds = new Set<string>();
  let downstreamLinkIds = new Set<string>();
  let pathNodeIds = new Set<string>();
  let pathLinkIds = new Set<string>();

  const drawNodeHover: NodeHoverDrawingFunction<NodeAttributes, EdgeAttributes> = (
    context,
    data,
    settings
  ) => {
    if (!data.label) return;
    context.font = `${settings.labelWeight} ${settings.labelSize}px ${settings.labelFont}`;
    const padding = 6;
    const x = data.x + data.size + padding;
    const width = context.measureText(data.label).width + padding * 2;
    context.fillStyle = '#0b111b';
    context.strokeStyle = '#334155';
    context.beginPath();
    context.roundRect(x, data.y - settings.labelSize, width, settings.labelSize + padding, 4);
    context.fill();
    context.stroke();
    context.fillStyle = '#e2e8f0';
    context.fillText(data.label, x + padding, data.y + settings.labelSize / 3);
  };

  onMount(() => {
    rebuild(projection);
    resizeObserver = new ResizeObserver(() => renderer?.resize());
    resizeObserver.observe(container);
    return destroy;
  });

  $effect(() => {
    const nextProjection = projection;
    const nextViewKey = viewKey;
    if (renderer && (nextProjection !== renderedProjection || nextViewKey !== renderedViewKey)) {
      rebuild(nextProjection);
    }
  });

  $effect(() => {
    const active = hoveredId ?? selectedId;
    const nextTrail = trail;
    if (!renderer) return;
    const highlight = directHighlight(projection.links, active);
    upstreamNodeIds = highlight.upstreamNodeIds;
    downstreamNodeIds = highlight.downstreamNodeIds;
    upstreamLinkIds = highlight.upstreamLinkIds;
    downstreamLinkIds = highlight.downstreamLinkIds;
    pathNodeIds = new Set(nextTrail);
    const pathPairs = new Set(
      nextTrail.slice(1).map((node, index) => `${nextTrail[index]}|${node}`)
    );
    pathLinkIds = new Set(
      projection.links
        .filter((link) => {
          const source = endpointId(link.source);
          const target = endpointId(link.target);
          return pathPairs.has(`${source}|${target}`) || pathPairs.has(`${target}|${source}`);
        })
        .map((link) => link.id)
    );
    renderer.refresh();
  });

  function rebuild(next: Projection) {
    stopRenderer();
    const graph = new MultiDirectedGraph<NodeAttributes, EdgeAttributes>();
    const nodeById = new Map(next.nodes.map((node) => [node.id, node]));
    const count = Math.max(1, next.nodes.length);
    next.nodes.forEach((node, index) => {
      const angle = index * 2.399963229728653;
      const radius = Math.sqrt((index + 1) / count);
      graph.addNode(node.id, {
        label: node.label,
        x: Math.cos(angle) * radius,
        y: Math.sin(angle) * radius,
        size: Math.min(2.5, 0.6 + Math.log2(node.degree + 1) * 0.25),
        color: communityColor(node.community)
      });
    });
    for (const link of next.links) {
      const source = endpointId(link.source);
      const target = endpointId(link.target);
      if (!graph.hasNode(source) || !graph.hasNode(target)) continue;
      graph.addDirectedEdgeWithKey(link.id, source, target, {
        label: `${link.kind} · ${link.count} relationship${link.count === 1 ? '' : 's'}`,
        size: Math.min(0.35, 0.08 + Math.log2(link.count + 1) * 0.08),
        color: '#263244',
        type: 'arrow'
      });
    }

    const nextRenderer = new Sigma<NodeAttributes, EdgeAttributes>(graph, container, {
      allowInvalidContainer: true,
      defaultEdgeType: 'arrow',
      defaultDrawNodeHover: drawNodeHover,
      edgeProgramClasses: { arrow: createEdgeArrowProgram<NodeAttributes, EdgeAttributes>() },
      hideEdgesOnMove: true,
      hideLabelsOnMove: true,
      labelColor: { color: '#e2e8f0' },
      labelDensity: 0.08,
      labelGridCellSize: 120,
      labelRenderedSizeThreshold: 8,
      minCameraRatio: 0.02,
      maxCameraRatio: 8,
      nodeReducer: (node, data) => reduceNode(node, data),
      edgeReducer: (edge, data) => reduceEdge(edge, data),
      stagePadding: 48,
      zIndex: true
    });
    renderer = nextRenderer;
    nextRenderer.on('enterNode', ({ node }) => (hoveredId = node));
    nextRenderer.on('leaveNode', () => (hoveredId = null));
    nextRenderer.on('clickNode', ({ node }) => onSelect(nodeById.get(node) ?? null));
    nextRenderer.on('clickStage', () => onSelect(null));

    if (graph.order > 1 && graph.size > 0) {
      layout = new FA2Layout(graph, {
        settings: {
          ...forceAtlas2.inferSettings(graph),
          barnesHutOptimize: true,
          scalingRatio: 20
        }
      });
      layout.start();
      layoutTimer = window.setTimeout(() => {
        layout?.stop();
        layoutTimer = null;
      }, 20_000);
    }
    renderedProjection = next;
    renderedViewKey = viewKey;
  }

  function reduceNode(node: string, data: NodeAttributes): Partial<NodeAttributes> {
    const active = hoveredId ?? selectedId;
    if (node === active) return { ...data, color: '#ef4444', size: 4, forceLabel: true, zIndex: 4 };
    if (upstreamNodeIds.has(node)) return { ...data, color: '#2dd4bf', size: 2.5, forceLabel: true, zIndex: 3 };
    if (downstreamNodeIds.has(node)) return { ...data, color: '#f59e0b', size: 2.5, forceLabel: true, zIndex: 3 };
    if (pathNodeIds.has(node)) return { ...data, color: '#a855f7', size: 2.5, forceLabel: true, zIndex: 2 };
    return active ? { ...data, color: '#263244', size: Math.min(1, data.size), zIndex: 0 } : data;
  }

  function reduceEdge(edge: string, data: EdgeAttributes): Partial<EdgeAttributes> {
    if (pathLinkIds.has(edge)) return { ...data, color: '#a855f7', size: 1.2, zIndex: 4 };
    if (upstreamLinkIds.has(edge)) return { ...data, color: '#2dd4bf', size: 0.8, zIndex: 3 };
    if (downstreamLinkIds.has(edge)) return { ...data, color: '#f59e0b', size: 0.8, zIndex: 3 };
    return selectedId || hoveredId ? { ...data, hidden: true } : data;
  }

  function communityColor(community: string): string {
    const palette = ['#38bdf8', '#2dd4bf', '#a78bfa', '#f59e0b', '#fb7185', '#84cc16'];
    let hash = 0;
    for (const character of community) hash = (hash * 31 + character.charCodeAt(0)) | 0;
    return palette[Math.abs(hash) % palette.length];
  }

  function handleKeydown(event: KeyboardEvent) {
    if (!projection.nodes.length) return;
    if (['ArrowRight', 'ArrowDown', 'ArrowLeft', 'ArrowUp'].includes(event.key)) {
      event.preventDefault();
      const step = event.key === 'ArrowRight' || event.key === 'ArrowDown' ? 1 : -1;
      keyboardIndex = (keyboardIndex + step + projection.nodes.length) % projection.nodes.length;
      hoveredId = projection.nodes[keyboardIndex].id;
    } else if (event.key === 'Enter' && keyboardIndex >= 0) {
      onSelect(projection.nodes[keyboardIndex]);
    } else if (event.key === 'Escape') {
      keyboardIndex = -1;
      hoveredId = null;
      onSelect(null);
    }
  }

  function stopRenderer() {
    if (layoutTimer !== null) window.clearTimeout(layoutTimer);
    layoutTimer = null;
    layout?.kill();
    layout = null;
    renderer?.kill();
    renderer = null;
  }

  function destroy() {
    resizeObserver?.disconnect();
    resizeObserver = null;
    stopRenderer();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex (WebGL composite widget exposes its keyboard model here) -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions (WebGL composite widget exposes its keyboard model here) -->
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
  .graph { height: 100%; min-height: 26rem; overflow: hidden; width: 100%; }
  .graph:focus-visible { outline: 2px solid var(--ring); outline-offset: -2px; }
  .sr-only { height: 1px; margin: -1px; overflow: hidden; position: absolute; width: 1px; clip: rect(0, 0, 0, 0); }
</style>
