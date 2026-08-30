import assert from 'node:assert/strict';
import test from 'node:test';
import {
  EXTERNAL_REPOSITORY,
  ORIGINS,
  projectGraph,
  type EntityRef,
  type GraphSnapshot,
  type ProjectionOptions,
  type SemanticEdge
} from './graph.ts';

const nodes: EntityRef[] = [
  entity('a', 'repo-a'),
  entity('b', 'repo-b'),
  entity('c', 'repo-c')
];
const edges: SemanticEdge[] = [
  edge('e1', 'a', 'b'),
  edge('e2', 'a', 'b'),
  edge('e3', 'b', 'c')
];
const snapshot: GraphSnapshot = {
  schema: 'test',
  workspace: {
    name: 'test',
    repositories: nodes.map((node) => ({
      identity: node.repository as string,
      displayName: node.repository as string
    }))
  },
  metadata: {
    revision: 1,
    view: 'test',
    freshness: {
      stale: false,
      indexing: false,
      dirty_repositories: [],
      enriching_repositories: []
    },
    analysis: { completeness: 'complete', diagnostics: [] }
  },
  traversal: { max_hops: 8, truncated: false },
  nodes,
  edges
};

const options: ProjectionOptions = {
  band: 'repository',
  repository: null,
  relationKinds: ['calls'],
  includeTests: true,
  origins: ORIGINS,
  rootId: null,
  selectedId: null
};

test('bundles equivalent visible edges without losing raw topology', () => {
  const graph = projectGraph(snapshot, options);
  assert.equal(graph.nodes.length, 3);
  assert.equal(graph.links.length, 2);
  assert.equal(graph.links.find((link) => link.rawEdgeIds.includes('e1'))?.count, 2);
  assert.equal(graph.rawLinkCount, 3);
});

test('re-rooting keeps every upstream and downstream reachable entity', () => {
  const graph = projectGraph(snapshot, { ...options, band: 'entity', rootId: 'b' });
  assert.deepEqual(
    graph.nodes.map((node) => [node.id, node.direction]),
    [
      ['b', 'both'],
      ['a', 'upstream'],
      ['c', 'downstream']
    ]
  );
  assert.equal(graph.links.find((link) => link.rawEdgeIds.includes('e1'))?.direction, 'upstream');
  assert.equal(graph.links.find((link) => link.rawEdgeIds.includes('e3'))?.direction, 'downstream');
});

test('upstream gRPC traversal reaches both clients and server implementations', () => {
  const grpcNodes = [
    entity('contract', 'contracts'),
    entity('grpc://example.Service/Call', 'contracts'),
    entity('client', 'client'),
    entity('server', 'server')
  ];
  const graph = projectGraph(
    {
      ...snapshot,
      nodes: grpcNodes,
      edges: [
        { ...edge('binds', 'grpc://example.Service/Call', 'contract'), kind: 'binds_contract' },
        { ...edge('calls', 'client', 'grpc://example.Service/Call'), kind: 'calls_rpc' },
        { ...edge('server', 'grpc://example.Service/Call', 'server'), kind: 'implemented_by' }
      ]
    },
    {
      ...options,
      band: 'entity',
      relationKinds: ['binds_contract', 'calls_rpc', 'implemented_by'],
      rootId: 'contract'
    }
  );
  assert.deepEqual(
    Object.fromEntries(graph.nodes.map((node) => [node.id, node.direction])),
    {
      contract: 'both',
      'grpc://example.Service/Call': 'upstream',
      client: 'upstream',
      server: 'upstream'
    }
  );
});

test('structural containment reconnects projections without widening dependency reachability', () => {
  const parent = entity('parent', 'repo-b');
  const defines = {
    ...edge('e4', 'parent', 'b'),
    kind: 'defines' as const,
    evidence: [
      { source: 'test', repository: 'repo-b', path: 'src/b.ts', line: 1, detail: null }
    ]
  };
  const graph = projectGraph(
    {
      ...snapshot,
      nodes: [...snapshot.nodes, parent],
      edges: [...snapshot.edges, defines]
    },
    {
      ...options,
      band: 'file',
      relationKinds: ['calls'],
      rootId: 'b'
    }
  );
  assert.equal(graph.nodes.some((node) => node.rawEntityIds.includes('parent')), false);
  assert.equal(graph.nodes.some((node) => node.id === 'ui:file:repo-b:src/b.ts'), true);
});

test('external repository filtering remains distinct from the workspace view', () => {
  const external = { ...entity('external', 'unused'), repository: null };
  const graph = projectGraph(
    { ...snapshot, nodes: [...snapshot.nodes, external] },
    { ...options, band: 'entity', repository: EXTERNAL_REPOSITORY }
  );
  assert.deepEqual(graph.nodes.map((node) => node.id), ['external']);
});

test('visible guards keep the nearest topology and report omissions', () => {
  const graph = projectGraph(
    snapshot,
    { ...options, band: 'entity', rootId: 'b' },
    { nodes: 2, links: 1, animatedLinks: 0 }
  );
  assert.deepEqual(graph.nodes.map((node) => node.id), ['b', 'a']);
  assert.equal(graph.omittedNodes, 1);
  assert.equal(graph.omittedLinks, 1);
  assert.equal(graph.omittedAnimations, 1);
  assert.equal(graph.truncated, true);
});

function entity(id: string, repository: string): EntityRef {
  return {
    id,
    kind: 'callable',
    name: id,
    repository,
    origin: 'source',
    test: false,
    metadata: null
  };
}

function edge(id: string, from: string, to: string): SemanticEdge {
  return {
    id,
    from,
    to,
    kind: 'calls',
    confidence: 1,
    evidence: []
  };
}
