## ADDED Requirements

### Requirement: Non-blocking durable topology jobs
The indexing pipeline SHALL coalesce topology work by workspace, source revision, and complete derived key; use no more than five attempts; materialize one immutable source snapshot per attempt; and never delay canonical publication or semantic reads.

#### Scenario: Newer revision publishes during topology work
- **WHEN** topology work for revision R is running while R+1 publishes
- **THEN** publication continues and the R result cannot become selected for R+1

### Requirement: Derived currentness and recovery
If a requested source revision is unavailable before topology acquisition, the job SHALL become superseded and enqueue the latest eligible key; terminal failure SHALL remain inspectable and cancellation/recovery SHALL retain currentness fences.

#### Scenario: Source revision garbage collected before acquisition
- **WHEN** a recovered topology job cannot open its requested revision
- **THEN** it records `superseded` rather than querying another revision under the old key
