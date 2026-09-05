import assert from 'node:assert/strict';
import test from 'node:test';
import {
  EXTERNAL_REPOSITORY,
  ORIGINS,
  directHighlight,
  entityCommunity,
  extendTrail,
  findEntity,
  mergeNeighborhoodBatches,
  projectGraph,
  projectLevelOfDetail,
  type EntityRef,
  type GraphNeighborhood,
  type GraphNeighborhoodBatch,
  type GraphOverviewSnapshot,
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

test('level-of-detail projection starts with aggregate communities', () => {
  const graph = projectLevelOfDetail(overview(), [], options);
  assert.deepEqual(graph.nodes.map((node) => node.id), [
    'community://repository/repo-a',
    'community://repository/repo-b'
  ]);
  assert.equal(graph.nodes.every((node) => node.aggregate), true);
  assert.equal(graph.links[0].count, 3);
  assert.equal(graph.rawNodeCount, 30);
});

test('repository expansion replaces only its aggregate and collapses boundary entities', () => {
  const graph = projectLevelOfDetail(overview(), [repositoryNeighborhood()], options);
  assert.deepEqual(graph.nodes.map((node) => node.id), [
    'a1',
    'a2',
    'community://repository/repo-b'
  ]);
  assert.equal(graph.nodes.some((node) => node.id === 'community://repository/repo-a'), false);
  assert.equal(graph.nodes.some((node) => node.id === 'b1'), false);
  assert.equal(
    graph.links.some(
      (link) => link.source === 'a1' && link.target === 'community://repository/repo-b'
    ),
    true
  );
});

test('repository expansion trusts authoritative ownership for contract entity IDs', () => {
  const neighborhood = repositoryNeighborhood();
  neighborhood.nodes[0] = {
    ...neighborhood.nodes[0],
    id: 'proto-method://booking.v1.Booking/Create',
    name: 'Create',
    repository: 'repo-a'
  };
  neighborhood.edges = neighborhood.edges.map((candidate) => ({
    ...candidate,
    from: candidate.from === 'a1' ? neighborhood.nodes[0].id : candidate.from
  }));
  const graph = projectLevelOfDetail(overview(), [neighborhood], options);
  assert.equal(graph.nodes.some((node) => node.id === neighborhood.nodes[0].id), true);
});

test('truncated expansion retains the unmaterialized overview boundary', () => {
  const neighborhood = { ...repositoryNeighborhood(), truncated: true };
  const graph = projectLevelOfDetail(overview(), [neighborhood], options);
  assert.equal(
    graph.nodes.find((node) => node.id === 'community://repository/repo-a')?.label,
    'repo-a (remaining)'
  );
  assert.equal(
    graph.links.find((link) =>
      link.source === 'community://repository/repo-a' &&
      link.target === 'community://repository/repo-b'
    )?.count,
    2
  );
});

test('stream batches validate order and assemble one cached neighborhood', () => {
  const neighborhood = repositoryNeighborhood();
  const batches: GraphNeighborhoodBatch[] = [
    {
      ...neighborhood,
      nodes: neighborhood.nodes.slice(0, 2),
      edges: [],
      batchIndex: 0,
      complete: false
    },
    {
      ...neighborhood,
      nodes: neighborhood.nodes.slice(2),
      edges: neighborhood.edges,
      batchIndex: 1,
      complete: true
    }
  ];
  assert.deepEqual(mergeNeighborhoodBatches(batches), neighborhood);
  assert.throws(
    () => mergeNeighborhoodBatches([{ ...batches[1], batchIndex: 1 }]),
    /expected batch 0/
  );
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

function overview(): GraphOverviewSnapshot {
  return {
    schema: 'overview',
    workspace: snapshot.workspace,
    metadata: snapshot.metadata,
    communities: [
      {
        id: 'community://repository/repo-a',
        kind: 'repository',
        name: 'repo-a',
        repository: 'repo-a',
        entity_count: 10
      },
      {
        id: 'community://repository/repo-b',
        kind: 'repository',
        name: 'repo-b',
        repository: 'repo-b',
        entity_count: 20
      }
    ],
    edges: [{
      id: 'c1',
      from: 'community://repository/repo-a',
      to: 'community://repository/repo-b',
      kind: 'calls',
      count: 3
    }]
  };
}

function repositoryNeighborhood(): GraphNeighborhood {
  return {
    schema: 'neighborhood',
    metadata: snapshot.metadata,
    focus: { kind: 'repository', id: 'repo-a' },
    maxEdges: 2000,
    truncated: false,
    nodes: [entity('a1', 'repo-a'), entity('a2', 'repo-a'), entity('b1', 'repo-b')],
    edges: [edge('n1', 'a1', 'a2'), edge('n2', 'a1', 'b1')]
  };
}
