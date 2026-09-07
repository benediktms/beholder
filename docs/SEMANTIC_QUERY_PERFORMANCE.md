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

### Indexed entity-name search (2026-09-07)

The version-2 entity search stores each fact-shard entity's display name and
uses a name-leading index for exact and prefix candidates. On a disposable APFS
clone of the installed 19 GB database, `EXPLAIN` changed candidate acquisition
from a scan of selected entity shards to a `stored_prefix_join` on
`analysis_fact_shard_entity_name:by_name`, followed by the existing
`analysis_fact_shard_selection:by_owner` lookup. The retained plan contains no
primary selected-shard entity scan.

Creating the relation took 3 ms, backfilling selected versions took 7.334
seconds, and building the name index took 1.673 seconds. The relation plus index
allocated 136,920 4 KiB pages, or 560,824,320 bytes. Opening the same clone with
the production migration, including display-name calculation and its atomic
migration marker, took 26.448 seconds once and 6-8 ms thereafter.

Warm release measurements used summary diagnostics on the existing
seven-repository workspace:

| Query | Warm run 1 | Warm run 2 | Result |
| --- | ---: | ---: | --- |
| Entity search, `package`, limit 20 | 410 ms | 403 ms | 20 matches |
| One target repository, start already in target | 109 ms | 108 ms | 15 bounded paths |
| Two target repositories with no common path | 128 ms | 130 ms | No paths |

The single-target case does not run reverse reachability for the target already
visited by the start entity. The impossible two-target case performs bounded
reverse acquisition and stops before forward entity hydration; it does not
compute and discard unrelated paths.


## Multi-path traversal limits (2026-09-05)

The new `TraverseGraph` operation retains defaults of 8 hops and 50 paths, with
hard maxima of 32 hops and 200 paths. The indexed acquisition implementation and
representative Beholder/Fresha measurements above are reused. New in-memory,
unoptimized measurements use the production fact-shard publication and traversal
queries on an 18-layer DAG with two nodes per layer and every adjacent layer
fully connected. Setup/publication is excluded; timings include acquisition,
entity hydration, enumeration, and snapshot metadata.

| Request | Time | Result |
| --- | ---: | --- |
| 8 hops, 50 paths | 12.3 ms | 50 paths; depth/path truncation |
| 32 hops, 200 paths | 20.0 ms | 200 paths; path truncation |
| 32 hops, unreachable destination | 37.2 ms | No paths; work truncation |

These are synthetic local measurements, not a new installed-daemon Fresha run.
They support retaining the provisional hop/path limits while bounding exponential
search independently of the number of returned paths. Reproduce with:

```sh
cargo test -p beholder-adapters-mnestic --features sqlite --lib \
  store::tests::multipath_bounds_exponential_search_even_when_destination_is_missing -- --nocapture
```

Acquisition admits at most 10,000 evidence rows across indexed frontiers, plus one
probe row used to detect overflow. It drops the entire boundary edge if its
evidence would be partial, and marks unexpanded frontiers incomplete. A shared
five-second acquisition deadline also bounds database evaluation and sorting;
expiration returns a deadline error. Enumeration counts node visits, adjacency
inspection, and edge attempts against a 100,000-step ceiling. All limits are
reported in traversal metadata. No new indexes are needed.

Focused tests additionally cover SQLite publication between acquisition and
hydration, database interruption followed by a successful query on the same
transaction, evidence-preserving overrides, and a 10,001-edge star that cannot
be misreported as known leaf paths when acquisition is capped. Legacy query
defaults and the slow-warning policy are unchanged.
