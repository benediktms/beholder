# Semantic query performance

> [!NOTE]
> This document retains query measurements and index evidence. The current behavioral
> contract is [`query-api`](../openspec/specs/query-api/spec.md).

Measurements below were taken on 2026-09-03 against the installed daemon's
SQLite database with the daemon stopped. The database contained the `beholder`
view at revision 1028 and the seven-repository `fresha` view at revision 183.
Phase measurements used a warm, unoptimized, SQLite-enabled targeted test
binary so graph acquisition, Rust processing, entity hydration, metadata, and
serialization could be timed separately.

## Baseline

- Installed-daemon Fresha `dependencies` and `impact` queries at `max_hops=4`
  exceeded 30 seconds because recursive Datalog acquired the complete reachable
  graph before Rust applied the hop limit.
- A Beholder `context` query exceeded 2 minutes 20 seconds, and cancelling its
  client left the synchronous daemon worker consuming CPU.
- The production-scale database was 9,625,325,568 bytes before experimental
  indexes were built.

## Bounded result

The production query now acquires one indexed frontier at a time and performs a
final boundary probe for exact truncation. Semantic reads emit a warning on
their trace when they exceed five seconds, but are allowed to finish. Blocking
database work runs on Tokio's blocking pool, so a disconnected client releases
its async worker immediately. Mnestic 0.14 does not expose a request-scoped
cancellation handle, so Beholder deliberately does not infer a query ID from
the global `::running` registry and risk killing another concurrent request.

| View and query | Revision read | Acquisition | Processing | Hydration | Metadata | Serialization | Total |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Beholder dependencies, depth 4, `canonical_remote` | 1.0 ms | 51.7 ms | 0.6 ms | 37.6 ms | 33.8 ms | 0.3 ms | 0.13 s |
| Fresha dependencies, depth 4, `document_object` | 4.7 ms | 85.3 ms | 0.5 ms | 8.9 ms | 720.6 ms | 0.3 ms | 0.82 s |
| Fresha impact, depth 4, `string_prop` | 1.0 ms | 73.0 ms | 0.3 ms | 8.7 ms | 627.6 ms | 0.1 ms | 0.72 s |

The final reverse-override correctness check adds an indexed selected-shard
existence lookup; its focused traversal test completes in 0.20 seconds.

## Materialized traversal edges

Materializing the resolved dependency edge set moves composition and override
resolution to publication. On the Fresha workspace, depth-four dependencies
completed in 0.59 seconds wall-clock and impact in 0.48 seconds while indexing
was active. Their server spans took 126 ms and 103 ms respectively; individual
indexed frontier reads took 0.10-0.84 ms.

The one-time upgrade took 91.18 seconds: 3.32 seconds for Beholder, 77.98
seconds for Fresha, 9 ms for the seed view, and 7.90 seconds for the TypeScript
performance workspace. A later Fresha publication rebuilt the derived edge set
in 93.35 seconds.

Changed fact-shard and enrichment publications now carry their affected source
entities into the materialization step. Indexed outgoing stages refresh only
those sources; full rebuilds remain limited to schema migration and legacy
repository-snapshot publication. An installed release run refreshed 6,201
Fresha sources and 71,978 resolved edges in 3.03 seconds. The initial incremental
query, which still evaluated the compound direct rules as one plan, had taken
74.24 seconds for 3,728 sources on the same workspace. A subsequent 48-source,
564-edge Fresha refresh completed in 354 ms.

An installed release smoke test returned the Beholder depth-4 dependency query
in 0.69 seconds while automatic indexing was active. Under simultaneous Fresha
indexing and TypeScript enrichment, slower traversals exceeded the former
five-second deadline. While one such traversal was running, `beholder daemon
status` still answered in 0.46 seconds. Those contended measurements are not
used as the warm acceptance result above.

`context` now reads materialized dependencies and each structural observation
source through separate indexed queries with one shared typed result shape. For
the Fresha `string_prop` entity, the server span fell from 88.4 seconds before
this work to 420 ms warm after indexing completed. The remaining dominant stage
is the fact-shard structural lookup at 321 ms; every other database stage was
below 4.3 ms. Two consecutive warm CLI runs took 0.82 seconds each. The first
post-index request took 1.38 seconds wall-clock with a 1.24-second server span
while the 22 GB database cache warmed.

The branching reference used a 100,000-entity in-memory DAG with fanout 4 and
depth 4. Loading took 3.529 seconds; direct closure took 4.576 milliseconds,
trace 4.779 milliseconds, and impact 1.176 milliseconds. This benchmark uses
the synthetic benchmark rules, while focused diamond tests cover the production
frontier traversal and deterministic path behavior.

## Query plans and indexes

`EXPLAIN` showed two costly materializations:

- entity hydration joined every current shard selection before probing the
  requested entity;
- reverse override resolution materialized every current base and enrichment
  override before joining the frontier.

The retained indexes bind current state, repository, owner and override target
before reading historical facts:

| Index group | Build time | SQLite allocation |
| --- | ---: | ---: |
| fact-shard entity by ID | 15.60 s | 571,813,888 bytes |
| fact-shard selection by owner | 1.94 s | 88,023,040 bytes |
| fact-shard dependency by source | 59.42 s | 514,084,864 bytes |
| fact-shard observation by source | about 65 s | 2,511,532,032 bytes |
| revision state by state | 0.03 s | 4,096 bytes |
| four reverse override indexes | 0.92 s | 34,996,224 bytes |
| fact-shard observation by target | 109.75 s | 639,747 pages, or 2,620,403,712 bytes at 4 KiB/page |

The fact-shard observation by-source index had previously been rejected because
it made the old compound acquisition query slower at 3.55 seconds. The staged
context query can bind the requested source before validating its selected
shard, so the same index now cuts that branch from 7.1 seconds to 320-559 ms.
It remains intentionally limited to structural `context`; dependency traversal
uses the much narrower dependency-by-source index.

The narrower retained dependency-by-source index supports valid shards whose
observation source differs from the shard owner without requiring an entity
fact. `EXPLAIN` changed that lookup from a full `stored_mat_join` to an indexed
`stored_prefix_join`; warm lookup of 32 edges took 17-19 milliseconds.

A repository-keyed shard-selection index was also discarded after correcting
the owner/version index made direct entity-ID candidate validation faster and
preserved non-repository entity schemes. It took 1.97 seconds to build and used
88,162,304 bytes before being dropped.
