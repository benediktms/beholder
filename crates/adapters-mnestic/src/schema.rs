pub(super) const CREATE_SCHEMA: &str = r#"
:create state_observation {
    state: String,
    from: String,
    relation: String,
    to: String,
    =>
    evidence: String,
}
"#;

pub(super) const CREATE_DEPENDENCY_SCHEMA: &str = r#"
:create state_dependency_observation {
    state: String,
    from: String,
    relation: String,
    to: String,
    =>
    evidence: String,
}
"#;

pub(super) const CREATE_METADATA_SCHEMA: &str = r#"
:create state_observation_metadata {
    state: String,
    from: String,
    relation: String,
    to: String,
    =>
    confidence: Float,
    provenance: String,
}
"#;

pub(super) const CREATE_ENTITY_SCHEMA: &str = r#"
:create state_entity {
    state: String,
    id: String,
    =>
    kind: String,
    metadata: String,
}
"#;

pub(super) const CREATE_GRPC_BINDING_SCHEMA: &str = r#"
:create state_grpc_binding_candidate {
    state: String,
    local_symbol: String,
    role: String,
    service: String,
    method: String,
    evidence: String,
    =>
    cardinality: String,
    confidence: Float,
    provenance: String,
}
"#;

pub(super) const CREATE_REVISION_OBSERVATION_SCHEMA: &str = r#"
:create analysis_revision_observation {
    view: String,
    revision: Int,
    from: String,
    relation: String,
    to: String,
    evidence: String,
    =>
    confidence: Float,
    provenance: String,
}
"#;

pub(super) const CREATE_REVISION_ENTITY_SCHEMA: &str = r#"
:create analysis_revision_entity {
    view: String,
    revision: Int,
    id: String,
    =>
    kind: String,
    metadata: String,
}
"#;

pub(super) const CREATE_GRPC_DIAGNOSTIC_SCHEMA: &str = r#"
:create analysis_revision_grpc_diagnostic {
    view: String,
    revision: Int,
    local_symbol: String,
    role: String,
    service: String,
    method: String,
    evidence: String,
    =>
    code: String,
    detail: String,
}
"#;

pub(super) const CREATE_ANALYSIS_METADATA_SCHEMA: &str = r#"
:create analysis_revision_metadata {
    view: String,
    revision: Int,
    =>
    incomplete: Bool,
}
"#;

pub(super) const CREATE_ANALYSIS_DIAGNOSTIC_SCHEMA: &str = r#"
:create analysis_revision_diagnostic {
    view: String,
    revision: Int,
    repository: String,
    code: String,
    severity: String,
    path: String,
    line: Int,
    =>
    detail: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_SCHEMA: &str = r#"
:create analysis_revision_enrichment {
    view: String,
    revision: Int,
    analyzer: String,
    =>
    version: String,
}
"#;

pub(super) const CREATE_REVISION_INPUT_SCHEMA: &str = r#"
:create analysis_revision_input {
    view: String,
    revision: Int,
    repository: String,
    =>
    fingerprint: String,
}
"#;

pub(super) const CREATE_REVISION_CONTEXT_SCHEMA: &str = r#"
:create analysis_revision_context {
    view: String,
    revision: Int,
    target: String,
    analyzer: String,
    context: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_INPUT_SCHEMA: &str = r#"
:create analysis_revision_enrichment_input {
    view: String,
    revision: Int,
    repository: String,
    analyzer: String,
    =>
    fingerprint: String,
}
"#;

pub(super) const CREATE_REPOSITORY_ENRICHMENT_SCHEMA: &str = r#"
:create analysis_revision_repository_enrichment {
    view: String,
    revision: Int,
    owner: String,
    =>
    repository: String,
    analyzer: String,
    version: String,
    input_fingerprint: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_OVERRIDE_OWNER_SCHEMA: &str = r#"
:create analysis_revision_enrichment_override_owner {
    view: String,
    revision: Int,
    from: String,
    relation: String,
    unresolved_to: String,
    =>
    analyzer: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_DIAGNOSTIC_OWNER_SCHEMA: &str = r#"
:create analysis_revision_enrichment_diagnostic_owner {
    view: String,
    revision: Int,
    repository: String,
    code: String,
    severity: String,
    path: String,
    line: Int,
    =>
    analyzer: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_ENTITY_OWNER_SCHEMA: &str = r#"
:create analysis_revision_enrichment_entity_owner {
    view: String,
    revision: Int,
    id: String,
    =>
    analyzer: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_OBSERVATION_OWNER_SCHEMA: &str = r#"
:create analysis_revision_enrichment_observation_owner {
    view: String,
    revision: Int,
    from: String,
    relation: String,
    to: String,
    evidence: String,
    =>
    analyzer: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_OUTPUT_SCHEMA: &str = r#"
:create enrichment_output {
    view: String,
    owner: String,
    =>
    repository: String,
    analyzer: String,
    version: String,
    input_fingerprint: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_ENTITY_CONTRIBUTION_SCHEMA: &str = r#"
:create enrichment_entity_contribution {
    view: String,
    owner: String,
    id: String,
    =>
    kind: String,
    metadata: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_OBSERVATION_CONTRIBUTION_SCHEMA: &str = r#"
:create enrichment_observation_contribution {
    view: String,
    owner: String,
    from: String,
    relation: String,
    to: String,
    evidence: String,
    =>
    confidence: Float,
    provenance: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_OVERRIDE_CONTRIBUTION_SCHEMA: &str = r#"
:create enrichment_override_contribution {
    view: String,
    owner: String,
    from: String,
    relation: String,
    unresolved_to: String,
    =>
    resolved_to: String,
    evidence: String,
    confidence: Float,
    provenance: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_DIAGNOSTIC_CONTRIBUTION_SCHEMA: &str = r#"
:create enrichment_diagnostic_contribution {
    view: String,
    owner: String,
    repository: String,
    code: String,
    severity: String,
    path: String,
    line: Int,
    =>
    detail: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_ENTITY_SELECTION_SCHEMA: &str = r#"
:create analysis_enrichment_entity_selection {
    view: String,
    id: String,
    =>
    owner: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_OBSERVATION_SELECTION_SCHEMA: &str = r#"
:create analysis_enrichment_observation_selection {
    view: String,
    from: String,
    relation: String,
    to: String,
    evidence: String,
    =>
    owner: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_OVERRIDE_SELECTION_SCHEMA: &str = r#"
:create analysis_enrichment_override_selection {
    view: String,
    from: String,
    relation: String,
    unresolved_to: String,
    =>
    owner: String,
}
"#;

pub(super) const CREATE_ENRICHMENT_DIAGNOSTIC_SELECTION_SCHEMA: &str = r#"
:create analysis_enrichment_diagnostic_selection {
    view: String,
    repository: String,
    code: String,
    severity: String,
    path: String,
    line: Int,
    =>
    owner: String,
}
"#;

pub(super) const CREATE_BASELINE_ENTITY_SCHEMA: &str = r#"
:create analysis_baseline_entity {
    view: String,
    id: String,
    =>
    kind: String,
    metadata: String,
    revision_owned: Bool,
}
"#;

pub(super) const CREATE_BASELINE_OBSERVATION_SCHEMA: &str = r#"
:create analysis_baseline_observation {
    view: String,
    from: String,
    relation: String,
    to: String,
    evidence: String,
    =>
    confidence: Float,
    provenance: String,
    revision_owned: Bool,
}
"#;

pub(super) const CREATE_BASELINE_OVERRIDE_SCHEMA: &str = r#"
:create analysis_baseline_dependency_override {
    view: String,
    from: String,
    relation: String,
    unresolved_to: String,
    =>
    resolved_to: String,
    evidence: String,
    confidence: Float,
    provenance: String,
}
"#;

pub(super) const CREATE_BASELINE_DIAGNOSTIC_SCHEMA: &str = r#"
:create analysis_baseline_diagnostic {
    view: String,
    repository: String,
    code: String,
    severity: String,
    path: String,
    line: Int,
    =>
    detail: String,
}
"#;

pub(super) const CREATE_FACT_SHARD_SELECTION_SCHEMA: &str = r#"
:create analysis_fact_shard_selection {
    view: String,
    producer: String,
    repository: String,
    owner: String,
    =>
    version: String,
}
"#;

pub(super) const CREATE_FACT_SHARD_ENTITY_SCHEMA: &str = r#"
:create analysis_fact_shard_entity {
    producer: String,
    owner: String,
    version: String,
    id: String,
    =>
    kind: String,
    metadata: String,
}
"#;

pub(super) const CREATE_FACT_SHARD_OBSERVATION_SCHEMA: &str = r#"
:create analysis_fact_shard_observation {
    producer: String,
    owner: String,
    version: String,
    from: String,
    relation: String,
    to: String,
    evidence: String,
    =>
    confidence: Float,
    provenance: String,
}
"#;

pub(super) const CREATE_FACT_SHARD_DEPENDENCY_SCHEMA: &str = r#"
:create analysis_fact_shard_dependency_observation {
    producer: String,
    owner: String,
    version: String,
    from: String,
    relation: String,
    to: String,
    evidence: String,
}
"#;

pub(super) const CREATE_BASELINE_FINGERPRINT_SCHEMA: &str = r#"
:create analysis_baseline_fingerprint {
    view: String,
    =>
    fingerprint: String,
}
"#;

pub(super) const CREATE_SCHEMA_MIGRATION_SCHEMA: &str = r#"
:create schema_migration {
    name: String,
    =>
    version: Int,
}
"#;

pub(super) const CREATE_OBSERVATION_TO_INDEX: &str =
    "::index create state_observation:by_to {to, state, from, relation, evidence}";
pub(super) const CREATE_METADATA_TO_INDEX: &str = "::index create state_observation_metadata:by_to \
     {to, state, from, relation, confidence, provenance}";
pub(super) const CREATE_REVISION_OBSERVATION_TO_INDEX: &str = "::index create analysis_revision_observation:by_to \
     {view, revision, to, from, relation, evidence, confidence, provenance}";
pub(super) const CREATE_ENRICHMENT_OBSERVATION_SELECTION_TO_INDEX: &str = "::index create analysis_enrichment_observation_selection:by_to \
     {view, to, from, relation, evidence, owner}";

pub(super) const CREATE_OVERRIDE_SCHEMA: &str = r#"
:create analysis_revision_dependency_override {
    view: String,
    revision: Int,
    from: String,
    relation: String,
    unresolved_to: String,
    =>
    resolved_to: String,
    evidence: String,
}
"#;

pub(super) const CREATE_OVERRIDE_METADATA_SCHEMA: &str = r#"
:create analysis_revision_dependency_override_metadata {
    view: String,
    revision: Int,
    from: String,
    relation: String,
    unresolved_to: String,
    =>
    confidence: Float,
    provenance: String,
}
"#;

pub(super) const CREATE_REVISION_SCHEMA: &str = r#"
:create analysis_revision {
    view: String,
    =>
    revision: Int,
}
"#;

pub(super) const CREATE_FINGERPRINT_SCHEMA: &str = r#"
:create analysis_fingerprint {
    view: String,
    =>
    fingerprint: String,
}
"#;

pub(super) const CREATE_VERIFICATION_FINGERPRINT_SCHEMA: &str = r#"
:create analysis_verification_fingerprint {
    view: String,
    =>
    fingerprint: String,
}
"#;

pub(super) const CREATE_REPOSITORY_STATE_SCHEMA: &str = r#"
:create repository_state {
    fingerprint: String,
    =>
    repository: String,
    head: String,
}
"#;

pub(super) const CREATE_REPOSITORY_REVISION_SCHEMA: &str = r#"
:create repository_revision {
    repository: String,
    =>
    source_state: String,
    analyzed_state: String,
    analysis_identity: String,
    head: String,
    incomplete: Bool,
}
"#;

pub(super) const CREATE_REPOSITORY_REVISION_DIAGNOSTIC_SCHEMA: &str = r#"
:create repository_revision_diagnostic {
    repository: String,
    code: String,
    severity: String,
    path: String,
    line: Int,
    =>
    detail: String,
}
"#;

pub(super) const CREATE_GARBAGE_COLLECTION_STATE_SCHEMA: &str = r#"
:create garbage_collection_state {
    state: String,
    =>
    repository: String,
    head: String,
}
"#;

pub(super) const CREATE_REVISION_STATE_SCHEMA: &str = r#"
:create analysis_revision_state {
    view: String,
    revision: Int,
    repository: String,
    =>
    state: String,
}
"#;

pub(super) const SEED: &str = r#"
?[state, from, relation, to, evidence] <- [
    ['seed-main', 'web/CheckoutPage', 'uses', 'web/CheckoutQuery', 'CheckoutPage.tsx:12'],
    ['seed-main', 'web/CheckoutQuery', 'selects', 'graphql/Query.checkout', 'CheckoutQuery.graphql:2'],
    ['seed-main', 'graphql/Query.checkout', 'resolved_by', 'bff/CheckoutResolver.checkout', 'schema.ex:41'],
    ['seed-main', 'bff/CheckoutResolver.checkout', 'calls', 'rpc/Pricing.GetPrice', 'checkout_resolver.ex:28'],
    ['seed-main', 'rpc/Pricing.GetPrice', 'implemented_by', 'pricing/get_price', 'pricing.proto:9'],
    ['seed-main', 'pricing/get_price', 'publishes', 'topic/pricing.updated', 'get_price.rs:18'],
    ['seed-main', 'topic/pricing.updated', 'consumed_by', 'cache/update_price', 'consumer.rs:7'],
    ['seed-feature', 'rpc/Pricing.GetPrice', 'implemented_by', 'pricing/get_price_v2', 'pricing.proto:9'],
]
:put state_observation {state, from, relation, to => evidence}
"#;

pub(super) const SEED_DEPENDENCIES: &str = r#"
?[state, from, relation, to, evidence] :=
    *state_observation{state, from, relation, to, evidence}
:put state_dependency_observation {state, from, relation, to => evidence}
"#;

pub(super) const SEED_METADATA: &str = r#"
?[state, from, relation, to, confidence, provenance] :=
    *state_observation{state, from, relation, to},
    confidence = 1.0,
    provenance = 'ast'
:put state_observation_metadata {state, from, relation, to => confidence, provenance}
"#;

pub(super) const SEED_REVISIONS: &str = r#"
?[view, revision] <- [['main', 0], ['feature', 0]]
:put analysis_revision {view => revision}
"#;

pub(super) const SEED_STATES: &str = r#"
?[view, revision, repository, state] <- [
    ['main', 0, 'seed', 'seed-main'],
    ['feature', 0, 'seed', 'seed-feature'],
]
:put analysis_revision_state {view, revision, repository => state}
"#;

pub(super) const DIRECT_RULES: &str = include_str!("../../../rules/core/direct.datalog");
pub(super) const BASE_DIRECT_RULES: &str = include_str!("../../../rules/core/base_direct.datalog");
pub(super) const DEPENDENCY_RULES: &str = include_str!("../../../rules/core/dependencies.datalog");
pub(super) const IMPACT_RULES: &str = include_str!("../../../rules/core/impact.datalog");
pub(super) const CONTEXT_QUERY: &str = include_str!("../../../rules/core/context.datalog");
