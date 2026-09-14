## ADDED Requirements

### Requirement: Intent-query compatibility boundary
The query API SHALL keep `beholder.entity_search.v2` and `beholder.traverse_graph.v2` behavior intact while exposing additive intent schemas; similarly named intent operations SHALL not alias legacy context, trace, impact, dependencies, or why semantics.

#### Scenario: Intent trace alongside legacy trace
- **WHEN** both an intent trace and a legacy trace are requested
- **THEN** the intent result returns bounded destination paths while the legacy result retains its current shortest-path behavior
