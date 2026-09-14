## Current-state audit and evidence

Repository authority at planning: `main` `476d54fdf853ec0eab557e37c7243ffacfb6fdd3`; current behavior is the ten main specs. Archived constraints are `2026-09-09-optimize-mcp-semantic-queries` and `2026-09-09-cross-language-evidence-contexts`. The connected Beholder graph was stale/indexing/incomplete, so source/specs are authoritative; MCP was used only to narrow paths.

### MCP and RPC

`crates/mcp/src/main.rs` exposes exactly `list_workspaces`, `search_entities`, and `traverse_graph`. `search_entities` calls `SearchEntities`, returns `beholder.entity_search.v2`, preserves case-sensitive exact/prefix ID/name matching, defaults 20/max 100, and defaults diagnostics to counts. `traverse_graph` calls `TraverseGraph`, returns `beholder.traverse_graph.v2`, accepts direction, destination or conjunctive repositories, hops 0-32 and paths 1-200, and reports acquisition/path/work/depth truncation. MCP serializes typed daemon errors as `{kind,code,message}`. `scripts/test-mcp-integration.sh` covers tool listing, JSON equality, diagnostics, targets, repeatability, evidence contexts, and structured errors. Daemon-client shields reject old schemas/ignored diagnostic preferences or repository targets.

### CLI, presentation, UI, workers

CLI classes: GUI `gui`; administration `daemon`, `workspace`, `repository`, `plugin`, `job`, `cache`, `index`, `enrich`; debug `index-rust`, `inspect {grpc-bindings,relations,revisions,observations}`; semantic `context`, `dependencies`, `impact`, `trace`, `why`; and `benchmark`, `benchmark-query`. Existing semantic commands are deliberately non-interchangeable: trace is one deterministic shortest path; why is evidence-first; dependencies/impact are reachability/entity-hop results. `crates/presentation` renders typed CLI results and is not MCP's renderer. Graph UI (`crates/graph-ui`, `graph-ui/src/lib/graph.ts`, `graph-ui/src/routes/+page.svelte`) loads full unbounded `WorkspaceTopology` and computes client-side queries with different semantics; it is not an architecture-map backend. Analyzer workers/plugins enrich syntax facts; topology/provider work must not reuse enrichment payloads or publish canonical observations.

### Data and issue boundaries

`crates/domain/src/semantic.rs` owns closed relations/evidence/provenance; `crates/dto/src/lib.rs` owns query DTOs and `WorkspaceTopology`; `crates/adapters-mnestic` owns deterministic storage aggregation and immutable snapshots. Repository is metadata, not a node/boundary. Relevant issues: #203 bounded human projections and #204 service-flow projections are superseded only after shipped acceptance; #180 remains an evidence-quality input; #163/#169 are UI adjacency; #88/#91 cache/evaluation discipline; #116/#118/#132 job/telemetry adjacency; #80/#205-#209 are unrelated correctness/toolchain work. Route logs lack client identity and cannot prove usage/removal. A1's repo-only/current-MCP baseline must be captured before A3-A5 or any B product behavior.

### Exhaustive CLI surface

| Surface | Current commands |
| --- | --- |
| Daemon | `install`, `uninstall`, `start`, `run`, `status`, `stop` |
| Jobs | `list [--page-token]`, `get <id>` |
| Workspaces | `register`, `enable-plugin`, `disable-plugin`, `list` |
| Plugins | `install`, `replace`, `list`, `remove` |
| Repositories | `register`, `delete`, `show` |
| Cache | `clear`, `gc [--status]` |
| Direct operations | `index-rust`, `index`, `enrich`, `inspect {grpc-bindings,relations,revisions,observations}` |
| Human/query | `gui`, `context`, `impact`, `dependencies`, `trace`, `why` |
| Evaluation | `benchmark`, `benchmark-query` |

### Exact MCP/RPC pointers

| Tool | Request/RPC | Response/pointers |
| --- | --- | --- |
| `list_workspaces` | no input; client `list_workspaces` → `ListWorkspaces` | MCP `WorkspaceList` of name/repository identity/display name; `mcp/main.rs:120`, client `lib.rs:547`, proto `daemon.proto:21,424`, daemon `rpc_service.rs:801` |
| `search_entities` | strict workspace/query; optional limit 1–100 default 20/diagnostics; `SearchEntitiesRequest` → `SearchEntities` | `EntitySearchResult` schema `beholder.entity_search.v2`, metadata/query/matches; shields; `main.rs:132`, client `293`, proto `83,311`, daemon `394`, DTO `lib.rs` |
| `traverse_graph` | strict workspace/start/direction/destination/targets/hops 0–32 default 8/paths 1–200 default 50/diagnostics; `TraverseGraphRequest` → `TraverseGraph` | `TraverseGraphResult` schema `beholder.traverse_graph.v2`, metadata/query/nodes/edges/paths/truncation; shields; `main.rs:152`, client `310`, proto `641,688`, daemon `343`, DTO `557,574,657` |
