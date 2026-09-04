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
  metadata: QueryMetadata;
  nodes: EntityRef[];
  edges: SemanticEdge[];
}

export interface QueryMetadata {
  revision: number;
  view: string;
  freshness: {
    stale: boolean;
    indexing: boolean;
    dirty_repositories: string[];
    enriching_repositories: string[];
  };
  analysis?: {
    completeness: 'complete' | 'incomplete';
    diagnostics: Array<{
      code: string;
      severity: 'known_limitation' | 'warning';
      repository: string;
      path: string;
      line: number | null;
      detail: string | null;
    }>;
  };
}

export interface GraphNode {
  id: string;
  label: string;
  kind: string;
  community: string;
  communityLabel: string;
  degree: number;
  x?: number;
  y?: number;
  vx?: number;
  vy?: number;
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
  nodes: Number.POSITIVE_INFINITY,
  links: Number.POSITIVE_INFINITY
};

export function endpointId(endpoint: string | GraphNode): string {
  return typeof endpoint === 'string' ? endpoint : endpoint.id;
}

export function findEntity(nodes: readonly EntityRef[], search: string): EntityRef | undefined {
  const exact = search.trim();
  if (!exact) return undefined;
  const folded = exact.toLowerCase();
  return nodes.find((node) => node.id === exact)
    ?? nodes.find((node) => node.id.toLowerCase() === folded || node.name.toLowerCase() === folded)
    ?? nodes.find((node) => node.id.toLowerCase().includes(folded) || node.name.toLowerCase().includes(folded));
}

export function extendTrail(input: {
  trail: readonly string[];
  next: string;
  connected: boolean;
  limit?: number;
}): string[] {
  const existingIndex = input.trail.indexOf(input.next);
  return (existingIndex >= 0
    ? input.trail.slice(0, existingIndex + 1)
    : input.connected
      ? [...input.trail, input.next]
      : [input.next]
  ).slice(-(input.limit ?? 24));
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
    const community = entityCommunity(entity);
    groups.set(entity.id, {
      id: entity.id,
      label: entity.name,
      kind: entity.kind,
      community: community.id,
      communityLabel: community.label,
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
    truncated: omittedNodes > 0 || omittedLinks > 0
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

export function entityCommunity(entity: EntityRef): { id: string; label: string } {
  if (!entity.repository) {
    return { id: EXTERNAL_REPOSITORY, label: 'External contracts' };
  }

  const prefix = `repo://${entity.repository}/`;
  if (!entity.id.startsWith(prefix)) {
    return {
      id: entity.repository,
      label: entity.repository.split('/').at(-1) ?? entity.repository
    };
  }

  const [language = 'source', ...path] = entity.id.slice(prefix.length).split('/');
  const groupedRoots = new Set(['apps', 'crates', 'packages', 'plugins', 'services']);
  const symbolRoot = /^[A-Z#:]/.test(path[0] ?? '') || (path[0]?.includes('.') ?? false);
  const area = groupedRoots.has(path[0]) && path[1]
    ? `${path[0]}/${path[1]}`
    : symbolRoot
      ? language
      : path[0] || language;
  return {
    id: `${entity.repository}/${language}/${area}`,
    label: area
  };
}

function compareById<T extends { id: string }>(left: T, right: T): number {
  return left.id.localeCompare(right.id);
}
