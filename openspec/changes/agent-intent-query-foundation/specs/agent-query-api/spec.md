## Purpose

Defines bounded, revision-consistent query contracts that give agents useful semantic answers without weakening canonical evidence or existing client compatibility.

## ADDED Requirements

### Requirement: Additive versioned intent surface
The system SHALL expose `workspace_overview`, `find_entities`, `inspect_entity`, `trace`, and `impact` with independent `beholder.intent.<tool>.v1` schemas while retaining the current three MCP tools and their schemas.

#### Scenario: Existing client after upgrade
- **WHEN** an existing client lists MCP tools after the intent surface is added
- **THEN** it receives all current tools with their existing contracts plus the five additive tools

### Requirement: Revision-consistent intent envelopes
Every intent request SHALL require `workspace`, accept an optional available published `revision`, reject unknown fields, and return one immutable canonical snapshot with schema, canonical readiness, derived statuses, normalized request, applied/hard bounds, truncation, and origin.

#### Scenario: Publication during a query
- **WHEN** a new revision publishes after an intent request starts
- **THEN** all canonical and derived data in that response identify the request's exact revision

### Requirement: Strict entity resolution and typed failures
Entity references SHALL resolve a canonical ID first or one exact display name; unknown references, ambiguous names, invalid fields, unavailable revisions, exceeded hard bounds, acquisition/work/deadline failures, and unavailable or mismatched derived data SHALL use stable typed errors.

#### Scenario: Ambiguous exact display name
- **WHEN** an exact display name resolves to two entities
- **THEN** the request returns `ambiguous_entity` with at most 20 deterministically ordered candidates and never selects a prefix match

### Requirement: Bounded deterministic intent results
Intent results SHALL sort canonical IDs and tie-breakers deterministically, preserve structured evidence, report sound partial results with explicit omissions/truncation, and never describe an incomplete frontier as exhaustive.

#### Scenario: Impact reaches its node bound
- **WHEN** another affected entity exists beyond `max_nodes`
- **THEN** the response is successful but marks the result truncated with its reason and omitted count

### Requirement: Canonical availability is independent of derived availability
Canonical intent results SHALL remain usable when a derived topology or overlay is absent, failed, queued, running, or superseded.

#### Scenario: Derived topology failure
- **WHEN** topology for the selected revision has failed
- **THEN** `inspect_entity` returns its canonical section and reports the failed derived status separately

### Requirement: Repeatable agent evaluation gate
Before product behavior work for this program, the benchmark SHALL freeze repository/workspace revisions, tool schemas, model/prompt/context identities, cache schedule, task corpus, weighted evidence-backed gold assertions, transcript/result retention, repeated-run policy, and critical correctness/misleading-claim floors.

#### Scenario: Current MCP arm unavailable
- **WHEN** a paired baseline arm cannot run
- **THEN** the manifest records it as unavailable without imputing a score

### Requirement: Compatible CLI namespace migration
The CLI SHALL provide `beholder query {find,inspect,traverse,trace,impact,dependencies,why}` and `beholder debug inspect ...` while preserving top-level semantic and low-level aliases with their existing semantics for at least one documented release.

#### Scenario: Legacy trace invocation
- **WHEN** a user invokes the legacy top-level `trace`
- **THEN** it returns the legacy shortest-path contract rather than the new multipath intent result
