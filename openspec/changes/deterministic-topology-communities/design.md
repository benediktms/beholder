## Context

Canonical `WorkspaceTopology`, semantic edges, immutable snapshots, and durable index/enrichment jobs already exist; no community or derived-state model exists. See proposal.md and delta specs for behavior.

## Goals / Non-Goals

**Goals:** a rebuildable, evidence-preserving projection, empirically gated flat CPM partitions, separate derived storage/readiness, and non-blocking jobs.

**Non-Goals:** canonical mutation, semantic labels, hierarchy, query-time computation, Infomap/SBM adoption, or hand-written Leiden.

## Decisions

- `topology-analysis` owns algorithm-neutral DTOs and canonical serialization; dense IDs and backend types stay adapter-local.
- Identity hashes include source identity, versioned canonical JSON config, backend/runtime/target/seed/thread policy. Fixed seed alone is not a cross-platform reproducibility claim.
- Preserve directed layers/evidence; initial Leiden symmetrizes within layer. Participants, support-only nodes, omissions, relation layers, weights, self/parallel rules are serialized hypotheses, not semantic truth.
- Audit native Rust first with default features disabled and single-thread operation; use an oracle only for differential evaluation. No production dependency if audit/fixture/utility gates fail.
- Use separate SQLite derived state and one selected pointer per exact key; topology jobs reuse five-attempt/coalescing/currentness patterns but not enrichment payloads.

## Risks / Trade-offs

- [Immature backend or license] → B3 can conclude do-not-adopt without dependency.
- [Hubs/weights mislead] → fixture, negative-control, perturbation, and agent utility gates precede default selection.
- [Revision GC race] → supersede before acquisition; materialize snapshot input within an attempt.

## Migration Plan

B1/B2 establish pure contracts/projection; B3 gates the backend; B4 profiles stay flat; B5a then B5b add persistence/jobs. Canonical publication has no dependency on any stage; derived artifacts can be discarded independently.

## Projection appendix (extracted)

Projection identity hashes source identity, schema/config canonical JSON, implementation; algorithm identity also hashes backend/version/config/seed/target/runtime/policy/thread count. Participants are Callable, GraphqlOperation/Field, KafkaTopic, Rpc, Service, ProtoMessage/Service, UnityPrefab; namespace/data nodes are support-only, Unknown excluded. Tests/external are excluded and never bridges; generated explicit; repository metadata only. Layers: execution `calls,calls_graphql,calls_rpc`; binding `binds_contract,implements,implemented_by,resolved_by`; contract/data `publishes,consumed_by,selects,request_type,response_type,uses`; containment `defines,field_of,imports,requires`. Direction/evidence persist; initial Leiden symmetrizes layers. Hypothesis weights execution/binding 1.0, contract/data .6, containment .2; compare raw/log/inverse-sqrt hub normalization. Self edges remain manifest only; parallel support retained. Coarse/standard/fine are flat independent keys. Native Rust audit, fixed seed, single-thread provenance precede adoption; exact cross-platform reproducibility is measured. Derived state is WAL SQLite, immutable blobs, exact selected pointer, retention; unavailable source supersedes; publication never waits.

After backend eligibility, an offline injection adapter feeds B3 deterministic output into the frozen A1 harness. It changes no runtime/product interface and does not depend on C2 packs. C/D require its preregistered correctness, non-inferiority, misleading-interpretation, and utility pass. D4 later separately validates the integrated path; D4 failure prevents default enablement.
