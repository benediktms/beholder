use super::schema::*;
use beholder_dto::{
    AnalysisCompleteness, AnalysisDiagnostic, AnalysisDiagnosticSeverity, AnalysisMetadata,
    RepositoryRevision,
};
use mnestic_engine::{
    DataValue, DbInstance, MultiTransaction, NamedRows, ScriptMutability, ScriptRunOptions,
};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SEMANTIC_QUERY_WARNING_SECONDS: f64 = 5.0;
const SLOW_QUERY_SECONDS: f64 = 1.0;
const QUERY_PLAN_TIMEOUT_SECONDS: f64 = 0.25;
const ANALYSIS_METADATA_RULES: &str = include_str!("../../../rules/core/analysis_metadata.datalog");

pub(super) trait QueryRunner {
    fn run_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
    ) -> Result<NamedRows, Box<dyn Error>>;

    fn explain_query(
        &self,
        _script: &str,
        _params: BTreeMap<String, DataValue>,
        _timeout: f64,
    ) -> Result<Option<NamedRows>, Box<dyn Error>> {
        Ok(None)
    }
}

impl QueryRunner for DbInstance {
    fn run_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
    ) -> Result<NamedRows, Box<dyn Error>> {
        Ok(self.run_script_with_options(
            script,
            params,
            ScriptMutability::Immutable,
            ScriptRunOptions::new(),
        )?)
    }

    fn explain_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
        timeout: f64,
    ) -> Result<Option<NamedRows>, Box<dyn Error>> {
        Ok(Some(self.run_script_with_options(
            &format!("::explain {{ {script} }}"),
            params,
            ScriptMutability::Immutable,
            ScriptRunOptions::new().with_timeout(timeout),
        )?))
    }
}

impl QueryRunner for MultiTransaction {
    fn run_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
    ) -> Result<NamedRows, Box<dyn Error>> {
        Ok(self.run_script(script, params)?)
    }
}

pub(super) struct SnapshotQueryRunner<'a> {
    transaction: &'a MultiTransaction,
    explain_db: &'a DbInstance,
}

impl<'a> SnapshotQueryRunner<'a> {
    pub(super) fn new(transaction: &'a MultiTransaction, explain_db: &'a DbInstance) -> Self {
        Self {
            transaction,
            explain_db,
        }
    }
}

impl QueryRunner for SnapshotQueryRunner<'_> {
    fn run_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
    ) -> Result<NamedRows, Box<dyn Error>> {
        self.transaction.run_query(script, params)
    }

    fn explain_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
        timeout: f64,
    ) -> Result<Option<NamedRows>, Box<dyn Error>> {
        self.explain_db.explain_query(script, params, timeout)
    }
}

pub(super) fn warn_on_slow_semantic_query<T>(read: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = read();
    let elapsed = started.elapsed();
    if semantic_query_is_slow(elapsed) {
        tracing::warn!(
            elapsed_ms = elapsed.as_millis(),
            threshold_seconds = SEMANTIC_QUERY_WARNING_SECONDS,
            "semantic query exceeded warning threshold"
        );
    }
    result
}

fn semantic_query_is_slow(elapsed: Duration) -> bool {
    elapsed.as_secs_f64() >= SEMANTIC_QUERY_WARNING_SECONDS
}

struct QuerySpec<'a> {
    operation: &'static str,
    view: &'a str,
    script: &'a str,
}

impl<'a> QuerySpec<'a> {
    fn new(operation: &'static str, view: &'a str, script: &'a str) -> Self {
        Self {
            operation,
            view,
            script,
        }
    }
}

fn query(
    db: &impl QueryRunner,
    view: &str,
    script: &str,
    additions: impl IntoIterator<Item = (&'static str, DataValue)>,
) -> Result<NamedRows, Box<dyn Error>> {
    let mut params = BTreeMap::from([("view".into(), view.into())]);
    params.extend(
        additions
            .into_iter()
            .map(|(name, value)| (name.into(), value)),
    );
    db.run_query(script, params)
}

fn observed_query(
    db: &impl QueryRunner,
    spec: QuerySpec<'_>,
    additions: impl Fn() -> Vec<(&'static str, DataValue)>,
) -> Result<NamedRows, Box<dyn Error>> {
    let (result, elapsed, span_enabled) = {
        let span = tracing::info_span!(
            "db.query",
            otel.kind = "client",
            peer.service = "mnestic",
            db.system.name = "mnestic",
            db.namespace = spec.view,
            db.operation = spec.operation,
            db.rows = tracing::field::Empty,
            db.outcome = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
            otel.status_message = tracing::field::Empty,
        );
        let _entered = span.enter();
        let started = Instant::now();
        let result = query(db, spec.view, spec.script, additions());
        let elapsed = started.elapsed();
        span.record("db.outcome", if result.is_ok() { "ok" } else { "error" });
        if let Ok(rows) = &result {
            span.record("db.rows", rows.rows.len());
        } else if let Err(error) = &result {
            span.record("otel.status_code", "ERROR");
            span.record("otel.status_message", tracing::field::display(error));
        }
        (result, elapsed, !span.is_disabled())
    };
    if span_enabled && (result.is_err() || elapsed.as_secs_f64() >= SLOW_QUERY_SECONDS) {
        let span = tracing::info_span!(
            "db.query.explain",
            otel.kind = "client",
            peer.service = "mnestic",
            db.system.name = "mnestic",
            db.namespace = spec.view,
            db.operation = spec.operation,
            db.plan.outcome = tracing::field::Empty,
            db.plan.materialized_joins = tracing::field::Empty,
            db.plan.prefix_joins = tracing::field::Empty,
            db.plan.stored_loads = tracing::field::Empty,
            db.plan.loaded_relations = tracing::field::Empty,
        );
        let _entered = span.enter();
        let explain = || {
            let mut params = BTreeMap::from([("view".into(), spec.view.into())]);
            params.extend(
                additions()
                    .into_iter()
                    .map(|(name, value)| (name.into(), value)),
            );
            db.explain_query(spec.script, params, QUERY_PLAN_TIMEOUT_SECONDS)
        };
        let plan = explain();
        match plan {
            Ok(Some(plan)) => record_query_plan(&span, &plan),
            Ok(None) => {
                span.record("db.plan.outcome", "unsupported");
            }
            Err(_) => {
                span.record("db.plan.outcome", "error");
            }
        }
    }
    result
}

fn record_query_plan(span: &tracing::Span, plan: &NamedRows) {
    let Some(operation_column) = plan.headers.iter().position(|header| header == "op") else {
        span.record("db.plan.outcome", "invalid");
        return;
    };
    let reference_column = plan.headers.iter().position(|header| header == "ref");
    let mut materialized_joins = 0;
    let mut prefix_joins = 0;
    let mut stored_loads = 0;
    let mut loaded_relations = BTreeSet::new();
    for row in &plan.rows {
        match row[operation_column].get_str() {
            Some("stored_mat_join") => materialized_joins += 1,
            Some("stored_prefix_join") => prefix_joins += 1,
            Some(operation) if operation.starts_with("load_stored") => {
                stored_loads += 1;
                if let Some(relation) = reference_column.and_then(|column| row[column].get_str()) {
                    loaded_relations.insert(relation);
                }
            }
            _ => {}
        }
    }
    span.record("db.plan.outcome", "ok");
    span.record("db.plan.materialized_joins", materialized_joins);
    span.record("db.plan.prefix_joins", prefix_joins);
    span.record("db.plan.stored_loads", stored_loads);
    span.record(
        "db.plan.loaded_relations",
        loaded_relations.into_iter().collect::<Vec<_>>().join(","),
    );
}

pub(super) fn inspect_relations(db: &DbInstance) -> Result<NamedRows, Box<dyn Error>> {
    Ok(db.run_script("::relations", BTreeMap::new(), ScriptMutability::Immutable)?)
}

pub(super) fn inspect_revisions(db: &DbInstance) -> Result<NamedRows, Box<dyn Error>> {
    Ok(db.run_script(
        "revision_head[view, revision, repository, head] := \
             *analysis_revision_repository_head{view, revision, repository, head}\n\
         revision_head[view, revision, repository, head] := \
             *analysis_revision_state{view, revision, repository, state}, \
             *repository_state{fingerprint: state, repository, head}, \
             not *analysis_revision_repository_head{view, revision, repository}\n\
         ?[view, revision, fingerprint, repository, head, state] := \
             *analysis_revision{view, revision}, \
             *analysis_fingerprint{view, fingerprint}, \
             *analysis_revision_state{view, revision, repository, state}, \
             revision_head[view, revision, repository, head]\n\
         :order view, repository",
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )?)
}

pub(super) fn analysis_revision(db: &impl QueryRunner, view: &str) -> Result<u64, Box<dyn Error>> {
    let rows = query(
        db,
        view,
        "?[revision] := *analysis_revision{view: $view, revision}",
        [],
    )?;
    Ok(rows
        .rows
        .first()
        .and_then(|row| row[0].get_int())
        .unwrap_or_default()
        .try_into()?)
}

pub(super) fn analysis_metadata(
    db: &impl QueryRunner,
    view: &str,
    revision: u64,
) -> Result<AnalysisMetadata, Box<dyn Error>> {
    let revision = i64::try_from(revision)?;
    let rows = observed_query(
        db,
        QuerySpec::new(
            "analysis_metadata.completeness",
            view,
            "?[incomplete] := *analysis_revision_metadata{\
             view: $view, revision: $revision, incomplete\
         }",
        ),
        || vec![("revision", revision.into())],
    )?;
    let incomplete = rows
        .rows
        .first()
        .and_then(|row| match row.first() {
            Some(DataValue::Bool(value)) => Some(*value),
            _ => None,
        })
        .unwrap_or_default();
    let script = format!(
        "{ANALYSIS_METADATA_RULES}\n\
         baseline_diagnostic[repository, code, severity, path, line, detail] := \
             *analysis_revision_diagnostic{{\
                 view: $view, revision: $revision, repository, code, severity, path, line, detail\
             }}, \
             not selected_diagnostic_replacement[repository, code]\n\
         ?[repository, code, severity, path, line, detail] := \
             baseline_diagnostic[repository, code, severity, path, line, detail]\n\
         ?[repository, code, severity, path, line, detail] := \
             enrichment_diagnostic[repository, code, severity, path, line, detail], \
             not baseline_diagnostic[repository, code, severity, path, line, _]\n\
         :order severity, repository, path, line, code"
    );
    let rows = observed_query(
        db,
        QuerySpec::new("analysis_metadata.diagnostics", view, &script),
        || vec![("revision", revision.into())],
    )?;
    let diagnostics = rows
        .rows
        .into_iter()
        .map(|row| {
            let severity = match row[2].get_str() {
                Some("known_limitation") => AnalysisDiagnosticSeverity::KnownLimitation,
                Some("warning") => AnalysisDiagnosticSeverity::Warning,
                _ => return Err("unknown stored analysis diagnostic severity".into()),
            };
            let line = u32::try_from(row[4].get_int().unwrap_or_default())?;
            let detail = row[5].get_str().unwrap_or_default();
            Ok(AnalysisDiagnostic {
                repository: row[0].get_str().unwrap_or_default().into(),
                code: row[1].get_str().unwrap_or_default().into(),
                severity,
                path: PathBuf::from(row[3].get_str().unwrap_or_default()),
                line: (line != 0).then_some(line),
                detail: (!detail.is_empty()).then(|| detail.into()),
            })
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(AnalysisMetadata {
        completeness: if incomplete {
            AnalysisCompleteness::Incomplete
        } else {
            AnalysisCompleteness::Complete
        },
        diagnostics,
    })
}

pub(super) fn repository_revision(
    db: &impl QueryRunner,
    repository: &str,
) -> Result<Option<RepositoryRevision>, Box<dyn Error>> {
    let params = BTreeMap::from([("repository".into(), repository.into())]);
    let revision = db.run_query(
        "?[source_state, analysis_identity, head, incomplete] := \
             *repository_revision{repository: $repository, source_state, analysis_identity, head, incomplete}",
        params.clone(),
    )?;
    let Some(row) = revision.rows.first() else {
        return Ok(None);
    };
    let source_state = row[0]
        .get_str()
        .ok_or("stored repository source state is not a string")?
        .to_owned();
    let analysis_identity = row[1]
        .get_str()
        .ok_or("stored repository analysis identity is not a string")?
        .to_owned();
    let head = row[2]
        .get_str()
        .ok_or("stored repository head is not a string")?;
    let incomplete = match &row[3] {
        DataValue::Bool(value) => *value,
        _ => return Err("stored repository completeness is not a boolean".into()),
    };
    let diagnostics = db.run_query(
        "?[code, severity, path, line, detail] := \
             *repository_revision_diagnostic{repository: $repository, code, severity, path, line, detail}\n\
         :order severity, path, line, code",
        params,
    )?;
    let diagnostics = diagnostics
        .rows
        .into_iter()
        .map(|row| {
            let severity = match row[1].get_str() {
                Some("known_limitation") => AnalysisDiagnosticSeverity::KnownLimitation,
                Some("warning") => AnalysisDiagnosticSeverity::Warning,
                _ => return Err("unknown stored repository diagnostic severity".into()),
            };
            let line = u32::try_from(row[3].get_int().unwrap_or_default())?;
            let detail = row[4].get_str().unwrap_or_default();
            Ok(AnalysisDiagnostic {
                repository: repository.into(),
                code: row[0].get_str().unwrap_or_default().into(),
                severity,
                path: PathBuf::from(row[2].get_str().unwrap_or_default()),
                line: (line != 0).then_some(line),
                detail: (!detail.is_empty()).then(|| detail.into()),
            })
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(Some(RepositoryRevision {
        source_state,
        head: (!head.is_empty()).then(|| head.into()),
        analysis_identity,
        analysis: AnalysisMetadata {
            completeness: if incomplete {
                AnalysisCompleteness::Incomplete
            } else {
                AnalysisCompleteness::Complete
            },
            diagnostics,
        },
    }))
}

pub(super) fn published_repository_head(
    db: &impl QueryRunner,
    view: &str,
    repository: &str,
) -> Result<Option<String>, Box<dyn Error>> {
    let rows = query(
        db,
        view,
        "selected_head[head] := \
             *analysis_revision{view: $view, revision}, \
             *analysis_revision_repository_head{\
                 view: $view, revision, repository: $repository, head\
             }\n\
         selected_head[head] := \
             *analysis_revision{view: $view, revision}, \
             *analysis_revision_state{view: $view, revision, repository: $repository, state}, \
             *repository_state{fingerprint: state, repository: $repository, head}, \
             not *analysis_revision_repository_head{\
                 view: $view, revision, repository: $repository\
             }\n\
         ?[head] := selected_head[head]",
        [("repository", repository.into())],
    )?;
    Ok(rows
        .rows
        .first()
        .and_then(|row| row[0].get_str())
        .filter(|head| !head.is_empty())
        .map(str::to_owned))
}

pub(super) fn entity_facts(
    db: &impl QueryRunner,
    view: &str,
    entities: &BTreeSet<String>,
) -> Result<NamedRows, Box<dyn Error>> {
    let requested = || {
        DataValue::List(
            entities
                .iter()
                .map(|id| DataValue::List(vec![id.as_str().into()]))
                .collect(),
        )
    };
    let mut baseline = observed_query(
        db,
        QuerySpec::new(
            "entity_facts.baseline",
            view,
            "requested[id] <- $entities\n\
         selected_state[state] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_state{view: $view, revision, state}\n\
         ?[id, kind, metadata] := requested[id], selected_state[state], \
             *state_entity{state, id, kind, metadata}\n\
         ?[id, kind, metadata] := requested[id], *analysis_revision{view: $view, revision}, \
             *analysis_revision_entity{view: $view, revision, id, kind, metadata}\n\
         :order id",
        ),
        || vec![("entities", requested())],
    )?;
    let mut baseline_ids = baseline
        .rows
        .iter()
        .filter_map(|row| row[0].get_str().map(str::to_owned))
        .collect::<BTreeSet<_>>();

    let nested = observed_query(
        db,
        QuerySpec::new(
            "entity_facts.shard",
            view,
            "requested[id] <- $entities\n\
         ?[id, kind, metadata] := requested[id], \
             *analysis_fact_shard_entity:by_id{id, producer, owner, version, kind, metadata}, \
             *analysis_fact_shard_selection:by_owner{\
                 view: $view, owner, producer, version\
             }\n\
         :order id\n\
         :reorder written",
        ),
        || {
            vec![(
                "entities",
                DataValue::List(
                    entities
                        .iter()
                        .filter(|id| !baseline_ids.contains(*id))
                        .map(|id| DataValue::List(vec![id.as_str().into()]))
                        .collect(),
                ),
            )]
        },
    )?;
    baseline_ids.extend(
        nested
            .rows
            .iter()
            .filter_map(|row| row[0].get_str().map(str::to_owned)),
    );
    baseline.rows.extend(nested.rows);

    let enrichment = observed_query(
        db,
        QuerySpec::new(
            "entity_facts.enrichment",
            view,
            "requested[id] <- $entities\n\
         selected_enrichment[owner] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_repository_enrichment{view: $view, revision, owner}\n\
         ?[id, kind, metadata] := requested[id], \
             *analysis_enrichment_entity_selection{view: $view, id, owner}, \
             selected_enrichment[owner], \
             *enrichment_entity_contribution{view: $view, owner, id, kind, metadata}\n\
         :order id",
        ),
        || vec![("entities", requested())],
    )?;
    baseline
        .rows
        .extend(enrichment.rows.into_iter().filter(|row| {
            row[0]
                .get_str()
                .is_some_and(|id| !baseline_ids.contains(id))
        }));
    Ok(baseline)
}

pub(super) fn inspect_observations(
    db: &DbInstance,
    relation: Option<&str>,
) -> Result<NamedRows, Box<dyn Error>> {
    let (filter, params) = match relation {
        Some(relation) => (
            ", relation == $relation",
            BTreeMap::from([("relation".into(), relation.into())]),
        ),
        None => ("", BTreeMap::new()),
    };
    Ok(db.run_script(
		&format!(
			"observation[state, from, relation, to, evidence, confidence, provenance] := \
                 *state_observation{{state, from, relation, to, evidence}}, \
			     *state_observation_metadata{{state, from, relation, to, confidence, provenance}}\n\
			 observation[state, from, relation, to, evidence, confidence, provenance] := \
			     *analysis_fact_shard_observation{{version: state, from, relation, to, evidence, confidence, provenance}}\n\
			 ?[state, from, relation, to, evidence, confidence, provenance] := \
			     observation[state, from, relation, to, evidence, confidence, provenance]{filter}\n\
             :order relation, from, to"
		),
        params,
        ScriptMutability::Immutable,
    )?)
}

pub(super) fn inspect_grpc_bindings(db: &DbInstance) -> Result<NamedRows, Box<dyn Error>> {
    Ok(db.run_script(
        "selected[view, revision, state, local_symbol, role, service, method, cardinality, evidence, confidence, provenance] := \
             *analysis_revision{view, revision}, \
             *analysis_revision_state{view, revision, state}, \
             *state_grpc_binding_candidate{\
                 state, local_symbol, role, service, method, evidence, cardinality, confidence, provenance\
             }\n\
         diagnostic[view, revision, local_symbol, role, service, method, evidence, code, detail] := \
             *analysis_revision_grpc_diagnostic{\
                 view, revision, local_symbol, role, service, method, evidence, code, detail\
             }\n\
         ?[view, local_symbol, role, service, method, cardinality, evidence, confidence, provenance, status, code, detail] := \
             selected[view, revision, _, local_symbol, role, service, method, cardinality, evidence, confidence, provenance], \
             diagnostic[view, revision, local_symbol, role, service, method, evidence, code, detail], \
             status = 'unmatched'\n\
         ?[view, local_symbol, role, service, method, cardinality, evidence, confidence, provenance, status, code, detail] := \
             selected[view, revision, _, local_symbol, role, service, method, cardinality, evidence, confidence, provenance], \
             not diagnostic[view, revision, local_symbol, role, service, method, evidence, _, _], \
             status = 'resolved', code = '', detail = ''\n\
         :order view, service, method, role, local_symbol",
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )?)
}

pub(super) fn context(
    db: &impl QueryRunner,
    view: &str,
    entity: &str,
) -> Result<NamedRows, Box<dyn Error>> {
    observed_query(db, QuerySpec::new("context", view, CONTEXT_QUERY), || {
        vec![("entity", entity.into())]
    })
}

pub(super) fn trace(
    db: &impl QueryRunner,
    view: &str,
    from: &str,
    _to: &str,
    max_hops: u32,
) -> Result<NamedRows, Box<dyn Error>> {
    closure(db, view, from, max_hops, TraversalDirection::Outgoing)
}

pub(super) fn impact(
    db: &impl QueryRunner,
    view: &str,
    entity: &str,
    max_hops: u32,
) -> Result<NamedRows, Box<dyn Error>> {
    closure(db, view, entity, max_hops, TraversalDirection::Incoming)
}

pub(super) fn dependencies(
    db: &impl QueryRunner,
    view: &str,
    entity: &str,
    max_hops: u32,
) -> Result<NamedRows, Box<dyn Error>> {
    closure(db, view, entity, max_hops, TraversalDirection::Outgoing)
}

#[derive(Clone, Copy)]
enum TraversalDirection {
    Outgoing,
    Incoming,
}

fn closure(
    db: &impl QueryRunner,
    view: &str,
    entity: &str,
    max_hops: u32,
    direction: TraversalDirection,
) -> Result<NamedRows, Box<dyn Error>> {
    let (core_rules, edge_rules, operations, boundary_operations, reorder) = match direction {
        TraversalDirection::Outgoing => (
            OUTGOING_DEPENDENCY_RULES,
            vec!["selected_edge"],
            vec!["traversal.outgoing.core"],
            vec!["traversal.outgoing.boundary.core"],
            ":reorder written",
        ),
        TraversalDirection::Incoming => (
            INCOMING_DEPENDENCY_RULES,
            vec![
                "state_edge",
                "base_override_edge",
                "revision_edge",
                "state_enrichment_override_edge",
                "revision_enrichment_override_edge",
                "enrichment_observation_edge",
                "grpc_implementation_edge",
            ],
            vec![
                "traversal.incoming.state",
                "traversal.incoming.base_override",
                "traversal.incoming.revision",
                "traversal.incoming.state_enrichment_override",
                "traversal.incoming.revision_enrichment_override",
                "traversal.incoming.enrichment_observation",
                "traversal.incoming.grpc_implementation",
            ],
            vec![
                "traversal.incoming.boundary.state",
                "traversal.incoming.boundary.base_override",
                "traversal.incoming.boundary.revision",
                "traversal.incoming.boundary.state_enrichment_override",
                "traversal.incoming.boundary.revision_enrichment_override",
                "traversal.incoming.boundary.enrichment_observation",
                "traversal.incoming.boundary.grpc_implementation",
            ],
            ":reorder written",
        ),
    };
    let scripts = edge_rules
        .iter()
        .map(|edge_rule| {
            format!(
                "{core_rules}\n\
                 frontier[id] <- $frontier\n\
                 ?[row_kind, entity, hops, edge_from, edge_to, relation, evidence, confidence, provenance] := \
                     {edge_rule}[edge_from, edge_to, relation, evidence, confidence, provenance], \
                     row_kind = 'edge', entity = '', hops = 0\n\
                 :order edge_from, edge_to, relation\n\
                 {reorder}"
            )
        })
        .collect::<Vec<_>>();
    let (
        shard_rules,
        shard_operation,
        shard_boundary_operation,
        override_rules,
        override_operation,
    ) = match direction {
        TraversalDirection::Outgoing => (
            OUTGOING_FACT_SHARD_DEPENDENCY_RULES,
            "traversal.outgoing.fact_shard",
            "traversal.outgoing.boundary.fact_shard",
            OUTGOING_DEPENDENCY_OVERRIDE_QUERY,
            "traversal.outgoing.overrides",
        ),
        TraversalDirection::Incoming => (
            INCOMING_FACT_SHARD_DEPENDENCY_RULES,
            "traversal.incoming.fact_shard",
            "traversal.incoming.boundary.fact_shard",
            INCOMING_DEPENDENCY_OVERRIDE_QUERY,
            "traversal.incoming.overrides",
        ),
    };
    let shard_script = format!(
        "{shard_rules}\n\
         frontier[id] <- $frontier\n\
         ?[row_kind, entity, hops, edge_from, edge_to, relation, evidence, confidence, provenance] := \
             shard_edge[edge_from, edge_to, relation, evidence, confidence, provenance], \
             row_kind = 'edge', entity = '', hops = 0\n\
         :order edge_from, edge_to, relation\n\
         :reorder written"
    );
    let override_script = format!(
        "{override_rules}\n\
         frontier[id] <- $frontier"
    );
    let override_key_script = format!(
        "{core_rules}\n\
         frontier[id] <- $frontier\n\
         ?[kind, from, relation, unresolved_to, resolved_to, confidence, provenance] := \
             base_override[from, relation, unresolved_to, resolved_to, _, confidence, provenance], \
             kind = 'base'\n\
         ?[kind, from, relation, unresolved_to, resolved_to, confidence, provenance] := \
             enrichment_override[\
                 from, relation, unresolved_to, resolved_to, _, confidence, provenance\
             ], \
             kind = 'enrichment'"
    );
    let boundary_projection = match direction {
        TraversalDirection::Outgoing => {
            "\
             ?[row_kind, entity, hops, edge_from, edge_to, relation, evidence, confidence, provenance] := \
                 boundary_edge[edge_from, edge_to, relation, evidence, confidence, provenance], \
                 frontier[edge_from], not visited[edge_to], \
                 row_kind = 'edge', entity = '', hops = 0"
        }
        TraversalDirection::Incoming => {
            "\
             special[edge_from, edge_to, relation] := \
                 boundary_edge[edge_from, edge_to, relation, _, _, _], \
                 relation = 'implemented_by', starts_with(edge_from, 'grpc://')\n\
             ?[row_kind, entity, hops, edge_from, edge_to, relation, evidence, confidence, provenance] := \
                 boundary_edge[edge_from, edge_to, relation, evidence, confidence, provenance], \
                 frontier[edge_to], not special[edge_from, edge_to, relation], \
                 not visited[edge_from], row_kind = 'edge', entity = '', hops = 0\n\
             ?[row_kind, entity, hops, edge_from, edge_to, relation, evidence, confidence, provenance] := \
                 boundary_edge[edge_from, edge_to, relation, evidence, confidence, provenance], \
                 special[edge_from, edge_to, relation], frontier[edge_from], \
                 not visited[edge_to], row_kind = 'edge', entity = '', hops = 0"
        }
    };
    let shard_not_overridden = match direction {
        TraversalDirection::Outgoing => "not dependency_override[from, relation, to, _, _, _, _]",
        TraversalDirection::Incoming => "not dependency_override_key[from, relation, to]",
    };
    let mut boundary_scripts = edge_rules
        .iter()
        .map(|edge_rule| {
            format!(
                "{core_rules}\n\
             boundary_edge[from, to, relation, evidence, confidence, provenance] := \
                 {edge_rule}[from, to, relation, evidence, confidence, provenance]\n\
             frontier[id] <- $frontier\n\
             visited[id] <- $visited\n\
             {boundary_projection}\n\
             :limit 1"
            )
        })
        .collect::<Vec<_>>();
    boundary_scripts.push(format!(
        "{core_rules}\n{shard_rules}\n\
             boundary_edge[from, to, relation, evidence, confidence, provenance] := \
                 shard_edge[from, to, relation, evidence, confidence, provenance], \
                 {shard_not_overridden}\n\
             frontier[id] <- $frontier\n\
             visited[id] <- $visited\n\
             {boundary_projection}\n\
             :limit 1"
    ));
    let mut boundary_operations = boundary_operations;
    boundary_operations.push(shard_boundary_operation);
    let mut frontier = BTreeSet::from([entity.to_owned()]);
    let mut visited = frontier.clone();
    let mut rows = Vec::new();
    let mut headers = Vec::new();
    for hops in 0..=max_hops {
        let hop_span = tracing::info_span!(
            "graph.traversal.hop",
            graph.direction = match direction {
                TraversalDirection::Outgoing => "outgoing",
                TraversalDirection::Incoming => "incoming",
            },
            graph.hop = hops,
            graph.frontier_size = frontier.len(),
        );
        let _entered = hop_span.enter();
        let values = DataValue::List(
            frontier
                .iter()
                .map(|entity| DataValue::List(vec![entity.as_str().into()]))
                .collect(),
        );
        let prefetched_shard = if matches!(direction, TraversalDirection::Incoming) {
            Some(observed_query(
                db,
                QuerySpec::new(shard_operation, view, &shard_script),
                || vec![("frontier", values.clone())],
            )?)
        } else {
            None
        };
        let fact_shard_baseline = prefetched_shard
            .as_ref()
            .map(|result| fact_shard_baseline(&result.rows));
        let (dependency_overrides, enrichment_overrides) =
            if matches!(direction, TraversalDirection::Incoming) {
                let candidates = observed_query(
                    db,
                    QuerySpec::new(
                        "traversal.incoming.override_candidates",
                        view,
                        &override_key_script,
                    ),
                    || {
                        vec![
                            ("frontier", values.clone()),
                            (
                                "fact_shard_baseline",
                                fact_shard_baseline
                                    .clone()
                                    .unwrap_or(DataValue::List(Vec::new())),
                            ),
                            ("dependency_overrides", DataValue::List(Vec::new())),
                            ("enrichment_overrides", DataValue::List(Vec::new())),
                        ]
                    },
                )?;
                let (keys, enrichments) = override_parameters(candidates.rows);
                (Some(keys), Some(enrichments))
            } else {
                (None, None)
            };
        let enrichment_overrides_empty = enrichment_overrides
            .as_ref()
            .is_some_and(|overrides| matches!(overrides, DataValue::List(rows) if rows.is_empty()));
        if hops == max_hops {
            let visited_values = DataValue::List(
                visited
                    .iter()
                    .map(|entity| DataValue::List(vec![entity.as_str().into()]))
                    .collect(),
            );
            for (operation, script) in boundary_operations.iter().copied().zip(&boundary_scripts) {
                if enrichment_overrides_empty && operation.contains("enrichment_override") {
                    continue;
                }
                let mut result =
                    observed_query(db, QuerySpec::new(operation, view, script), || {
                        let mut additions = vec![
                            ("frontier", values.clone()),
                            ("visited", visited_values.clone()),
                        ];
                        if let Some(baseline) = &fact_shard_baseline {
                            additions.push(("fact_shard_baseline", baseline.clone()));
                        }
                        if let Some(overrides) = &dependency_overrides {
                            additions.push(("dependency_overrides", overrides.clone()));
                        }
                        if let Some(overrides) = &enrichment_overrides {
                            additions.push(("enrichment_overrides", overrides.clone()));
                        }
                        additions
                    })?;
                if headers.is_empty() {
                    headers = result.headers;
                }
                if let Some(mut row) = result.rows.pop() {
                    row[2] = i64::from(hops).into();
                    rows.push(row);
                    break;
                }
            }
            break;
        }
        let mut edge_groups = if let Some(shard) = prefetched_shard {
            let baseline = fact_shard_baseline.unwrap_or(DataValue::List(Vec::new()));
            let mut groups = Vec::new();
            for (operation, script) in operations.iter().copied().zip(&scripts) {
                if enrichment_overrides_empty && operation.contains("enrichment_override") {
                    groups.push(Vec::new());
                    continue;
                }
                let core = observed_query(db, QuerySpec::new(operation, view, script), || {
                    vec![
                        ("frontier", values.clone()),
                        ("fact_shard_baseline", baseline.clone()),
                        (
                            "dependency_overrides",
                            dependency_overrides
                                .clone()
                                .unwrap_or(DataValue::List(Vec::new())),
                        ),
                        (
                            "enrichment_overrides",
                            enrichment_overrides
                                .clone()
                                .unwrap_or(DataValue::List(Vec::new())),
                        ),
                    ]
                })?;
                if headers.is_empty() {
                    headers = core.headers;
                }
                groups.push(core.rows);
            }
            groups.push(shard.rows);
            groups
        } else {
            let mut groups = Vec::new();
            for (operation, script) in operations.iter().copied().zip(&scripts) {
                let result = observed_query(db, QuerySpec::new(operation, view, script), || {
                    vec![("frontier", values.clone())]
                })?;
                if headers.is_empty() {
                    headers = result.headers;
                }
                groups.push(result.rows);
            }
            let shard = observed_query(
                db,
                QuerySpec::new(shard_operation, view, &shard_script),
                || vec![("frontier", values.clone())],
            )?;
            groups.push(shard.rows);
            groups
        };
        let overrides = observed_query(
            db,
            QuerySpec::new(override_operation, view, &override_script),
            || vec![("frontier", values.clone())],
        )?;
        let shard_group = edge_groups.last_mut().expect("shard query always runs");
        merge_fact_shard_overrides(shard_group, overrides.rows, direction, &frontier);
        let mut result_rows = edge_groups.into_iter().flatten().collect::<Vec<_>>();
        for row in &mut result_rows {
            row[2] = i64::from(hops).into();
        }
        let mut next = BTreeSet::new();
        for row in &result_rows {
            let from = row[3].get_str().unwrap_or_default();
            let to = row[4].get_str().unwrap_or_default();
            let relation = row[5].get_str().unwrap_or_default();
            let entity = match direction {
                TraversalDirection::Outgoing => to,
                TraversalDirection::Incoming
                    if relation == "implemented_by" && from.starts_with("grpc://") =>
                {
                    to
                }
                TraversalDirection::Incoming => from,
            };
            if visited.insert(entity.to_owned()) {
                next.insert(entity.to_owned());
            }
        }
        rows.extend(result_rows);
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(NamedRows::new(headers, rows))
}

fn fact_shard_baseline(rows: &[Vec<DataValue>]) -> DataValue {
    DataValue::List(
        rows.iter()
            .map(|row| {
                DataValue::List(vec![
                    row[3].clone(),
                    row[5].clone(),
                    row[4].clone(),
                    row[6].clone(),
                ])
            })
            .collect(),
    )
}

fn override_parameters(rows: Vec<Vec<DataValue>>) -> (DataValue, DataValue) {
    let mut keys = Vec::with_capacity(rows.len());
    let mut enrichments = Vec::new();
    for row in rows {
        keys.push(DataValue::List(vec![
            row[1].clone(),
            row[2].clone(),
            row[3].clone(),
        ]));
        if row[0].get_str() == Some("enrichment") {
            enrichments.push(DataValue::List(row[1..].to_vec()));
        }
    }
    (DataValue::List(keys), DataValue::List(enrichments))
}

fn merge_fact_shard_overrides(
    shard_edges: &mut Vec<Vec<DataValue>>,
    overrides: Vec<Vec<DataValue>>,
    direction: TraversalDirection,
    frontier: &BTreeSet<String>,
) {
    let original = std::mem::take(shard_edges);
    let overridden = overrides
        .iter()
        .map(|row| edge_key(row, 1))
        .collect::<BTreeSet<_>>();
    shard_edges.extend(
        original
            .iter()
            .filter(|row| !overridden.contains(&edge_key(row, 4)))
            .cloned(),
    );
    for mut override_ in overrides {
        let resolved_is_frontier = override_[4]
            .get_str()
            .is_some_and(|resolved| frontier.contains(resolved));
        match (override_[0].get_str(), direction) {
            (Some("base_override"), TraversalDirection::Outgoing) => {
                override_[0] = "edge".into();
                override_[1] = "".into();
                shard_edges.push(override_);
            }
            (Some("base_override"), TraversalDirection::Incoming) if resolved_is_frontier => {
                override_[0] = "edge".into();
                override_[1] = "".into();
                shard_edges.push(override_);
            }
            (Some("enrichment_override"), TraversalDirection::Outgoing) => {
                let key = edge_key(&override_, 1);
                for row in original.iter().filter(|row| edge_key(row, 4) == key) {
                    let mut resolved = override_.clone();
                    resolved[0] = "edge".into();
                    resolved[1] = "".into();
                    resolved[6] = row[6].clone();
                    shard_edges.push(resolved);
                }
            }
            (Some("enrichment_override"), TraversalDirection::Incoming) if resolved_is_frontier => {
                override_[0] = "edge".into();
                override_[1] = "".into();
                shard_edges.push(override_);
            }
            _ => {}
        }
    }
}

fn edge_key(row: &[DataValue], target: usize) -> (String, String, String) {
    (
        row[3].get_str().unwrap_or_default().to_owned(),
        row[5].get_str().unwrap_or_default().to_owned(),
        row[target].get_str().unwrap_or_default().to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::DIRECT_RULES;
    use crate::{SemanticStore, database::memory_database};
    use beholder_domain::{
        DependencyRelation, LogicalRepository, Observation, RepositoryFacts, RepositoryState,
        StructuralRelation, WorkspaceView,
    };
    use mnestic_engine::ScriptMutability;
    use std::{cell::Cell, collections::BTreeSet, fs, time::Duration, time::SystemTime};

    struct DisabledPlanRunner(Cell<bool>);

    impl QueryRunner for DisabledPlanRunner {
        fn run_query(
            &self,
            _script: &str,
            _params: BTreeMap<String, DataValue>,
        ) -> Result<NamedRows, Box<dyn Error>> {
            Err("query failed".into())
        }

        fn explain_query(
            &self,
            _script: &str,
            _params: BTreeMap<String, DataValue>,
            _timeout: f64,
        ) -> Result<Option<NamedRows>, Box<dyn Error>> {
            self.0.set(true);
            Ok(None)
        }
    }

    #[test]
    fn semantic_query_warns_at_five_seconds() {
        assert!(!semantic_query_is_slow(Duration::from_millis(4_999)));
        assert!(semantic_query_is_slow(Duration::from_secs(5)));
    }

    #[test]
    fn disabled_query_span_skips_plan_collection() {
        let runner = DisabledPlanRunner(Cell::new(false));
        let parameter_builds = Cell::new(0);

        let _ = observed_query(
            &runner,
            QuerySpec::new("test", "main", "?[value] <- [[1]]"),
            || {
                parameter_builds.set(parameter_builds.get() + 1);
                Vec::new()
            },
        );

        assert!(!runner.0.get());
        assert_eq!(parameter_builds.get(), 1);
    }

    #[test]
    fn snapshot_queries_can_be_explained() {
        let db = memory_database().unwrap();
        let transaction = db.multi_transaction(false);
        let query_runner = SnapshotQueryRunner::new(&transaction, &db);
        let plan = query_runner
            .explain_query(
                "?[value] <- [[1]]",
                BTreeMap::new(),
                QUERY_PLAN_TIMEOUT_SECONDS,
            )
            .unwrap()
            .unwrap();
        transaction.abort().unwrap();

        assert!(plan.headers.iter().any(|header| header == "op"));
    }

    fn facts(view: &WorkspaceView, observations: Vec<Observation>) -> RepositoryFacts {
        RepositoryFacts {
            state: view.repository_states[0].clone(),
            analysis_identity: "analysis".into(),
            incomplete: false,
            diagnostics: Vec::new(),
            entities: Vec::new(),
            grpc_bindings: Vec::new(),
            observations,
        }
    }
    #[test]
    fn impact_traverses_dependants() {
        let store = SemanticStore::memory().unwrap();
        let result = store
            .impact(
                "main",
                "rpc/Pricing.GetPrice",
                beholder_dto::DEFAULT_MAX_HOPS,
            )
            .unwrap();
        assert!(
            result
                .affected
                .iter()
                .any(|value| value.entity == "web/CheckoutPage")
        );
        assert!(
            !result
                .affected
                .iter()
                .any(|value| value.entity == "pricing/get_price")
        );
        let limited = store.impact("main", "rpc/Pricing.GetPrice", 1).unwrap();
        assert!(limited.traversal.truncated);
        assert!(limited.affected.iter().all(|value| value.hops == 1));
    }

    #[test]
    fn trace_chooses_a_string_predecessor() {
        let db = memory_database().unwrap();
        db.run_script(
            "?[state, from, relation, to, evidence] <- [
                    ['diamond-state', 'start', 'calls', 'left', 'left:1'],
                    ['diamond-state', 'start', 'calls', 'right', 'right:1'],
                    ['diamond-state', 'left', 'calls', 'end', 'left:2'],
                    ['diamond-state', 'right', 'calls', 'end', 'right:2'],
                    ['diamond-state', 'before', 'calls', 'start', 'before:1'],
                    ['diamond-state', 'end', 'calls', 'far', 'far:1'],
                 ]
                 :put state_dependency_observation {state, from, relation, to => evidence}",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .unwrap();
        db.run_script(
            "?[state, from, relation, to, confidence, provenance] :=
                     *state_dependency_observation{state, from, relation, to},
                     confidence = 1.0, provenance = 'ast'
                 :put state_observation_metadata {
                     state, from, relation, to => confidence, provenance
                 }",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .unwrap();
        db.run_script(
            "?[view, revision] <- [['diamond', 0]]
                 :put analysis_revision {view => revision}",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .unwrap();
        db.run_script(
            "?[view, revision, repository, state] <- [
                    ['diamond', 0, 'diamond', 'diamond-state']
                 ]
                 :put analysis_revision_state {view, revision, repository => state}",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .unwrap();

        let result = crate::semantic::trace(
            "diamond",
            "start",
            "end",
            beholder_dto::DEFAULT_MAX_HOPS,
            crate::inspection::inspection_result(
                trace(
                    &db,
                    "diamond",
                    "start",
                    "end",
                    beholder_dto::DEFAULT_MAX_HOPS,
                )
                .unwrap(),
            ),
            crate::inspection::InspectionResult {
                headers: Vec::new(),
                rows: Vec::new(),
                next: None,
            },
        )
        .unwrap();
        assert_eq!(result.paths[0].nodes, ["start", "left", "end"]);

        let boundary = dependencies(&db, "diamond", "start", 0).unwrap();
        assert_eq!(boundary.rows.len(), 1);

        let limited_rows = trace(&db, "diamond", "start", "end", 1).unwrap();
        assert!(
            limited_rows
                .rows
                .iter()
                .all(|row| row[3].get_str() != Some("end"))
        );
        let limited_impact = impact(&db, "diamond", "end", 1).unwrap();
        assert!(
            limited_impact
                .rows
                .iter()
                .all(|row| row[3].get_str() != Some("before"))
        );
        let limited = crate::semantic::trace(
            "diamond",
            "start",
            "end",
            1,
            crate::inspection::inspection_result(limited_rows),
            crate::inspection::InspectionResult {
                headers: Vec::new(),
                rows: Vec::new(),
                next: None,
            },
        )
        .unwrap();
        assert!(limited.paths.is_empty());
        assert!(limited.traversal.truncated);
    }

    #[test]
    fn typed_trace_deduplicates_graph_and_resolves_path_references() {
        let result = SemanticStore::memory()
            .unwrap()
            .trace(
                "main",
                "web/CheckoutPage",
                "cache/update_price",
                beholder_dto::DEFAULT_MAX_HOPS,
            )
            .unwrap();
        let node_ids = result
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<BTreeSet<_>>();
        let edge_ids = result
            .edges
            .iter()
            .map(|edge| edge.id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(node_ids.len(), result.nodes.len());
        assert_eq!(edge_ids.len(), result.edges.len());
        assert_eq!(result.nodes[0].kind, beholder_dto::EntityKind::Callable);
        assert!(result.edges.iter().all(|edge| {
            edge.confidence == 1.0
                && edge
                    .evidence
                    .iter()
                    .all(|evidence| evidence.source_kind == beholder_dto::EvidenceKind::Ast)
        }));
        for path in &result.paths {
            assert!(path.nodes.iter().all(|id| node_ids.contains(id.as_str())));
            assert!(path.edges.iter().all(|id| edge_ids.contains(id.as_str())));
        }
    }

    #[test]
    fn workspace_smoke() {
        let store = SemanticStore::memory().unwrap();
        let feature = query(
            &store.db,
            "feature",
            &format!(
                "{DIRECT_RULES}\n?[provider] := direct[\
                        'rpc/Pricing.GetPrice', provider, 'implemented_by', _, _, _\
                     ]"
            ),
            [],
        )
        .unwrap();
        let feature = format!("{feature:?}");
        assert!(feature.contains("pricing/get_price_v2"));
        assert!(!feature.contains("pricing/get_price\""));

        let result = store.context("main", "rpc/Pricing.GetPrice").unwrap();
        assert_eq!(result.edges.len(), 2);
        assert!(
            result
                .nodes
                .iter()
                .any(|value| value.id == "pricing/get_price")
        );
    }

    #[test]
    fn structural_facts_are_context_only() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-structural-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store = SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap();
        let view = WorkspaceView::new(
            "structural",
            "analysis",
            vec![RepositoryState {
                repository: LogicalRepository {
                    identity: "repo".into(),
                },
                head: Some("head".into()),
                fingerprint: "state".into(),
            }],
        )
        .unwrap();
        store
            .publish(
                &view,
                &[facts(
                    &view,
                    vec![
                        Observation::structural(
                            "repo/file",
                            StructuralRelation::Defines,
                            "repo/caller",
                            "src/lib.rs:1",
                        ),
                        Observation::dependency(
                            "repo/caller",
                            DependencyRelation::Calls,
                            "repo/target",
                            "src/lib.rs:2",
                        ),
                    ],
                )],
                &[],
            )
            .unwrap();

        assert_eq!(
            store
                .context("structural", "repo/file")
                .unwrap()
                .edges
                .len(),
            1
        );
        assert!(
            store
                .trace(
                    "structural",
                    "repo/file",
                    "repo/target",
                    beholder_dto::DEFAULT_MAX_HOPS,
                )
                .unwrap()
                .paths
                .is_empty()
        );
        assert_eq!(
            store
                .trace(
                    "structural",
                    "repo/caller",
                    "repo/target",
                    beholder_dto::DEFAULT_MAX_HOPS,
                )
                .unwrap()
                .paths
                .len(),
            1
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }
}
