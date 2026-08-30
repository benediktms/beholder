export const RELATION_KINDS = [
  'binds_contract',
  'calls',
  'calls_graphql',
  'calls_rpc',
  'consumed_by',
  'defines',
  'field_of',
  'implements',
  'implemented_by',
  'imports',
  'publishes',
  'requires',
  'request_type',
  'resolved_by',
  'selects',
  'response_type',
  'uses'
] as const;

export const ORIGINS = ['source', 'generated', 'external_dependency'] as const;
export const MAX_VISIBLE_NODES = 10_000;
export const MAX_VISIBLE_LINKS = 25_000;
export const MAX_ANIMATED_LINKS = 250;

export type RelationKind = (typeof RELATION_KINDS)[number];
export type EntityOrigin = (typeof ORIGINS)[number];
export type ZoomBand = 'repository' | 'module' | 'file' | 'entity';
export type PathDirection = 'upstream' | 'downstream' | 'both' | 'neutral';

export interface WorkspaceSummary {
  name: string;
  repositories: Array<{ identity: string; displayName: string }>;
}

export interface EntityRef {
  id: string;
  kind: string;
  name: string;
  repository: string | null;
  origin: EntityOrigin;
  test: boolean;
  metadata: Record<string, unknown> | null;
}

export interface EvidenceRef {
  source: string;
  repository: string | null;
  path: string | null;
  line: number | null;
  detail: string | null;
}

export interface SemanticEdge {
  id: string;
  from: string;
  to: string;
  kind: RelationKind;
  confidence: number;
  evidence: EvidenceRef[];
}

export interface GraphSnapshot {
  schema: string;
  workspace: WorkspaceSummary;
  metadata: {
    revision: number;
    view: string;
    freshness: {
      stale: boolean;
      indexing: boolean;
      dirty_repositories: string[];
      enriching_repositories: string[];
    };
    analysis: { completeness: 'complete' | 'incomplete'; diagnostics: unknown[] };
  };
  traversal: { max_hops: number; truncated: boolean };
  nodes: EntityRef[];
  edges: SemanticEdge[];
}

export interface GraphNode {
  id: string;
  label: string;
  level: ZoomBand;
  kind: string;
  rawEntityIds: string[];
  count: number;
  internalEdges: number;
  direction: PathDirection;
  hops: number;
  selected: boolean;
  x?: number;
  y?: number;
}

export interface GraphLink {
  id: string;
  source: string | GraphNode;
  target: string | GraphNode;
  kind: RelationKind;
  count: number;
  confidence: number;
  evidenceCount: number;
  rawEdgeIds: string[];
  direction: PathDirection;
  hops: number;
  selected: boolean;
  animated: boolean;
}

export interface Projection {
  nodes: GraphNode[];
  links: GraphLink[];
  rawNodeCount: number;
  rawLinkCount: number;
  omittedNodes: number;
  omittedLinks: number;
  omittedAnimations: number;
  truncated: boolean;
}

export interface ProjectionOptions {
  band: ZoomBand;
  repository: string | null;
  relationKinds: readonly RelationKind[];
  includeTests: boolean;
  origins: readonly EntityOrigin[];
  rootId: string | null;
  selectedId: string | null;
}

export interface ProjectionLimits {
  nodes: number;
  links: number;
  animatedLinks: number;
}

const DEFAULT_LIMITS: ProjectionLimits = {
  nodes: MAX_VISIBLE_NODES,
  links: MAX_VISIBLE_LINKS,
  animatedLinks: MAX_ANIMATED_LINKS
};

const STRUCTURAL_RELATIONS = new Set<RelationKind>([
  'defines',
  'field_of',
  'request_type',
  'response_type'
]);

export function zoomBand(scale: number): ZoomBand {
  if (scale < 0.75) return 'repository';
  if (scale < 1.5) return 'module';
  if (scale < 3) return 'file';
  return 'entity';
}

export function endpointId(endpoint: string | GraphNode): string {
  return typeof endpoint === 'string' ? endpoint : endpoint.id;
}

export function projectGraph(
  snapshot: GraphSnapshot,
  options: ProjectionOptions,
  limits: ProjectionLimits = DEFAULT_LIMITS
): Projection {
  const nodeById = new Map(snapshot.nodes.map((node) => [node.id, node]));
  const root = options.rootId ? nodeById.get(options.rootId) : undefined;
  const allowedNodes = snapshot.nodes.filter(
    (node) =>
      node.id === root?.id ||
      ((options.repository === null || node.repository === options.repository) &&
        (options.includeTests || !node.test) &&
        options.origins.includes(node.origin))
  );
  const allowedIds = new Set(allowedNodes.map((node) => node.id));
  const allowedKinds = new Set(options.relationKinds);
  const filteredEdges = snapshot.edges.filter(
    (edge) => allowedKinds.has(edge.kind) && allowedIds.has(edge.from) && allowedIds.has(edge.to)
  );

  const reachability = root
    ? reachable(
        root.id,
        filteredEdges.filter((edge) => !STRUCTURAL_RELATIONS.has(edge.kind))
      )
    : {
        nodeIds: allowedIds,
        upstreamNodes: new Map<string, number>(),
        downstreamNodes: new Map<string, number>(),
        upstreamEdges: new Set<string>(),
        downstreamEdges: new Set<string>()
      };
  const rawNodes = allowedNodes.filter((node) => reachability.nodeIds.has(node.id));
  const rawIds = new Set(rawNodes.map((node) => node.id));
  const rawEdges = filteredEdges.filter(
    (edge) => rawIds.has(edge.from) && rawIds.has(edge.to)
  );
  const fileByEntity = containmentFiles(rawNodes, rawEdges);
  const displayNames = new Map(
    snapshot.workspace.repositories.map((repository) => [
      repository.identity,
      repository.displayName
    ])
  );
  const visibleId = new Map<string, string>();
  const groups = new Map<string, GraphNode>();

  for (const entity of rawNodes) {
    const group = groupFor(entity, options.band, fileByEntity, displayNames);
    visibleId.set(entity.id, group.id);
    const direction = root
      ? directionFor(
          reachability.upstreamNodes.has(entity.id),
          reachability.downstreamNodes.has(entity.id),
          entity.id === root.id
        )
      : 'neutral';
    const hops = root
      ? Math.min(
          reachability.upstreamNodes.get(entity.id) ?? Number.POSITIVE_INFINITY,
          reachability.downstreamNodes.get(entity.id) ?? Number.POSITIVE_INFINITY
        )
      : 0;
    const existing = groups.get(group.id);
    if (existing) {
      existing.rawEntityIds.push(entity.id);
      existing.count += 1;
      existing.direction = mergeDirection(existing.direction, direction);
      existing.hops = Math.min(existing.hops, hops);
      existing.selected ||= entity.id === root?.id;
    } else {
      groups.set(group.id, {
        ...group,
        rawEntityIds: [entity.id],
        count: 1,
        internalEdges: 0,
        direction,
        hops,
        selected: group.id === options.selectedId || entity.id === root?.id
      });
    }
  }

  const links = new Map<string, GraphLink>();
  for (const edge of rawEdges) {
    const source = visibleId.get(edge.from);
    const target = visibleId.get(edge.to);
    if (!source || !target) continue;
    if (source === target) {
      const node = groups.get(source);
      if (node) node.internalEdges += 1;
      continue;
    }
    const direction = root
      ? directionFor(
          reachability.upstreamEdges.has(edge.id),
          reachability.downstreamEdges.has(edge.id),
          false
        )
      : 'neutral';
    const id = `${source}|${edge.kind}|${target}`;
    const existing = links.get(id);
    if (existing) {
      existing.count += 1;
      existing.confidence = Math.max(existing.confidence, edge.confidence);
      existing.evidenceCount += edge.evidence.length;
      existing.rawEdgeIds.push(edge.id);
      existing.direction = mergeDirection(existing.direction, direction);
    } else {
      links.set(id, {
        id,
        source,
        target,
        kind: edge.kind,
        count: 1,
        confidence: edge.confidence,
        evidenceCount: edge.evidence.length,
        rawEdgeIds: [edge.id],
        direction,
        hops: Math.min(groups.get(source)?.hops ?? 0, groups.get(target)?.hops ?? 0),
        selected: false,
        animated: false
      });
    }
  }

  for (const link of links.values()) {
    link.rawEdgeIds.sort();
    link.selected =
      Boolean(groups.get(endpointId(link.source))?.selected) ||
      Boolean(groups.get(endpointId(link.target))?.selected);
  }
  for (const node of groups.values()) node.rawEntityIds.sort();

  const allNodes = [...groups.values()].sort(compareByHopThenId);
  const keptNodes = allNodes.slice(0, limits.nodes);
  const keptIds = new Set(keptNodes.map((node) => node.id));
  const allLinks = [...links.values()].sort(compareByHopThenId);
  const keptLinks = allLinks
    .filter(
      (link) => keptIds.has(endpointId(link.source)) && keptIds.has(endpointId(link.target))
    )
    .slice(0, limits.links);
  const emphasized = keptLinks.filter(
    (link) => link.selected || link.direction !== 'neutral'
  );
  emphasized.slice(0, limits.animatedLinks).forEach((link) => (link.animated = true));

  const omittedNodes = allNodes.length - keptNodes.length;
  const omittedLinks = allLinks.length - keptLinks.length;
  const omittedAnimations = Math.max(0, emphasized.length - limits.animatedLinks);
  return {
    nodes: keptNodes,
    links: keptLinks,
    rawNodeCount: rawNodes.length,
    rawLinkCount: rawEdges.length,
    omittedNodes,
    omittedLinks,
    omittedAnimations,
    truncated:
      snapshot.traversal.truncated ||
      omittedNodes > 0 ||
      omittedLinks > 0 ||
      omittedAnimations > 0
  };
}

function reachable(rootId: string, edges: SemanticEdge[]) {
  const outgoing = adjacency(edges, 'from');
  const incoming = adjacency(edges, 'to');
  const downstream = walk(rootId, outgoing, (edge) => edge.to);
  const upstream = walk(rootId, incoming, (edge) => edge.from);
  return {
    nodeIds: new Set([...downstream.nodes.keys(), ...upstream.nodes.keys()]),
    upstreamNodes: upstream.nodes,
    downstreamNodes: downstream.nodes,
    upstreamEdges: upstream.edges,
    downstreamEdges: downstream.edges
  };
}

function adjacency(edges: SemanticEdge[], endpoint: 'from' | 'to') {
  const adjacent = new Map<string, SemanticEdge[]>();
  for (const edge of edges) {
    const values = adjacent.get(edge[endpoint]) ?? [];
    values.push(edge);
    adjacent.set(edge[endpoint], values);
  }
  for (const values of adjacent.values()) values.sort((a, b) => a.id.localeCompare(b.id));
  return adjacent;
}

function walk(
  rootId: string,
  adjacent: Map<string, SemanticEdge[]>,
  next: (edge: SemanticEdge) => string
) {
  const nodes = new Map([[rootId, 0]]);
  const traversedEdges = new Set<string>();
  const queue = [rootId];
  for (let index = 0; index < queue.length; index += 1) {
    const id = queue[index];
    const hops = nodes.get(id) ?? 0;
    for (const edge of adjacent.get(id) ?? []) {
      traversedEdges.add(edge.id);
      const neighbor = next(edge);
      if (!nodes.has(neighbor)) {
        nodes.set(neighbor, hops + 1);
        queue.push(neighbor);
      }
    }
  }
  return { nodes, edges: traversedEdges };
}

function containmentFiles(nodes: EntityRef[], edges: SemanticEdge[]): Map<string, string> {
  const nodeIds = new Set(nodes.map((node) => node.id));
  const nodeById = new Map(nodes.map((node) => [node.id, node]));
  const files = new Map<string, string>();
  const structural = edges
    .filter((edge) => edge.kind === 'defines' && nodeIds.has(edge.to))
    .sort((a, b) => a.id.localeCompare(b.id));
  for (const edge of structural) {
    const target = nodeById.get(edge.to);
    const path = evidencePath(edge, target?.repository ?? null);
    if (path && !files.has(edge.to)) files.set(edge.to, path);
    const ownerPath = evidencePath(edge, nodeById.get(edge.from)?.repository ?? null);
    if (ownerPath && !files.has(edge.from)) files.set(edge.from, ownerPath);
  }
  for (const node of nodes) {
    if (files.has(node.id)) continue;
    const path = edges
      .filter((edge) => edge.from === node.id)
      .sort((a, b) => a.id.localeCompare(b.id))
      .map((edge) => evidencePath(edge, node.repository))
      .find(Boolean);
    if (path) files.set(node.id, path);
  }
  return files;
}

function evidencePath(edge: SemanticEdge, repository: string | null): string | undefined {
  return edge.evidence
    .filter((evidence) => evidence.repository === repository && evidence.path)
    .map((evidence) => evidence.path as string)
    .sort()[0];
}

function groupFor(
  entity: EntityRef,
  band: ZoomBand,
  files: Map<string, string>,
  displayNames: Map<string, string>
): Omit<GraphNode, 'rawEntityIds' | 'count' | 'internalEdges' | 'direction' | 'hops' | 'selected'> {
  const repository = entity.repository ?? 'external/contracts';
  if (band === 'repository') {
    return {
      id: `ui:repository:${repository}`,
      label: displayNames.get(repository) ?? repository,
      level: band,
      kind: 'repository'
    };
  }
  const file = files.get(entity.id) ?? 'no-file';
  if (band === 'module') {
    const separator = file.lastIndexOf('/');
    const module = separator < 0 ? '(root)' : file.slice(0, separator);
    return {
      id: `ui:module:${repository}:${module}`,
      label: module,
      level: band,
      kind: 'module'
    };
  }
  if (band === 'file') {
    return {
      id: `ui:file:${repository}:${file}`,
      label: file,
      level: band,
      kind: 'file'
    };
  }
  return { id: entity.id, label: entity.name, level: band, kind: entity.kind };
}

function directionFor(upstream: boolean, downstream: boolean, root: boolean): PathDirection {
  if (root || (upstream && downstream)) return 'both';
  if (upstream) return 'upstream';
  if (downstream) return 'downstream';
  return 'neutral';
}

function mergeDirection(left: PathDirection, right: PathDirection): PathDirection {
  if (left === 'neutral') return right;
  if (right === 'neutral' || left === right) return left;
  return 'both';
}

function compareByHopThenId<T extends { hops: number; id: string }>(left: T, right: T): number {
  return left.hops - right.hops || left.id.localeCompare(right.id);
}
