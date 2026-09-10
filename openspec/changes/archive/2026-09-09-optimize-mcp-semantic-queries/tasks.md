## 1. Query contracts

- [x] 1.1 Add version-2 entity-search and traversal DTO fields, validation, diagnostic counts, and repository-boundary termination; verify the focused `beholder-dto` tests cover empty, duplicate, unknown, conjunctive, and mutually exclusive inputs.
- [x] 1.2 Extend protobuf, daemon mapping, and daemon-client input structs with additive diagnostics and `target_repositories` fields, advance protocol version 23 to 24, and verify focused protocol and RPC mapping tests pass.

## 2. Indexed entity search

- [x] 2.1 Measure and record the candidate display-name relation/index `EXPLAIN`, build time, and database allocation on a disposable database clone; retain the index only if it removes the selected-shard scan.
- [x] 2.2 Add persisted fact-shard display names, selected-version backfill, and existing garbage-collection integration; verify one focused adapter test covers publication, stale-version exclusion, backfill, and removal.
- [x] 2.3 Replace shard substring scanning with indexed exact/prefix candidate lookup and stable ranking across all entity sources; verify focused search tests cover exact ID, exact name, prefixes, case sensitivity, limits, and rejected mid-string fragments.

## 3. Repository-constrained traversal

- [x] 3.1 Add bounded reverse target-reachability acquisition using existing dependency indexes and shared traversal budgets; verify a focused adapter test proves an impossible multi-target request returns no paths without entity hydration.
- [x] 3.2 Carry visited-target state through forward acquisition and admit only successors that can reach every missing target; verify the `O → A → B → D` and `A → C` cases, unordered targets, redundant intermediate targets, and no-common-path behavior.
- [x] 3.3 Implement final-target repository scoping and repository-boundary termination without acquiring outgoing repository branches; verify focused tests cover starts inside a target, ownership-neutral contracts, both directions, and incomplete-frontier precedence.

## 4. Diagnostics and MCP

- [x] 4.1 Split aggregate diagnostic counts from detailed row retrieval and thread metadata options through snapshot queries; verify summary mode executes no detailed-diagnostic query and detail mode returns identical counts plus rows.
- [x] 4.2 Expose `target_repositories` and `include_diagnostics` through MCP with summary diagnostics as its default; verify MCP router/schema tests and `scripts/test-mcp-integration.sh` cover both summary and explicit-detail requests.

## 5. Performance evidence

- [x] 5.1 Update `docs/SEMANTIC_QUERY_PERFORMANCE.md` with before/after plans, index cost, and two warm timings for entity search plus single- and multi-target traversal; verify every warm measurement is below one second on the existing large multi-repository benchmark workspace.
- [x] 5.2 Run only the targeted DTO, Mnestic adapter, daemon/RPC, daemon-client, and MCP tests touched above, then run `openspec validate optimize-mcp-semantic-queries --strict` and record any broader suite as CI-owned.
