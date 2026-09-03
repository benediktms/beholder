# Semantic query performance

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

An installed release smoke test returned the Beholder depth-4 dependency query
in 0.69 seconds while automatic indexing was active. Under simultaneous Fresha
indexing and TypeScript enrichment, slower traversals exceeded the former
five-second deadline. While one such traversal was running, `beholder daemon
status` still answered in 0.46 seconds. Those contended measurements are not
used as the warm acceptance result above.

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
| revision state by state | 0.03 s | 4,096 bytes |
| four reverse override indexes | 0.92 s | 34,996,224 bytes |
| fact-shard observation by target | 109.75 s | 639,747 pages, or 2,620,403,712 bytes at 4 KiB/page |

An experimental fact-shard observation by-source index was rejected: it took
92.74 seconds to build, added 2,511,532,032 bytes (26 percent of the original
database), and made acquisition slower at 3.55 seconds. It is not created by
Beholder and was dropped from the measurement database. SQLite retains freed
pages until an explicit reclaim, so the database file does not shrink merely by
dropping the index.

The narrower retained dependency-by-source index supports valid shards whose
observation source differs from the shard owner without requiring an entity
fact. `EXPLAIN` changed that lookup from a full `stored_mat_join` to an indexed
`stored_prefix_join`; warm lookup of 32 edges took 17-19 milliseconds.

A repository-keyed shard-selection index was also discarded after correcting
the owner/version index made direct entity-ID candidate validation faster and
preserved non-repository entity schemes. It took 1.97 seconds to build and used
88,162,304 bytes before being dropped.
