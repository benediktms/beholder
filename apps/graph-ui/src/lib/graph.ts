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
export const EXTERNAL_REPOSITORY = 'external/contracts';

export type RelationKind = (typeof RELATION_KINDS)[number];
export type EntityOrigin = (typeof ORIGINS)[number];

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
  kind: string;
  degree: number;
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
}

export interface Projection {
  nodes: GraphNode[];
  links: GraphLink[];
  rawNodeCount: number;
  rawLinkCount: number;
  omittedNodes: number;
  omittedLinks: number;
  truncated: boolean;
}

export interface ProjectionOptions {
  repositories: readonly string[];
  relationKinds: readonly RelationKind[];
  includeTests: boolean;
  origins: readonly EntityOrigin[];
}

export interface ProjectionLimits {
  nodes: number;
  links: number;
}

const DEFAULT_LIMITS: ProjectionLimits = {
  nodes: MAX_VISIBLE_NODES,
  links: MAX_VISIBLE_LINKS
};

export function endpointId(endpoint: string | GraphNode): string {
  return typeof endpoint === 'string' ? endpoint : endpoint.id;
}

export function projectGraph(
  snapshot: GraphSnapshot,
  options: ProjectionOptions,
  limits: ProjectionLimits = DEFAULT_LIMITS
): Projection {
  const selectedRepositories = new Set(options.repositories);
  const allowedNodes = snapshot.nodes.filter(
    (node) =>
      (selectedRepositories.size === 0 ||
        selectedRepositories.has(node.repository ?? EXTERNAL_REPOSITORY)) &&
      (options.includeTests || !node.test) &&
      options.origins.includes(node.origin)
  );
  const allowedIds = new Set(allowedNodes.map((node) => node.id));
  const allowedKinds = new Set(options.relationKinds);
  const filteredEdges = snapshot.edges.filter(
    (edge) => allowedKinds.has(edge.kind) && allowedIds.has(edge.from) && allowedIds.has(edge.to)
  );

  const groups = new Map<string, GraphNode>();

  for (const entity of allowedNodes) {
    groups.set(entity.id, {
      id: entity.id,
      label: entity.name,
      kind: entity.kind,
      degree: 0
    });
  }

  const links = new Map<string, GraphLink>();
  for (const edge of filteredEdges) {
    const source = edge.from;
    const target = edge.to;
    if (source === target) continue;
    const id = `${source}|${edge.kind}|${target}`;
    const existing = links.get(id);
    if (existing) {
      existing.count += 1;
      existing.confidence = Math.max(existing.confidence, edge.confidence);
      existing.evidenceCount += edge.evidence.length;
      existing.rawEdgeIds.push(edge.id);
    } else {
      links.set(id, {
        id,
        source,
        target,
        kind: edge.kind,
        count: 1,
        confidence: edge.confidence,
        evidenceCount: edge.evidence.length,
        rawEdgeIds: [edge.id]
      });
    }
  }

  for (const link of links.values()) {
    link.rawEdgeIds.sort();
  }
  const allNodes = [...groups.values()].sort(compareById);
  const keptNodes = allNodes.slice(0, limits.nodes);
  const keptIds = new Set(keptNodes.map((node) => node.id));
  const allLinks = [...links.values()].sort(compareById);
  const keptLinks = allLinks
    .filter(
      (link) => keptIds.has(endpointId(link.source)) && keptIds.has(endpointId(link.target))
    )
    .slice(0, limits.links);
  for (const link of keptLinks) {
    const source = groups.get(endpointId(link.source));
    const target = groups.get(endpointId(link.target));
    if (source) source.degree += 1;
    if (target) target.degree += 1;
  }
  const omittedNodes = allNodes.length - keptNodes.length;
  const omittedLinks = allLinks.length - keptLinks.length;
  return {
    nodes: keptNodes,
    links: keptLinks,
    rawNodeCount: allowedNodes.length,
    rawLinkCount: filteredEdges.length,
    omittedNodes,
    omittedLinks,
    truncated:
      snapshot.traversal.truncated ||
      omittedNodes > 0 ||
      omittedLinks > 0
  };
}

export function directHighlight(links: GraphLink[], selectedId: string | null) {
  const upstreamNodeIds = new Set<string>();
  const downstreamNodeIds = new Set<string>();
  const upstreamLinkIds = new Set<string>();
  const downstreamLinkIds = new Set<string>();
  if (!selectedId) {
    return { upstreamNodeIds, downstreamNodeIds, upstreamLinkIds, downstreamLinkIds };
  }
  for (const link of links) {
    const source = endpointId(link.source);
    const target = endpointId(link.target);
    if (target === selectedId) {
      upstreamNodeIds.add(source);
      upstreamLinkIds.add(link.id);
    }
    if (source === selectedId) {
      downstreamNodeIds.add(target);
      downstreamLinkIds.add(link.id);
    }
  }
  return { upstreamNodeIds, downstreamNodeIds, upstreamLinkIds, downstreamLinkIds };
}

export function nodeValue(degree: number): number {
  return Math.max(0.25, degree - 0.75);
}

export function labelOpacity(scale: number, highlight: number): number {
  const zoom = scale <= 1.1 ? 0 : scale >= 1.5 ? 1 : (scale - 1.1) / 0.4;
  return Math.max(zoom, highlight);
}

function compareById<T extends { id: string }>(left: T, right: T): number {
  return left.id.localeCompare(right.id);
}
