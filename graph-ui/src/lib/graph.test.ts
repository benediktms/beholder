import assert from 'node:assert/strict';
import test from 'node:test';
import {
  EXTERNAL_REPOSITORY,
  ORIGINS,
  directHighlight,
  entityCommunity,
  extendTrail,
  findEntity,
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
  nodes,
  edges
};

const options: ProjectionOptions = {
  repositories: [],
  relationKinds: ['calls'],
  includeTests: true,
  origins: ORIGINS
};

test('bundles equivalent visible edges without losing raw topology', () => {
  const graph = projectGraph(snapshot, options);
  assert.equal(graph.nodes.length, 3);
  assert.equal(graph.links.length, 2);
  assert.equal(graph.links.find((link) => link.rawEdgeIds.includes('e1'))?.count, 2);
  assert.deepEqual(Object.fromEntries(graph.nodes.map((node) => [node.id, node.degree])), {
    a: 1,
    b: 2,
    c: 1
  });
  assert.equal(graph.rawLinkCount, 3);
});

test('direct highlighting distinguishes upstream and downstream relationships', () => {
  const graph = projectGraph(snapshot, options);
  const highlight = directHighlight(graph.links, 'b');
  assert.deepEqual([...highlight.upstreamNodeIds], ['a']);
  assert.deepEqual([...highlight.downstreamNodeIds], ['c']);
  assert.deepEqual([...highlight.upstreamLinkIds], ['a|calls|b']);
  assert.deepEqual([...highlight.downstreamLinkIds], ['b|calls|c']);
});

test('selection extends only across connected nodes and truncates on backtracking', () => {
  assert.deepEqual(extendTrail({ trail: ['a'], next: 'b', connected: true }), ['a', 'b']);
  assert.deepEqual(extendTrail({ trail: ['a', 'b'], next: 'a', connected: true }), ['a']);
  assert.deepEqual(extendTrail({ trail: ['a', 'b'], next: 'c', connected: false }), ['c']);
});

test('filesystem communities use stable package boundaries', () => {
  assert.deepEqual(
    entityCommunity(entity(
      'repo://github.com/benediktms/beholder/rust/crates/daemon/src/rpc_service/start',
      'github.com/benediktms/beholder'
    )),
    {
      id: 'github.com/benediktms/beholder/rust/crates/daemon',
      label: 'crates/daemon'
    }
  );
  assert.equal(
    entityCommunity(entity(
      'repo://github.com/benediktms/beholder/javascript/graph-ui/src/lib/graph/projectGraph',
      'github.com/benediktms/beholder'
    )).label,
    'graph-ui'
  );
  assert.equal(
    entityCommunity(entity(
      'repo://github.com/benediktms/beholder/protobuf/Beholder.V1.Daemon.Service/GetWorkspace',
      'github.com/benediktms/beholder'
    )).label,
    'protobuf'
  );
});

test('selection-independent projection keeps disconnected nodes visible', () => {
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
      relationKinds: ['calls']
    }
  );
  assert.equal(graph.nodes.some((node) => node.id === 'parent'), true);
  assert.equal(graph.nodes.some((node) => node.id === 'b'), true);
});

test('external repository filtering remains distinct from the workspace view', () => {
  const external = { ...entity('external', 'unused'), repository: null };
  const graph = projectGraph(
    { ...snapshot, nodes: [...snapshot.nodes, external] },
    { ...options, repositories: [EXTERNAL_REPOSITORY] }
  );
  assert.deepEqual(graph.nodes.map((node) => node.id), ['external']);
});

test('repository filtering accepts multiple repositories', () => {
  const graph = projectGraph(snapshot, {
    ...options,
    repositories: ['repo-a', 'repo-c']
  });
  assert.deepEqual(graph.nodes.map((node) => node.id), ['a', 'c']);
});

test('entity search prefers exact case-sensitive canonical IDs', () => {
  const candidates = [entity('Example', 'repo-a'), entity('example', 'repo-a')];
  assert.equal(findEntity(candidates, 'example')?.id, 'example');
  assert.equal(findEntity([entity('INSPECT', 'repo-a')], 'inspect')?.id, 'INSPECT');
  assert.equal(findEntity(candidates, '  '), undefined);
});

test('visible guards keep deterministic topology and report omissions', () => {
  const graph = projectGraph(snapshot, options, { nodes: 2, links: 1 });
  assert.deepEqual(graph.nodes.map((node) => node.id), ['a', 'b']);
  assert.deepEqual(graph.nodes.map((node) => node.degree), [1, 1]);
  assert.equal(graph.omittedNodes, 1);
  assert.equal(graph.omittedLinks, 1);
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
