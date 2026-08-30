# ADR 0008: Desktop workspace graph prototype

- Status: accepted
- Date: 2026-08-30

## Context

Beholder has typed entities and relationships but no visual way to explore a
workspace from its broad structure down to a local dependency neighbourhood.
The interaction model is uncertain enough that an executable prototype is more
useful than committing to a production backend contract first.

The prototype needs to open on an aggregated workspace graph, narrow to a
repository, expand modules into files and typed entities as the user zooms, and
re-root around a selected entity. A re-rooted graph includes every reachable
upstream and downstream entity. Direction must remain legible while selection
and hover emphasize the active neighbourhood.

## Decision

Build a Tauri 2 desktop prototype with a SvelteKit client, shadcn-svelte
components, and [`force-graph`](https://github.com/vasturiano/force-graph) for
the 2D force-directed canvas.

The prototype uses a realistic typed Rust fixture behind the intended Tauri
command boundary. It does not add a daemon RPC merely to serve an interaction
that has not yet been validated. Replacing the fixture later requires one
bounded, revision-consistent workspace/repository topology query returning the
existing entity and semantic-edge DTOs with explicit truncation metadata.

The UI:

1. opens on aggregated workspace repositories and modules;
2. filters to a repository;
3. expands topology through repository, module, file, and entity zoom bands;
4. re-roots on selection around all reachable upstream and downstream nodes;
5. colors upstream and downstream paths differently;
6. uses animated directional particles only on emphasized links;
7. highlights the selected node, direct neighbours, and incident edges; and
8. limits filters to repository, relationship kind, tests, and origin.

Visible node, link, and animation guards protect the renderer without changing
all-reachable traversal semantics. When a projection exceeds a guard, the UI
keeps the nearest topology deterministically, reports omissions, and asks the
user to narrow the basic filters.

Do not add entity search, persisted layouts, graph editing, daemon lifecycle
management, a generic renderer abstraction, or a new backend API to this slice.

### Production integration boundary

The future production flow reuses the existing typed client boundary:

```text
SvelteKit view
    -> Tauri command (Rust)
    -> beholder-daemon-client
    -> existing Unix-socket gRPC daemon
    -> revision-consistent SemanticStore snapshot
    -> existing typed query DTO
    -> client-side filter and aggregation
    -> force-graph
```

The ownership boundaries are explicit in the current code:

| Surface | Existing contract | Production use |
| --- | --- | --- |
| Workspace | `ListWorkspaces` returns names and selected repositories ([proto](../../proto/beholder/v1/daemon.proto#L333-L377)); the client exposes `list_workspaces` ([client](../../crates/daemon-client/src/lib.rs#L447-L457)). | Workspace picker and virtual workspace/repository grouping nodes. |
| Repository | `GetRepository` returns revision and indexing status ([proto](../../proto/beholder/v1/daemon.proto#L379-L411)); the client exposes `get_repository` ([client](../../crates/daemon-client/src/lib.rs#L422-L435)). | Optional freshness detail in the inspector; not a graph query. |
| Entity | `EntityRef` already carries stable ID, kind, display name, repository, origin, test flag, and typed metadata ([DTO](../../crates/dto/src/lib.rs#L245-L254)). | Raw leaf node. No second entity model in Rust. |
| Relationship | `SemanticEdge` already carries direction, closed relation kind, confidence, and evidence ([DTO](../../crates/dto/src/lib.rs#L256-L359)). | Raw directed link and inspector evidence. |
| Direct context | `context(workspace, entity)` returns incoming and outgoing incident edges ([client](../../crates/daemon-client/src/lib.rs#L253-L263), [mapper](../../crates/adapters-mnestic/src/semantic.rs#L21-L58)). | One-hop detail refresh after selection, if needed. |
| Downstream | `dependencies(workspace, entity, max_hops)` returns the outgoing reachable subgraph and hop counts ([client](../../crates/daemon-client/src/lib.rs#L265-L280), [mapper](../../crates/adapters-mnestic/src/semantic.rs#L60-L92)). | “Downstream” mode. |
| Upstream | `impact(workspace, entity, max_hops)` returns the incoming reachable subgraph and hop counts ([client](../../crates/daemon-client/src/lib.rs#L282-L297), [mapper](../../crates/adapters-mnestic/src/semantic.rs#L94-L126)). | “Upstream” mode. |
| Path | `trace(workspace, from, to, max_hops)` returns the directed shortest path ([client](../../crates/daemon-client/src/lib.rs#L299-L316), [mapper](../../crates/adapters-mnestic/src/semantic.rs#L218-L253)). | Highlight a requested path without recomputing it in TypeScript. |
| Query state | Every result includes view, analysis revision, freshness, completeness, diagnostics, hop limit, and truncation ([DTO](../../crates/dto/src/lib.rs#L132-L161)). | Visible stale/incomplete/truncated warning and safe merge guard. |

The reusable production data boundary is therefore the public functions and
serializable results in `beholder-daemon-client` and `beholder-dto`.
`docs/QUERY_OUTPUT.md` says renderers consume typed results and Mnestic rows stop
at the storage adapter. The fixture deliberately preserves those entity and
edge DTO shapes so the desktop renderer can later become another consumer of
that boundary.

Two details prevent accidental contract drift:

- Workspace and repository are view/ownership metadata, not semantic entities. Add them only as UI-owned virtual grouping nodes; do not publish fake semantic facts.
- Query edge IDs (`e1`, `e2`, and so on) are response-local. Client merges must key a raw relationship by `(from, to, kind)` and merge evidence, not treat the returned edge ID as durable identity.

#### Current gaps that the prototype must expose rather than hide

- There is no entity list or search RPC.
- There is no bounded “whole workspace graph” RPC. This is why the seedless
  workspace view remains fixture-backed in the prototype.
- Reachability results contain dependency topology, not a complete structural
  containment tree. Exact `defines`/`field_of` parents and children are
  available through `context`; loading context for every reachable node would
  create an unacceptable N+1 query pattern.
- “File” and general-purpose “type” are not distinct `EntityKind` variants.
  Source files and language scopes are represented as `Namespace` entities
  connected by `defines`; GraphQL and Protobuf types are typed explicitly.
- The domain `Workspace` value returned by `list_workspaces` is not a serializable UI DTO. The Tauri command should map only `name`, repository `identity`, and `display_name` into a local `WorkspaceSummary`; do not add UI serialization concerns to `beholder-domain`.

### Renderer choice

All four candidates are framework-agnostic browser libraries and can be created after Svelte mount in a Tauri webview. Tauri's official SvelteKit guidance requires a static adapter, SPA mode, and disabled SSR, which also avoids browser-global checks for these renderers ([Tauri SvelteKit guide](https://v2.tauri.app/start/frontend/sveltekit/)).

| Renderer | Force layout | Zoom-time topology changes | Direction and animation | Selection | Large graph posture | Decision |
| --- | --- | --- | --- | --- | --- | --- |
| [`force-graph`](https://github.com/vasturiano/force-graph) | Built-in `d3-force`, configurable and reheatable. | `graphData()` supports incremental updates; `onZoomEnd` exposes scale. | Native arrowheads and moving `source -> target` particles. | Click/hover callbacks plus documented highlight and multi-select examples. | Canvas renderer with official ~4k and ~75k element examples; CPU simulation is the ceiling. | **Choose.** Meets every prototype interaction with one dependency and no custom renderer. |
| [Cytoscape.js](https://js.cytoscape.org/) | Built-in CoSE and extension layouts. | Mature add/remove, events, classes, and zoom API. | Arrow styles are native; moving dashed edges require repeatedly changing `line-dash-offset`. | Best built-in selection/state model of the shortlist. | Canvas docs warn that edges, arrows, labels, and animation degrade on large graphs ([performance notes](https://js.cytoscape.org/#performance)). | Viable fallback if editing/compound graph UX becomes more important than reachability scale. More machinery for this read-only prototype. |
| [Sigma.js](https://www.sigmajs.org/docs/) + Graphology | ForceAtlas2 is available as a separate layout/worker. | Graphology mutations trigger refresh; camera state is available. | Native arrow program, but moving directional edges require a custom WebGL program or extra layer ([renderer model](https://www.sigmajs.org/docs/advanced/renderers/)). | Reducers are a good highlighting mechanism. | WebGL library explicitly aimed at thousands of nodes and edges. | Reject for prototype: it turns one explicit requirement into custom rendering and adds Graphology plus layout wiring. |
| [`@cosmograph/cosmograph`](https://cosmograph.app/docs-lib/) | Native GPU force simulation. | Hot add/remove APIs and zoom control are documented ([hot updates](https://cosmograph.app/docs-lib/features/data-update/)). | Native arrowheads, but no documented moving directional particle/flow primitive. | Native point/link selection and highlighting. | Strongest large-graph option, with Arrow-oriented advanced input. | Reject for prototype: heavier data preparation and custom animation would be required. Revisit only after a measured `force-graph` ceiling. |

`force-graph` directly documents the required primitives: incremental `graphData`, directional arrows and particles, force configuration, click/hover handlers, and zoom-end callbacks in its [API](https://github.com/vasturiano/force-graph#api-reference). Its large example is evidence that the renderer is worth prototyping, not a Beholder performance guarantee.

### Prototype data contract

Do not add a daemon schema for the prototype. The Tauri bridge exposes
`list_workspaces` and `load_graph`; both are backed by a typed Rust fixture that
uses the existing `beholder-dto` entity, edge, query metadata, and traversal
types. The frontend receives a `GraphSnapshot` containing workspace metadata,
typed nodes and edges, revision/freshness metadata, and explicit truncation.

The frontend derives renderer input as a view model, not a transport contract:

```ts
type GraphNode = {
  id: string;                 // raw entity ID or virtual ui:<level>:<key>
  label: string;
  level: 'workspace' | 'repository' | 'file' | 'scope' | 'entity';
  rawEntityIds: string[];
  count: number;
  selected: boolean;
};

type GraphLink = {
  source: string;
  target: string;
  kind: RelationKind;
  count: number;
  confidence: number;         // maximum confidence among bundled raw edges
  evidenceCount: number;
  selected: boolean;
};
```

Keep the original DTO arrays alongside this projection so selection can show exact entity metadata and evidence without another backend call.

### Aggregation and semantic zoom rules

Build one deterministic projection from the immutable graph snapshot at four
detail levels. Never rewrite the underlying semantic graph.

1. **Repository:** group the workspace by `EntityRef.repository`; unowned
   entities remain in an external group.
2. **Module:** group source scopes and the entities they define.
3. **File:** derive containment from structural edges and evidence paths;
   ambiguous entities remain in a deterministic fallback bucket.
4. **Entity:** show raw typed entities, including callables, operations, RPCs,
   messages, topics, and services.

At each level, map every raw endpoint to its visible ancestor. Bundle only
edges with the same visible `(source, target, RelationKind)`. Drop visible
self-links but retain their count on the aggregate node as `internalEdges`.
Structural edges establish containment; dependency, contract, RPC, GraphQL,
and messaging edges remain visible between containers.

Use zoom bands with hysteresis, recalculating only on `onZoomEnd` when a boundary is crossed:

| Relative zoom `k` | Visible level |
| --- | --- |
| `< 0.75` | repository |
| `0.75 .. < 1.5` | module |
| `1.5 .. < 3` | file |
| `>= 3` | raw entity |

These are calibration defaults, not a backend contract. Recalculate only after
a zoom boundary is crossed, update `graphData`, and briefly reheat the force
simulation.

### Traversal semantics

Selecting a raw entity re-roots the projection. The client traverses every
non-structural edge in both directions:

- **Downstream** follows `from` to `to` and answers what the entity depends on.
- **Upstream** follows incoming edges and answers what may be affected by the
  entity.
- A node or edge reachable in both directions uses a combined visual state.

The traversal has no semantic hop limit. Render guards may omit distant visible
topology, but the omission is explicit and does not redefine reachability.

### Minimal controls and filters

The prototype needs only:

- repository;
- relation-kind multi-select;
- include tests; and
- origin: source, generated, external dependency.

Filtering happens after the typed response is received. After filtering edges,
retain only nodes still reachable from the selected root so the renderer does
not show unexplained islands. No text search, saved views, confidence slider,
time travel, ownership filter, minimap, or layout selector belongs in this
prototype.

Selection highlights the chosen aggregate/raw node, its incident visible edges,
and neighbouring nodes; everything else is dimmed. Directional particles are
limited by the animation guard.

### Representative validation fixture

Use a realistic Fresha-shaped typed fixture spanning Checkout, Packages, B2C,
and a Protobuf registry. It crosses GraphQL, gRPC, Kafka, generated code, tests,
and an external dependency. The fixture is product-neutral test data; it is not
a claim about live workspace contents.

The acceptance pass is:

1. repository -> module -> file -> entity expansion occurs only when zoom bands change;
2. selecting an aggregate highlights all represented raw IDs and the inspector shows exact evidence;
3. selecting an entity shows all upstream and downstream reachable topology;
4. tests, origins, repositories, and relation kinds filter without breaking connectivity; and
5. the graph remains interactive at the prototype render guard below.

### Known ceilings and stop conditions

There are two independent ceilings.

#### Query ceiling

`dependencies` and `impact` compute transitive reachability in Datalog first; the Rust mapper applies `max_hops` afterward ([dependency rules](../../rules/core/dependencies.datalog), [mapper](../../crates/adapters-mnestic/src/semantic.rs#L128-L146)). A low hop setting therefore bounds the returned graph but does not necessarily bound storage-query work. The daemon client also caps decoded responses at 64 MiB ([client](../../crates/daemon-client/src/lib.rs#L27-L45)).

Do not “fix” either limit in the UI prototype. Production integration must add
the bounded workspace topology query first and measure it independently from
renderer performance.

#### Renderer ceiling

`force-graph` uses a CPU `d3-force` simulation and HTML canvas redraw. Its ~75k-element example is not a guarantee for labelled, selectable, animated Beholder topology. Apply these explicit prototype guards:

- maximum 10,000 visible nodes;
- maximum 25,000 visible links;
- maximum 250 simultaneously animated links, with the normal case limited to the selected edge/path; and
- labels only for selected/hovered nodes below file level.

When a projection exceeds a guard, keep the nearest-hop nodes deterministically
and show how many were omitted; ask the user to narrow repositories, relations,
tests, or origins. Revisit Cosmograph or Sigma only if the query is healthy and
a measured renderer profile, not graph size alone, shows the canvas/CPU ceiling.

### Implementation layout

Keep the prototype in one application:

```text
apps/graph-ui/
├── package.json                 # SvelteKit, Tauri JS API, force-graph
├── svelte.config.js             # adapter-static with SPA fallback
├── vite.config.ts
├── src/
│   ├── lib/
│   │   ├── graph.ts             # reachability, filters, projection, guards
│   │   └── GraphCanvas.svelte   # force-graph lifecycle and interactions
│   └── routes/
│       ├── +layout.ts           # export const ssr = false
│       └── +page.svelte         # controls, graph, evidence inspector
└── src-tauri/
    ├── Cargo.toml               # tauri + beholder-dto
    ├── build.rs
    ├── tauri.conf.json
    └── src/
        ├── fixture.rs           # realistic typed graph fixture
        └── main.rs              # list_workspaces/load_graph commands
```

Register `apps/graph-ui` in Moon, add its `src-tauri` crate to the Cargo
workspace, and pin Node/pnpm in `mise.toml`. Keep Cargo authoritative for Rust
and package scripts authoritative for Svelte. Do not create a shared UI crate,
renderer adapter interface, graph service, global store, or component library
for one screen.

The smallest checks cover edge bundling, all-reachable re-rooting, containment
projection, and visible guards, followed by Svelte type-check/build and a Tauri
Rust check.

## Consequences

- The interaction can be evaluated without prematurely expanding the daemon API.
- The fixture cannot establish live-workspace query latency, freshness, or scale.
- `force-graph` supplies the required interaction primitives with one renderer
  dependency, but its CPU simulation needs explicit visible guards.
- Production integration is a separate decision after the prototype proves the
  workspace-to-neighbourhood interaction.
- A different renderer is justified only by a measured rendering bottleneck.

## Sources

Repository sources:

- [`README.md`](../../README.md) — current workspace, daemon, and query architecture.
- [`docs/QUERY_OUTPUT.md`](../QUERY_OUTPUT.md) — typed renderer boundary and raw-output guarantees.
- [`crates/dto/src/lib.rs`](../../crates/dto/src/lib.rs) — entity, edge, result, freshness, and traversal types.
- [`proto/beholder/v1/daemon.proto`](../../proto/beholder/v1/daemon.proto) — current workspace, repository, and query RPCs.
- [`crates/daemon-client/src/lib.rs`](../../crates/daemon-client/src/lib.rs) — reusable process client and response ceiling.
- [`crates/adapters-mnestic/src/semantic.rs`](../../crates/adapters-mnestic/src/semantic.rs) and [`rules/core`](../../rules/core) — exact traversal direction and hop filtering.
- [`docs/INDEXING_PERFORMANCE.md`](../INDEXING_PERFORMANCE.md) — representative large-workspace measurements.

External primary sources:

- [Tauri 2: SvelteKit](https://v2.tauri.app/start/frontend/sveltekit/) — static SPA configuration and SSR boundary.
- [`force-graph` README/API](https://github.com/vasturiano/force-graph) — canvas/d3-force architecture, incremental data, arrows, particles, events, zoom, examples, and MIT license.
- [Cytoscape.js documentation](https://js.cytoscape.org/) — layouts, edge animation property, selection, and performance notes.
- [Sigma.js documentation](https://www.sigmajs.org/docs/) — WebGL/Graphology model, reducers, and renderer programs.
- [Cosmograph library documentation](https://cosmograph.app/docs-lib/) — GPU force graph, JS/TS integration, hot data changes, arrows, and selection.
