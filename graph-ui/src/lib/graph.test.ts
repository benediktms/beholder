import assert from 'node:assert/strict';
import test from 'node:test';
import {
  EXTERNAL_REPOSITORY,
  ORIGINS,
  directHighlight,
  findEntity,
  investigate,
  labelOpacity,
  nodeValue,
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

test('low-degree nodes stay small while connected nodes scale up', () => {
  assert.equal(nodeValue(0), 0.25);
  assert.equal(nodeValue(1), 0.25);
  assert.equal(nodeValue(2), 1.25);
  assert.equal(nodeValue(3), 2.25);
});

test('labels fade in with zoom but highlighted nodes remain labelled', () => {
  assert.equal(labelOpacity(0.55, 0), 0);
  assert.ok(Math.abs(labelOpacity(1.3, 0) - 0.5) < Number.EPSILON);
  assert.equal(labelOpacity(1.5, 0), 1);
  assert.equal(labelOpacity(0.55, 0.8), 0.8);
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
  assert.equal(findEntity(candidates, '  '), undefined);
});

test('complete projections never apply speculative topology limits', () => {
  const graph = projectGraph(snapshot, options);
  assert.deepEqual(graph.nodes.map((node) => node.id), ['a', 'b', 'c']);
  assert.equal(graph.omittedNodes, 0);
  assert.equal(graph.omittedLinks, 0);
  assert.equal(graph.truncated, false);
});

test('pinned investigations preserve dependency, impact, and shortest-path direction', () => {
  assert.deepEqual([...investigate(snapshot, 'dependencies', 'a').nodeIds], ['a', 'b', 'c']);
  assert.deepEqual([...investigate(snapshot, 'impact', 'c').nodeIds], ['c', 'b', 'a']);
  assert.deepEqual([...investigate(snapshot, 'trace', 'a', 'c').edgeIds].sort(), ['e1', 'e3']);
});

test('impact follows implemented_by from an RPC without crossing between implementations', () => {
  const rpc = 'grpc://example.Service/Call';
  const implementationA = 'implementation-a';
  const implementationB = 'implementation-b';
  const implementedByA = { ...edge('implemented-a', rpc, implementationA), kind: 'implemented_by' as const };
  const implementedByB = { ...edge('implemented-b', rpc, implementationB), kind: 'implemented_by' as const };
  const result = investigate(
    {
      ...snapshot,
      nodes: [entity(rpc, 'repo-a'), entity(implementationA, 'repo-a'), entity(implementationB, 'repo-b')],
      edges: [implementedByA, implementedByB]
    },
    'impact',
    implementationA
  );
  assert.deepEqual([...result.nodeIds], [implementationA]);
  assert.deepEqual(
    [...investigate({ ...snapshot, edges: [implementedByA, implementedByB] }, 'impact', rpc).nodeIds],
    [rpc, implementationA, implementationB]
  );
});

test('trace keeps both endpoints when no path exists', () => {
  const result = investigate(snapshot, 'trace', 'c', 'a');
  assert.deepEqual([...result.nodeIds], ['c', 'a']);
  assert.deepEqual([...result.edgeIds], []);
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
