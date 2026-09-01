<script lang="ts">
  import ForceGraph from 'force-graph';
  import { onMount } from 'svelte';
  import {
    MAX_ANIMATED_LINKS,
    directHighlight,
    labelOpacity,
    nodeValue,
    type GraphLink,
    type GraphNode,
    type Projection
  } from './graph';

  let {
    projection,
    selectedId,
    onSelect
  }: {
    projection: Projection;
    selectedId: string | null;
    onSelect: (node: GraphNode | null) => void;
  } = $props();

  let container: HTMLDivElement;
  let graph: ForceGraph<GraphNode, GraphLink> | null = null;
  let initialFitPending = true;
  let hoveredId = $state<string | null>(null);
  let keyboardIndex = $state(-1);
  let activeId: string | null = null;
  let highlightedUpstreamNodes = new Set<string>();
  let highlightedDownstreamNodes = new Set<string>();
  let highlightedUpstreamLinks = new Set<string>();
  let highlightedDownstreamLinks = new Set<string>();
  const selectedStrength = new Map<string, number>();
  const upstreamNodeStrength = new Map<string, number>();
  const downstreamNodeStrength = new Map<string, number>();
  const upstreamLinkStrength = new Map<string, number>();
  const downstreamLinkStrength = new Map<string, number>();
  let transitionFrame: number | null = null;

  onMount(() => {
    graph = new ForceGraph<GraphNode, GraphLink>(container)
      .backgroundColor('#080b12')
      .nodeId('id')
      .nodeRelSize(4)
      .nodeVal((node) => nodeValue(node.degree))
      .nodeLabel((node) => node.label)
      .nodeColor('#3388bb')
      .nodePointerAreaPaint(drawNodePointerArea)
      .nodeCanvasObjectMode(() => 'after')
      .nodeCanvasObject(drawNodeOverlay)
      .onRenderFramePre(drawHighlightHalos)
      .linkLabel((link) => `${link.kind} · ${link.count} relationship${link.count === 1 ? '' : 's'}`)
      .linkColor(linkColor)
      .linkWidth((link) => 0.7 + linkHighlight(link.id) * 3.3)
      .linkDirectionalArrowLength((link) => 2.5 + linkHighlight(link.id) * 2.5)
      .linkDirectionalArrowRelPos(0.88)
      .linkDirectionalArrowColor(linkColor)
      .linkDirectionalParticles((link) => (linkHighlight(link.id) > 0.01 ? 4 : 0))
      .linkDirectionalParticleColor(() => '#e2e8f0')
      .linkDirectionalParticleSpeed(0.006)
      .linkDirectionalParticleWidth((link) => linkHighlight(link.id) * 3.5)
      .onNodeHover((node) => (hoveredId = node?.id ?? null))
      .onNodeClick(onSelect)
      .onBackgroundClick(() => onSelect(null))
      .onEngineStop(() => {
        if (!initialFitPending || !graph?.graphData().nodes.length) return;
        initialFitPending = false;
        graph.zoomToFit(400, 36);
      })
      .autoPauseRedraw(false)
      .cooldownTicks(140);

    const resize = new ResizeObserver(([entry]) => {
      graph?.width(entry.contentRect.width).height(entry.contentRect.height);
    });
    resize.observe(container);
    updateData(projection);
    return () => {
      resize.disconnect();
      if (transitionFrame !== null) cancelAnimationFrame(transitionFrame);
      graph?._destructor();
      graph = null;
    };
  });

  $effect(() => {
    const next = projection;
    if (graph) updateData(next);
  });

  $effect(() => {
    const nextActiveId = hoveredId ?? selectedId;
    if (graph) setHighlight(nextActiveId);
  });

  function updateData(next: Projection) {
    if (!graph) return;
    const previousById = new Map(graph.graphData().nodes.map((node) => [node.id, node]));
    for (const node of next.nodes) {
      const previous = previousById.get(node.id);
      if (previous?.x !== undefined && previous.y !== undefined) {
        node.x = previous.x;
        node.y = previous.y;
      }
    }
    hoveredId = null;
    keyboardIndex = -1;
    graph.graphData({ nodes: next.nodes, links: next.links }).d3ReheatSimulation();
  }

  function setHighlight(id: string | null) {
    activeId = id;
    const next = directHighlight(projection.links, id);
    highlightedUpstreamNodes = next.upstreamNodeIds;
    highlightedDownstreamNodes = next.downstreamNodeIds;
    highlightedUpstreamLinks = new Set(
      [...next.upstreamLinkIds].slice(0, MAX_ANIMATED_LINKS)
    );
    highlightedDownstreamLinks = new Set(
      [...next.downstreamLinkIds].slice(
        0,
        MAX_ANIMATED_LINKS - highlightedUpstreamLinks.size
      )
    );
    startTransition();
  }

  function startTransition() {
    if (transitionFrame !== null) return;
    transitionFrame = requestAnimationFrame(animateHighlight);
  }

  function animateHighlight() {
    let unsettled = false;
    for (const node of projection.nodes) {
      unsettled = tween(selectedStrength, node.id, node.id === activeId ? 1 : 0) || unsettled;
      unsettled = tween(
        upstreamNodeStrength,
        node.id,
        highlightedUpstreamNodes.has(node.id) ? 1 : 0
      ) || unsettled;
      unsettled = tween(
        downstreamNodeStrength,
        node.id,
        highlightedDownstreamNodes.has(node.id) ? 1 : 0
      ) || unsettled;
    }
    for (const link of projection.links) {
      unsettled = tween(
        upstreamLinkStrength,
        link.id,
        highlightedUpstreamLinks.has(link.id) ? 1 : 0
      ) || unsettled;
      unsettled = tween(
        downstreamLinkStrength,
        link.id,
        highlightedDownstreamLinks.has(link.id) ? 1 : 0
      ) || unsettled;
    }
    transitionFrame = unsettled ? requestAnimationFrame(animateHighlight) : null;
  }

  function tween(values: Map<string, number>, id: string, target: number): boolean {
    const current = strength(values, id);
    if (Math.abs(target - current) < 0.01) {
      values.set(id, target);
      return false;
    }
    values.set(id, current + (target - current) * 0.16);
    return true;
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

  function strength(values: Map<string, number>, id: string): number {
    return values.get(id) ?? 0;
  }

  function linkHighlight(id: string): number {
    return Math.min(
      1,
      strength(upstreamLinkStrength, id) + strength(downstreamLinkStrength, id)
    );
  }

  function linkColor(link: GraphLink): string {
    const upstream = strength(upstreamLinkStrength, link.id);
    const downstream = strength(downstreamLinkStrength, link.id);
    const base = Math.max(0, 1 - upstream - downstream);
    const weight = base + upstream + downstream;
    const red = Math.round((203 * base + 45 * upstream + 245 * downstream) / weight);
    const green = Math.round((213 * base + 212 * upstream + 158 * downstream) / weight);
    const blue = Math.round((225 * base + 191 * upstream + 11 * downstream) / weight);
    return `rgba(${red}, ${green}, ${blue}, ${0.28 + linkHighlight(link.id) * 0.62})`;
  }

  function drawHighlightHalos(context: CanvasRenderingContext2D, scale: number) {
    for (const node of graph?.graphData().nodes ?? []) {
      if (node.x === undefined || node.y === undefined) continue;
      const nodeRadius = Math.sqrt(nodeValue(node.degree)) * 4;
      const upstream = strength(upstreamNodeStrength, node.id);
      const downstream = strength(downstreamNodeStrength, node.id);
      const selected = strength(selectedStrength, node.id);
      if (upstream > 0) {
        context.beginPath();
        context.arc(node.x, node.y, nodeRadius + 6 / scale, 0, 2 * Math.PI);
        context.fillStyle = `rgba(45, 212, 191, ${upstream})`;
        context.fill();
      }
      if (downstream > 0) {
        context.beginPath();
        context.arc(node.x, node.y, nodeRadius + 4 / scale, 0, 2 * Math.PI);
        context.fillStyle = `rgba(245, 158, 11, ${downstream})`;
        context.fill();
      }
      if (selected > 0) {
        context.beginPath();
        context.arc(node.x, node.y, nodeRadius + 4 / scale, 0, 2 * Math.PI);
        context.fillStyle = `rgba(239, 68, 68, ${selected})`;
        context.fill();
      }
    }
  }

  function drawNodeOverlay(
    node: GraphNode,
    context: CanvasRenderingContext2D,
    scale: number
  ) {
    if (node.x === undefined || node.y === undefined) return;
    const radius = Math.sqrt(nodeValue(node.degree)) * 4;
    const opacity = labelOpacity(
      scale,
      Math.max(
        strength(selectedStrength, node.id),
        strength(upstreamNodeStrength, node.id),
        strength(downstreamNodeStrength, node.id)
      )
    );
    if (opacity < 0.01) return;
    const fontSize = 11 / scale;
    context.font = `600 ${fontSize}px Inter, ui-sans-serif, system-ui`;
    context.textAlign = 'center';
    context.textBaseline = 'top';
    context.fillStyle = `rgba(226, 232, 240, ${opacity})`;
    context.fillText(node.label, node.x, node.y + radius + 4 / scale);
  }

  function drawNodePointerArea(
    node: GraphNode,
    color: string,
    context: CanvasRenderingContext2D,
    scale: number
  ) {
    if (node.x === undefined || node.y === undefined) return;
    context.beginPath();
    context.arc(
      node.x,
      node.y,
      Math.max(Math.sqrt(nodeValue(node.degree)) * 4, 8 / scale),
      0,
      2 * Math.PI
    );
    context.fillStyle = color;
    context.fill();
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
