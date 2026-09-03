use super::schema::*;
use beholder_dto::{
    AnalysisCompleteness, AnalysisDiagnostic, AnalysisDiagnosticSeverity, AnalysisMetadata,
    RepositoryRevision,
};
use mnestic_engine::{
    DataValue, DbInstance, MultiTransaction, NamedRows, ScriptMutability, ScriptRunOptions,
};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SEMANTIC_QUERY_TIMEOUT_SECONDS: f64 = 5.0;
const SLOW_QUERY_SECONDS: f64 = 1.0;
const QUERY_PLAN_TIMEOUT_SECONDS: f64 = 0.25;
const ANALYSIS_METADATA_RULES: &str = include_str!("../../../rules/core/analysis_metadata.datalog");

thread_local! {
    static SEMANTIC_QUERY_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

pub(super) trait QueryRunner {
    fn run_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
        timeout: f64,
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
        timeout: f64,
    ) -> Result<NamedRows, Box<dyn Error>> {
        Ok(self.run_script_with_options(
            script,
            params,
            ScriptMutability::Immutable,
            ScriptRunOptions::new().with_timeout(timeout),
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
        timeout: f64,
    ) -> Result<NamedRows, Box<dyn Error>> {
        Ok(self.run_script(&format!("{script}\n:timeout {timeout}"), params)?)
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
        timeout: f64,
    ) -> Result<NamedRows, Box<dyn Error>> {
        self.transaction.run_query(script, params, timeout)
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

pub(super) fn within_query_budget<T>(read: impl FnOnce() -> T) -> T {
    let previous = SEMANTIC_QUERY_DEADLINE.replace(Some(
        Instant::now() + Duration::from_secs_f64(SEMANTIC_QUERY_TIMEOUT_SECONDS),
    ));
    let result = read();
    SEMANTIC_QUERY_DEADLINE.set(previous);
    result
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
    let timeout = remaining_query_budget();
    if timeout == 0.0 {
        return Err(query_timeout().into());
    }
    db.run_query(script, params, timeout).map_err(|error| {
        if error.to_string() == "Query exceeded its time budget" {
            Box::new(query_timeout()) as Box<dyn Error>
        } else {
            error
        }
    })
}

fn remaining_query_budget() -> f64 {
    SEMANTIC_QUERY_DEADLINE.with(|deadline| {
        deadline
            .get()
            .map(|deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs_f64()
            })
            .unwrap_or(SEMANTIC_QUERY_TIMEOUT_SECONDS)
    })
}

fn without_consuming_query_budget<T>(read: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = read();
    SEMANTIC_QUERY_DEADLINE.with(|deadline| {
        if let Some(deadline_at) = deadline.get() {
            deadline.set(Some(deadline_at + started.elapsed()));
        }
    });
    result
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
        let explain_params = || {
            let mut params = BTreeMap::from([("view".into(), spec.view.into())]);
            params.extend(
                additions()
                    .into_iter()
                    .map(|(name, value)| (name.into(), value)),
            );
            params
        };
        let plan = if result.is_ok() {
            without_consuming_query_budget(|| {
                db.explain_query(spec.script, explain_params(), QUERY_PLAN_TIMEOUT_SECONDS)
            })
        } else {
            if remaining_query_budget() == 0.0 {
                span.record("db.plan.outcome", "deadline_exhausted");
                return result;
            }
            let params = explain_params();
            let plan_timeout = remaining_query_budget().min(QUERY_PLAN_TIMEOUT_SECONDS);
            if plan_timeout == 0.0 {
                span.record("db.plan.outcome", "deadline_exhausted");
                return result;
            }
            db.explain_query(spec.script, params, plan_timeout)
        };
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

fn query_timeout() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "semantic query exceeded its five-second budget",
    )
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
    let rows = query(
        db,
        view,
        "?[incomplete] := *analysis_revision_metadata{\
             view: $view, revision: $revision, incomplete\
         }",
        [("revision", i64::try_from(revision)?.into())],
    )?;
    let incomplete = rows
        .rows
        .first()
        .and_then(|row| match row.first() {
            Some(DataValue::Bool(value)) => Some(*value),
            _ => None,
        })
        .unwrap_or_default();
    let rows = query(
        db,
        view,
        &format!(
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
        ),
        [("revision", i64::try_from(revision)?.into())],
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
        SEMANTIC_QUERY_TIMEOUT_SECONDS,
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
        SEMANTIC_QUERY_TIMEOUT_SECONDS,
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
    let rules = match direction {
        TraversalDirection::Outgoing => vec![
            (
                OUTGOING_DEPENDENCY_RULES,
                "selected_edge",
                ":reorder written",
            ),
            (
                OUTGOING_FACT_SHARD_DEPENDENCY_RULES,
                "shard_edge",
                ":reorder written",
            ),
        ],
        TraversalDirection::Incoming => vec![
            (INCOMING_DEPENDENCY_RULES, "selected_edge", ""),
            (
                INCOMING_FACT_SHARD_DEPENDENCY_RULES,
                "shard_edge",
                ":reorder written",
            ),
        ],
    };
    let scripts = rules
        .into_iter()
        .map(|(rules, edge_rule, reorder)| {
            format!(
                "{rules}\n\
                 frontier[id] <- $frontier\n\
                 ?[row_kind, entity, hops, edge_from, edge_to, relation, evidence, confidence, provenance] := \
                     {edge_rule}[edge_from, edge_to, relation, evidence, confidence, provenance], \
                     row_kind = 'edge', entity = '', hops = 0\n\
                 :order edge_from, edge_to, relation\n\
                 {reorder}"
            )
        })
        .collect::<Vec<_>>();
    let override_rules = match direction {
        TraversalDirection::Outgoing => OUTGOING_DEPENDENCY_OVERRIDE_QUERY,
        TraversalDirection::Incoming => INCOMING_DEPENDENCY_OVERRIDE_QUERY,
    };
    let override_script = format!(
        "{override_rules}\n\
         frontier[id] <- $frontier"
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
    let (core_rules, shard_rules, shard_not_overridden) = match direction {
        TraversalDirection::Outgoing => (
            OUTGOING_DEPENDENCY_RULES,
            OUTGOING_FACT_SHARD_DEPENDENCY_RULES,
            "not dependency_override[from, relation, to, _, _, _, _]",
        ),
        TraversalDirection::Incoming => (
            INCOMING_DEPENDENCY_RULES,
            INCOMING_FACT_SHARD_DEPENDENCY_RULES,
            "not dependency_override[from, relation, to]",
        ),
    };
    let boundary_scripts = [
        format!(
            "{core_rules}\n\
             boundary_edge[from, to, relation, evidence, confidence, provenance] := \
                 selected_edge[from, to, relation, evidence, confidence, provenance]\n\
             frontier[id] <- $frontier\n\
             visited[id] <- $visited\n\
             {boundary_projection}\n\
             :limit 1"
        ),
        format!(
            "{core_rules}\n{shard_rules}\n\
             boundary_edge[from, to, relation, evidence, confidence, provenance] := \
                 shard_edge[from, to, relation, evidence, confidence, provenance], \
                 {shard_not_overridden}\n\
             frontier[id] <- $frontier\n\
             visited[id] <- $visited\n\
             {boundary_projection}\n\
             :limit 1"
        ),
    ];
    let (operations, boundary_operations, override_operation) = match direction {
        TraversalDirection::Outgoing => (
            ["traversal.outgoing.core", "traversal.outgoing.fact_shard"],
            [
                "traversal.outgoing.boundary.core",
                "traversal.outgoing.boundary.fact_shard",
            ],
            "traversal.outgoing.overrides",
        ),
        TraversalDirection::Incoming => (
            ["traversal.incoming.core", "traversal.incoming.fact_shard"],
            [
                "traversal.incoming.boundary.core",
                "traversal.incoming.boundary.fact_shard",
            ],
            "traversal.incoming.overrides",
        ),
    };
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
        if hops == max_hops {
            let visited_values = DataValue::List(
                visited
                    .iter()
                    .map(|entity| DataValue::List(vec![entity.as_str().into()]))
                    .collect(),
            );
            for (operation, script) in boundary_operations.into_iter().zip(&boundary_scripts) {
                let mut result =
                    observed_query(db, QuerySpec::new(operation, view, script), || {
                        vec![
                            ("frontier", values.clone()),
                            ("visited", visited_values.clone()),
                        ]
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
        let mut edge_groups = Vec::new();
        for (operation, script) in operations.into_iter().zip(&scripts) {
            let result = observed_query(db, QuerySpec::new(operation, view, script), || {
                vec![("frontier", values.clone())]
            })?;
            if headers.is_empty() {
                headers = result.headers;
            }
            edge_groups.push(result.rows);
        }
        let overrides = observed_query(
            db,
            QuerySpec::new(override_operation, view, &override_script),
            || vec![("frontier", values.clone())],
        )?;
        merge_fact_shard_overrides(&mut edge_groups[1], overrides.rows, direction, &frontier);
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
    use std::{
        cell::RefCell,
        collections::{BTreeMap, BTreeSet},
        fs, thread,
        time::Duration,
        time::SystemTime,
    };

    struct TimeoutRunner(RefCell<Vec<f64>>);

    impl QueryRunner for TimeoutRunner {
        fn run_query(
            &self,
            _script: &str,
            _params: BTreeMap<String, DataValue>,
            timeout: f64,
        ) -> Result<NamedRows, Box<dyn Error>> {
            self.0.borrow_mut().push(timeout);
            Ok(NamedRows::default())
        }
    }

    struct ExhaustedPlanRunner(Cell<bool>);

    impl QueryRunner for ExhaustedPlanRunner {
        fn run_query(
            &self,
            _script: &str,
            _params: BTreeMap<String, DataValue>,
            _timeout: f64,
        ) -> Result<NamedRows, Box<dyn Error>> {
            SEMANTIC_QUERY_DEADLINE.set(Some(Instant::now() - Duration::from_millis(1)));
            Err(query_timeout().into())
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

    struct DisabledPlanRunner(Cell<bool>);

    impl QueryRunner for DisabledPlanRunner {
        fn run_query(
            &self,
            _script: &str,
            _params: BTreeMap<String, DataValue>,
            _timeout: f64,
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

    struct ExpiringPlanRunner(Cell<bool>);

    impl QueryRunner for ExpiringPlanRunner {
        fn run_query(
            &self,
            _script: &str,
            _params: BTreeMap<String, DataValue>,
            _timeout: f64,
        ) -> Result<NamedRows, Box<dyn Error>> {
            SEMANTIC_QUERY_DEADLINE.set(Some(Instant::now() + Duration::from_millis(1)));
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

    fn with_info_spans<T>(read: impl FnOnce() -> T) -> T {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_writer(io::sink)
            .finish();
        tracing::subscriber::with_default(subscriber, read)
    }

    #[test]
    fn one_budget_is_shared_by_every_query_in_a_semantic_read() {
        let runner = TimeoutRunner(RefCell::new(Vec::new()));

        within_query_budget(|| {
            query(&runner, "main", "?[value] <- [[1]]", []).unwrap();
            thread::sleep(Duration::from_millis(5));
            query(&runner, "main", "?[value] <- [[1]]", []).unwrap();
        });

        let timeouts = runner.0.into_inner();
        assert!(timeouts[0] <= SEMANTIC_QUERY_TIMEOUT_SECONDS);
        assert!(timeouts[1] < timeouts[0]);

        SEMANTIC_QUERY_DEADLINE.set(Some(Instant::now() - Duration::from_millis(1)));
        let error = query(
            &TimeoutRunner(RefCell::new(Vec::new())),
            "main",
            "?[value] <- [[1]]",
            [],
        )
        .unwrap_err();
        SEMANTIC_QUERY_DEADLINE.set(None);
        assert_eq!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::TimedOut)
        );
    }

    #[test]
    fn mnestic_stops_work_when_the_query_budget_expires() {
        let db = memory_database().unwrap();
        SEMANTIC_QUERY_DEADLINE.set(Some(Instant::now() + Duration::from_millis(1)));
        let error = query(
            &db,
            "main",
            "?[left, right] := left in int_range(100000), right in int_range(100000)",
            [],
        )
        .unwrap_err();
        SEMANTIC_QUERY_DEADLINE.set(None);

        assert_eq!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::TimedOut)
        );
    }

    #[test]
    fn exhausted_query_budget_skips_plan_collection() {
        let runner = ExhaustedPlanRunner(Cell::new(false));
        let parameter_builds = Cell::new(0);

        let _ = with_info_spans(|| {
            within_query_budget(|| {
                observed_query(
                    &runner,
                    QuerySpec::new("test", "main", "?[value] <- [[1]]"),
                    || {
                        parameter_builds.set(parameter_builds.get() + 1);
                        Vec::new()
                    },
                )
            })
        });

        assert!(!runner.0.get());
        assert_eq!(parameter_builds.get(), 1);
    }

    #[test]
    fn parameter_building_cannot_extend_failure_plan_past_the_deadline() {
        let runner = ExpiringPlanRunner(Cell::new(false));
        let parameter_builds = Cell::new(0);

        let _ = with_info_spans(|| {
            within_query_budget(|| {
                observed_query(
                    &runner,
                    QuerySpec::new("test", "main", "?[value] <- [[1]]"),
                    || {
                        parameter_builds.set(parameter_builds.get() + 1);
                        if parameter_builds.get() == 2 {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Vec::new()
                    },
                )
            })
        });

        assert!(!runner.0.get());
        assert_eq!(parameter_builds.get(), 2);
    }

    #[test]
    fn disabled_query_span_skips_plan_collection() {
        let runner = DisabledPlanRunner(Cell::new(false));
        let parameter_builds = Cell::new(0);

        let _ = within_query_budget(|| {
            observed_query(
                &runner,
                QuerySpec::new("test", "main", "?[value] <- [[1]]"),
                || {
                    parameter_builds.set(parameter_builds.get() + 1);
                    Vec::new()
                },
            )
        });

        assert!(!runner.0.get());
        assert_eq!(parameter_builds.get(), 1);
    }

    #[test]
    fn diagnostics_do_not_consume_the_query_budget() {
        within_query_budget(|| {
            let before = SEMANTIC_QUERY_DEADLINE.get().unwrap();
            without_consuming_query_budget(|| thread::sleep(Duration::from_millis(5)));
            let after = SEMANTIC_QUERY_DEADLINE.get().unwrap();

            assert!(after > before);
        });
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
