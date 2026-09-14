## Purpose

Defines reproducible, evidence-preserving derived topology and flat structural community artifacts without changing the revision-consistent canonical semantic graph.

## ADDED Requirements

### Requirement: Derived artifacts cannot alter canonical facts
Topology projection, partitioning, retention, and failure handling SHALL not mutate canonical entities, observations, relationships, evidence, provenance, confidence, revisions, publication, or canonical query availability.

#### Scenario: Partition failure
- **WHEN** a projection or Leiden run fails
- **THEN** the canonical revision remains published and canonical queries remain usable

### Requirement: Complete derived identity and readiness
Projection, algorithm, and semantic-derived artifacts SHALL use complete content identities including source revision, versioned canonical configuration, implementation/backend/runtime/target/thread/seed policy as applicable, and expose `absent`, `queued`, `running`, `ready`, `failed`, or `superseded` separately from canonical readiness.

#### Scenario: Algorithm input changes
- **WHEN** a backend version, weight configuration, source revision, seed, or thread policy changes
- **THEN** the resulting artifact has a distinct key and is never mixed with another key

### Requirement: Deterministic evidence-preserving projection
The projection SHALL serialize sorted node IDs, edge tuples, evidence references, and configuration without map-order dependence; it SHALL preserve every canonical directed edge and relation kind in its manifest.

#### Scenario: Row permutation
- **WHEN** identical canonical rows arrive in a different order
- **THEN** projection bytes, keys, and supporting-edge lists are identical

### Requirement: Explicit projection participation and transformation
The projection SHALL record each input node and edge as optimization participant, support-only, or omitted with deterministic reason/count; it SHALL retain direction, relation, evidence, self-edge, and parallel-edge provenance when producing weighted algorithm edges.

#### Scenario: Excluded bridge candidate
- **WHEN** a test, external dependency, or unknown entity is excluded by configuration
- **THEN** it is not used to synthesize an optimization bridge and its omission is reported

### Requirement: Independent flat resolution partitions
Coarse, standard, and fine CPM profiles SHALL be independently keyed flat partitions with stable run-scoped membership IDs and no parent/child relation inferred from overlap or backend levels.

#### Scenario: Non-nested profiles
- **WHEN** a fine group overlaps two coarse groups
- **THEN** both partitions remain separate and no hierarchy edge is returned

### Requirement: Bounded separate retention
Derived artifacts SHALL be retained and evicted independently under bounded quota/age rules without deleting or changing canonical state.

#### Scenario: Retention pressure
- **WHEN** derived quota or age is exceeded
- **THEN** unselected derived artifacts are removed while canonical facts remain unchanged
