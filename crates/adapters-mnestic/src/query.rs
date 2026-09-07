use super::schema::*;
use beholder_dto::{
    AnalysisCompleteness, AnalysisDiagnostic, AnalysisDiagnosticSeverity, AnalysisMetadata,
    DiagnosticCounts, RepositoryRevision,
};
use mnestic_engine::{
    DataValue, DbInstance, MultiTransaction, NamedRows, ScriptMutability, ScriptRunOptions,
};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SEMANTIC_QUERY_WARNING_SECONDS: f64 = 5.0;
const REPOSITORY_QUERY_TIMEOUT_SECONDS: f64 = 5.0;
const SLOW_QUERY_SECONDS: f64 = 1.0;
const QUERY_PLAN_TIMEOUT_SECONDS: f64 = 0.25;

pub(super) trait QueryRunner {
    fn run_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
    ) -> Result<NamedRows, Box<dyn Error>>;

    fn run_query_with_timeout(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
        _timeout: Option<f64>,
    ) -> Result<NamedRows, Box<dyn Error>> {
        self.run_query(script, params)
    }

    fn explain_query(
        &self,
        _script: &str,
        _params: BTreeMap<String, DataValue>,
        _timeout: f64,
    ) -> Result<Option<NamedRows>, Box<dyn Error>> {
        Ok(None)
    }

    fn run<Q: MnesticQuery>(&self, view: &str, params: Q::Params) -> Result<Vec<Q::Row>, QueryError>
    where
        Self: Sized,
    {
        let bind = || {
            let mut bound = BTreeMap::from([("view".into(), view.into())]);
            bound.extend(Q::bind(&params));
            bound
        };
        let result = observed_bound_query(
            self,
            QuerySpec::new(Q::OPERATION, view, Q::SCRIPT),
            Q::TIMEOUT_SECONDS,
            bind,
        )
        .map_err(|error| QueryError::new(Q::OPERATION, error.to_string()))?;
        let expected = Q::HEADERS
            .iter()
            .map(|header| (*header).to_owned())
            .collect::<Vec<_>>();
        if result.headers != expected {
            return Err(QueryError::new(
                Q::OPERATION,
                format!(
                    "unexpected output headers: expected {expected:?}, got {:?}",
                    result.headers
                ),
            ));
        }
        if let Some((index, row)) = result
            .rows
            .iter()
            .enumerate()
            .find(|(_, row)| row.len() != expected.len())
        {
            return Err(QueryError::new(
                Q::OPERATION,
                format!(
                    "unexpected row width at row {index}: expected {}, got {}",
                    expected.len(),
                    row.len()
                ),
            ));
        }
        result
            .rows
            .iter()
            .map(|row| Q::decode(&result.headers, row))
            .collect()
    }
}

pub(super) trait MnesticQuery {
    const OPERATION: &'static str;
    const SCRIPT: &'static str;
    const HEADERS: &'static [&'static str];
    const TIMEOUT_SECONDS: Option<f64> = None;

    type Params;
    type Row;

    fn bind(params: &Self::Params) -> BTreeMap<String, DataValue>;
    fn decode(headers: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError>;
}

#[derive(Debug)]
pub(super) struct QueryError {
    operation: &'static str,
    detail: String,
}

impl QueryError {
    fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Mnestic query {} failed: {}",
            self.operation, self.detail
        )
    }
}

impl Error for QueryError {}

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

    fn run_query_with_timeout(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
        timeout: Option<f64>,
    ) -> Result<NamedRows, Box<dyn Error>> {
        let options = timeout.map_or_else(ScriptRunOptions::new, |timeout| {
            ScriptRunOptions::new().with_timeout(timeout)
        });
        Ok(self.run_script_with_options(script, params, ScriptMutability::Immutable, options)?)
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

    fn run_query_with_timeout(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
        timeout: Option<f64>,
    ) -> Result<NamedRows, Box<dyn Error>> {
        self.transaction
            .run_query_with_timeout(script, params, timeout)
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

#[cfg(test)]
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

fn observed_bound_query(
    db: &impl QueryRunner,
    spec: QuerySpec<'_>,
    timeout: Option<f64>,
    params: impl Fn() -> BTreeMap<String, DataValue>,
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
        let result = db.run_query_with_timeout(spec.script, params(), timeout);
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
        let plan = db.explain_query(spec.script, params(), QUERY_PLAN_TIMEOUT_SECONDS);
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

struct AnalysisRevisionQuery;

impl MnesticQuery for AnalysisRevisionQuery {
    const OPERATION: &'static str = "analysis_revision";
    const SCRIPT: &'static str = "?[revision] := *analysis_revision{view: $view, revision}";
    const HEADERS: &'static [&'static str] = &["revision"];

    type Params = ();
    type Row = i64;

    fn bind(_: &Self::Params) -> BTreeMap<String, DataValue> {
        BTreeMap::new()
    }

    fn decode(_: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
        Ok(row[0].get_int().unwrap_or_default())
    }
}

pub(super) fn analysis_revision(db: &impl QueryRunner, view: &str) -> Result<u64, Box<dyn Error>> {
    Ok(db
        .run::<AnalysisRevisionQuery>(view, ())?
        .into_iter()
        .next()
        .unwrap_or_default()
        .try_into()?)
}

struct AnalysisMetadataParams {
    revision: i64,
}

struct AnalysisMetadataCompleteness;

impl MnesticQuery for AnalysisMetadataCompleteness {
    const OPERATION: &'static str = "analysis_metadata.completeness";
    const SCRIPT: &'static str = "?[incomplete] := *analysis_revision_metadata{view: $view, revision: $revision, incomplete}";
    const HEADERS: &'static [&'static str] = &["incomplete"];

    type Params = AnalysisMetadataParams;
    type Row = bool;

    fn bind(params: &Self::Params) -> BTreeMap<String, DataValue> {
        BTreeMap::from([("revision".into(), params.revision.into())])
    }

    fn decode(_: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
        Ok(matches!(row[0], DataValue::Bool(true)))
    }
}

struct AnalysisMetadataDiagnostics;

impl MnesticQuery for AnalysisMetadataDiagnostics {
    const OPERATION: &'static str = "analysis_metadata.diagnostics";
    const SCRIPT: &'static str = concat!(
        include_str!("../../../rules/core/analysis_metadata.datalog"),
        r#"
baseline_diagnostic[repository, code, severity, path, line, detail] :=
    *analysis_revision_diagnostic{view: $view, revision: $revision, repository, code, severity, path, line, detail},
    not selected_diagnostic_replacement[repository, code]
?[repository, code, severity, path, line, detail] := baseline_diagnostic[repository, code, severity, path, line, detail]
?[repository, code, severity, path, line, detail] := enrichment_diagnostic[repository, code, severity, path, line, detail],
    not baseline_diagnostic[repository, code, severity, path, line, _]
:order severity, repository, path, line, code"#
    );
    const HEADERS: &'static [&'static str] =
        &["repository", "code", "severity", "path", "line", "detail"];

    type Params = AnalysisMetadataParams;
    type Row = AnalysisDiagnostic;

    fn bind(params: &Self::Params) -> BTreeMap<String, DataValue> {
        AnalysisMetadataCompleteness::bind(params)
    }

    fn decode(_: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
        let severity = match row[2].get_str() {
            Some("known_limitation") => AnalysisDiagnosticSeverity::KnownLimitation,
            Some("warning") => AnalysisDiagnosticSeverity::Warning,
            _ => {
                return Err(QueryError::new(
                    Self::OPERATION,
                    "unknown stored analysis diagnostic severity",
                ));
            }
        };
        let line = u32::try_from(row[4].get_int().unwrap_or_default())
            .map_err(|error| QueryError::new(Self::OPERATION, error.to_string()))?;
        let detail = row[5].get_str().unwrap_or_default();
        Ok(AnalysisDiagnostic {
            repository: row[0].get_str().unwrap_or_default().into(),
            code: row[1].get_str().unwrap_or_default().into(),
            severity,
            path: PathBuf::from(row[3].get_str().unwrap_or_default()),
            line: (line != 0).then_some(line),
            detail: (!detail.is_empty()).then(|| detail.into()),
        })
    }
}

struct AnalysisMetadataDiagnosticCounts;

impl MnesticQuery for AnalysisMetadataDiagnosticCounts {
    const OPERATION: &'static str = "analysis_metadata.diagnostic_counts";
    const SCRIPT: &'static str = concat!(
        include_str!("../../../rules/core/analysis_metadata.datalog"),
        r#"
baseline_diagnostic[repository, code, severity, path, line, detail] :=
    *analysis_revision_diagnostic{view: $view, revision: $revision, repository, code, severity, path, line, detail},
    not selected_diagnostic_replacement[repository, code]
diagnostic[repository, code, severity, path, line, detail] := baseline_diagnostic[repository, code, severity, path, line, detail]
diagnostic[repository, code, severity, path, line, detail] := enrichment_diagnostic[repository, code, severity, path, line, detail],
    not baseline_diagnostic[repository, code, severity, path, line, _]
?[severity, count(code)] := diagnostic[repository, code, severity, path, line, detail]
:order severity"#
    );
    const HEADERS: &'static [&'static str] = &["severity", "count(code)"];

    type Params = AnalysisMetadataParams;
    type Row = (AnalysisDiagnosticSeverity, u64);

    fn bind(params: &Self::Params) -> BTreeMap<String, DataValue> {
        AnalysisMetadataCompleteness::bind(params)
    }

    fn decode(_: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
        let severity = match row[0].get_str() {
            Some("known_limitation") => AnalysisDiagnosticSeverity::KnownLimitation,
            Some("warning") => AnalysisDiagnosticSeverity::Warning,
            _ => {
                return Err(QueryError::new(
                    Self::OPERATION,
                    "unknown stored analysis diagnostic severity",
                ));
            }
        };
        let count = row[1]
            .get_int()
            .and_then(|count| u64::try_from(count).ok())
            .ok_or_else(|| QueryError::new(Self::OPERATION, "diagnostic count is invalid"))?;
        Ok((severity, count))
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct AnalysisMetadataOptions {
    pub include_diagnostics: bool,
}

impl Default for AnalysisMetadataOptions {
    fn default() -> Self {
        Self {
            include_diagnostics: true,
        }
    }
}

pub(super) fn analysis_metadata(
    db: &impl QueryRunner,
    view: &str,
    revision: u64,
    options: AnalysisMetadataOptions,
) -> Result<AnalysisMetadata, Box<dyn Error>> {
    let revision = i64::try_from(revision)?;
    let params = AnalysisMetadataParams { revision };
    let incomplete = db
        .run::<AnalysisMetadataCompleteness>(
            view,
            AnalysisMetadataParams {
                revision: params.revision,
            },
        )?
        .into_iter()
        .next()
        .unwrap_or_default();
    let (diagnostics, diagnostic_counts) = if options.include_diagnostics {
        let diagnostics = db.run::<AnalysisMetadataDiagnostics>(view, params)?;
        let counts = diagnostic_counts(&diagnostics);
        (diagnostics, counts)
    } else {
        let mut counts = DiagnosticCounts::default();
        for (severity, count) in db.run::<AnalysisMetadataDiagnosticCounts>(view, params)? {
            counts.total += count;
            match severity {
                AnalysisDiagnosticSeverity::KnownLimitation => counts.known_limitations = count,
                AnalysisDiagnosticSeverity::Warning => counts.warnings = count,
            }
        }
        (Vec::new(), counts)
    };
    Ok(AnalysisMetadata {
        completeness: if incomplete {
            AnalysisCompleteness::Incomplete
        } else {
            AnalysisCompleteness::Complete
        },
        diagnostic_counts,
        diagnostics,
    })
}

struct RepositoryParams {
    repository: String,
}

struct RepositoryRevisionQuery;

impl MnesticQuery for RepositoryRevisionQuery {
    const OPERATION: &'static str = "repository_revision";
    const SCRIPT: &'static str = "?[source_state, analysis_identity, head, incomplete] := *repository_revision{repository: $repository, source_state, analysis_identity, head, incomplete}";
    const HEADERS: &'static [&'static str] =
        &["source_state", "analysis_identity", "head", "incomplete"];
    const TIMEOUT_SECONDS: Option<f64> = Some(REPOSITORY_QUERY_TIMEOUT_SECONDS);

    type Params = RepositoryParams;
    type Row = (String, String, String, bool);

    fn bind(params: &Self::Params) -> BTreeMap<String, DataValue> {
        BTreeMap::from([("repository".into(), params.repository.as_str().into())])
    }

    fn decode(_: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
        let source_state = row[0].get_str().ok_or_else(|| {
            QueryError::new(
                Self::OPERATION,
                "stored repository source state is not a string",
            )
        })?;
        let analysis_identity = row[1].get_str().ok_or_else(|| {
            QueryError::new(
                Self::OPERATION,
                "stored repository analysis identity is not a string",
            )
        })?;
        let head = row[2].get_str().ok_or_else(|| {
            QueryError::new(Self::OPERATION, "stored repository head is not a string")
        })?;
        let incomplete = match row[3] {
            DataValue::Bool(value) => value,
            _ => {
                return Err(QueryError::new(
                    Self::OPERATION,
                    "stored repository completeness is not a boolean",
                ));
            }
        };
        Ok((
            source_state.into(),
            analysis_identity.into(),
            head.into(),
            incomplete,
        ))
    }
}

struct RepositoryDiagnosticsQuery;

impl MnesticQuery for RepositoryDiagnosticsQuery {
    const OPERATION: &'static str = "repository_revision.diagnostics";
    const SCRIPT: &'static str = "?[code, severity, path, line, detail] := *repository_revision_diagnostic{repository: $repository, code, severity, path, line, detail}\n:order severity, path, line, code";
    const HEADERS: &'static [&'static str] = &["code", "severity", "path", "line", "detail"];
    const TIMEOUT_SECONDS: Option<f64> = Some(REPOSITORY_QUERY_TIMEOUT_SECONDS);

    type Params = RepositoryParams;
    type Row = (
        String,
        AnalysisDiagnosticSeverity,
        PathBuf,
        Option<u32>,
        Option<String>,
    );

    fn bind(params: &Self::Params) -> BTreeMap<String, DataValue> {
        RepositoryRevisionQuery::bind(params)
    }

    fn decode(_: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
        let severity = match row[1].get_str() {
            Some("known_limitation") => AnalysisDiagnosticSeverity::KnownLimitation,
            Some("warning") => AnalysisDiagnosticSeverity::Warning,
            _ => {
                return Err(QueryError::new(
                    Self::OPERATION,
                    "unknown stored repository diagnostic severity",
                ));
            }
        };
        let line = u32::try_from(row[3].get_int().unwrap_or_default())
            .map_err(|error| QueryError::new(Self::OPERATION, error.to_string()))?;
        let detail = row[4].get_str().unwrap_or_default();
        Ok((
            row[0].get_str().unwrap_or_default().into(),
            severity,
            PathBuf::from(row[2].get_str().unwrap_or_default()),
            (line != 0).then_some(line),
            (!detail.is_empty()).then(|| detail.into()),
        ))
    }
}

pub(super) fn repository_revision(
    db: &DbInstance,
    repository: &str,
) -> Result<Option<RepositoryRevision>, Box<dyn Error>> {
    let params = RepositoryParams {
        repository: repository.into(),
    };
    let Some((source_state, analysis_identity, head, incomplete)) = db
        .run::<RepositoryRevisionQuery>(
            repository,
            RepositoryParams {
                repository: params.repository.clone(),
            },
        )?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let diagnostics = db
        .run::<RepositoryDiagnosticsQuery>(repository, params)?
        .into_iter()
        .map(|(code, severity, path, line, detail)| AnalysisDiagnostic {
            repository: repository.into(),
            code,
            severity,
            path,
            line,
            detail,
        })
        .collect::<Vec<_>>();
    let diagnostic_counts = diagnostic_counts(&diagnostics);
    Ok(Some(RepositoryRevision {
        source_state,
        head: (!head.is_empty()).then_some(head),
        analysis_identity,
        analysis: AnalysisMetadata {
            completeness: if incomplete {
                AnalysisCompleteness::Incomplete
            } else {
                AnalysisCompleteness::Complete
            },
            diagnostic_counts,
            diagnostics,
        },
    }))
}

fn diagnostic_counts(diagnostics: &[AnalysisDiagnostic]) -> DiagnosticCounts {
    let mut counts = DiagnosticCounts {
        total: diagnostics.len() as u64,
        ..Default::default()
    };
    for diagnostic in diagnostics {
        match diagnostic.severity {
            AnalysisDiagnosticSeverity::KnownLimitation => counts.known_limitations += 1,
            AnalysisDiagnosticSeverity::Warning => counts.warnings += 1,
        }
    }
    counts
}

struct PublishedRepositoryHead;

impl MnesticQuery for PublishedRepositoryHead {
    const OPERATION: &'static str = "published_repository_head";
    const SCRIPT: &'static str = r#"selected_head[head] :=
    *analysis_revision{view: $view, revision},
    *analysis_revision_repository_head{view: $view, revision, repository: $repository, head}
selected_head[head] :=
    *analysis_revision{view: $view, revision},
    *analysis_revision_state{view: $view, revision, repository: $repository, state},
    *repository_state{fingerprint: state, repository: $repository, head},
    not *analysis_revision_repository_head{view: $view, revision, repository: $repository}
?[head] := selected_head[head]"#;
    const HEADERS: &'static [&'static str] = &["head"];

    type Params = RepositoryParams;
    type Row = String;

    fn bind(params: &Self::Params) -> BTreeMap<String, DataValue> {
        RepositoryRevisionQuery::bind(params)
    }

    fn decode(_: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
        Ok(row[0].get_str().unwrap_or_default().into())
    }
}

pub(super) fn published_repository_head(
    db: &impl QueryRunner,
    view: &str,
    repository: &str,
) -> Result<Option<String>, Box<dyn Error>> {
    Ok(db
        .run::<PublishedRepositoryHead>(
            view,
            RepositoryParams {
                repository: repository.into(),
            },
        )?
        .into_iter()
        .next()
        .filter(|head| !head.is_empty()))
}

struct EntityFactsParams {
    entities: BTreeSet<String>,
}

#[derive(Debug)]
struct EntityFactRow {
    id: String,
    kind: DataValue,
    metadata: DataValue,
}

impl EntityFactRow {
    fn into_values(self) -> Vec<DataValue> {
        vec![self.id.into(), self.kind, self.metadata]
    }
}

const ENTITY_FACT_HEADERS: &[&str] = &["id", "kind", "metadata"];

macro_rules! entity_facts_query {
    ($name:ident, $operation:literal, $script:literal) => {
        struct $name;

        impl MnesticQuery for $name {
            const OPERATION: &'static str = $operation;
            const SCRIPT: &'static str = $script;
            const HEADERS: &'static [&'static str] = ENTITY_FACT_HEADERS;

            type Params = EntityFactsParams;
            type Row = EntityFactRow;

            fn bind(params: &Self::Params) -> BTreeMap<String, DataValue> {
                BTreeMap::from([(
                    "entities".into(),
                    DataValue::List(
                        params
                            .entities
                            .iter()
                            .map(|id| DataValue::List(vec![id.as_str().into()]))
                            .collect(),
                    ),
                )])
            }

            fn decode(_: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
                Ok(EntityFactRow {
                    id: row[0]
                        .get_str()
                        .ok_or_else(|| {
                            QueryError::new(Self::OPERATION, "column id is not a string")
                        })?
                        .into(),
                    kind: row[1].clone(),
                    metadata: row[2].clone(),
                })
            }
        }
    };
}

entity_facts_query!(
    BaselineEntityFacts,
    "entity_facts.baseline",
    "requested[id] <- $entities\n\
     selected_state[state] := *analysis_revision{view: $view, revision}, *analysis_revision_state{view: $view, revision, state}\n\
     ?[id, kind, metadata] := requested[id], selected_state[state], *state_entity{state, id, kind, metadata}\n\
     ?[id, kind, metadata] := requested[id], *analysis_revision{view: $view, revision}, *analysis_revision_entity{view: $view, revision, id, kind, metadata}\n\
     :order id"
);
entity_facts_query!(
    ShardEntityFacts,
    "entity_facts.shard",
    "requested[id] <- $entities\n\
     ?[id, kind, metadata] := requested[id], *analysis_fact_shard_entity:by_id{id, producer, owner, version, kind, metadata}, *analysis_fact_shard_selection:by_owner{view: $view, owner, producer, version}\n\
     :order id\n\
     :reorder written"
);
entity_facts_query!(
    EnrichmentEntityFacts,
    "entity_facts.enrichment",
    "requested[id] <- $entities\n\
     selected_enrichment[owner] := *analysis_revision{view: $view, revision}, *analysis_revision_repository_enrichment{view: $view, revision, owner}\n\
     ?[id, kind, metadata] := requested[id], *analysis_enrichment_entity_selection{view: $view, id, owner}, selected_enrichment[owner], *enrichment_entity_contribution{view: $view, owner, id, kind, metadata}\n\
     :order id"
);

pub(super) fn entity_facts(
    db: &impl QueryRunner,
    view: &str,
    entities: &BTreeSet<String>,
) -> Result<NamedRows, Box<dyn Error>> {
    let mut baseline = db.run::<BaselineEntityFacts>(
        view,
        EntityFactsParams {
            entities: entities.clone(),
        },
    )?;
    let mut baseline_ids = baseline
        .iter()
        .map(|row| row.id.clone())
        .collect::<BTreeSet<_>>();
    let nested = db.run::<ShardEntityFacts>(
        view,
        EntityFactsParams {
            entities: entities
                .iter()
                .filter(|id| !baseline_ids.contains(*id))
                .cloned()
                .collect(),
        },
    )?;
    baseline_ids.extend(nested.iter().map(|row| row.id.clone()));
    baseline.extend(nested);
    baseline.extend(
        db.run::<EnrichmentEntityFacts>(
            view,
            EntityFactsParams {
                entities: entities.clone(),
            },
        )?
        .into_iter()
        .filter(|row| !baseline_ids.contains(&row.id)),
    );
    Ok(NamedRows::new(
        ENTITY_FACT_HEADERS
            .iter()
            .map(|header| (*header).into())
            .collect(),
        baseline
            .into_iter()
            .map(EntityFactRow::into_values)
            .collect(),
    ))
}

struct AllEntityFactsQuery;

impl MnesticQuery for AllEntityFactsQuery {
    const OPERATION: &'static str = "entity_facts.all";
    const SCRIPT: &'static str = "selected_state[state] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_state{view: $view, revision, state}\n\
         selected_enrichment[owner] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_repository_enrichment{view: $view, revision, owner}\n\
         baseline_id[id] := selected_state[state], *state_entity{state, id}\n\
         baseline_id[id] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_entity{view: $view, revision, id}\n\
         baseline_id[id] := *analysis_fact_shard_selection{view: $view, producer, owner, version}, \
             *analysis_fact_shard_entity{producer, owner, version, id}\n\
         ?[id, kind, metadata] := selected_state[state], *state_entity{state, id, kind, metadata}\n\
         ?[id, kind, metadata] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_entity{view: $view, revision, id, kind, metadata}\n\
         ?[id, kind, metadata] := \
             *analysis_fact_shard_selection{view: $view, producer, owner, version}, \
             *analysis_fact_shard_entity{producer, owner, version, id, kind, metadata}\n\
         ?[id, kind, metadata] := \
             *analysis_enrichment_entity_selection{view: $view, id, owner}, \
             selected_enrichment[owner], \
             *enrichment_entity_contribution{view: $view, owner, id, kind, metadata}, \
             not baseline_id[id]\n\
         :order id";
    const HEADERS: &'static [&'static str] = ENTITY_FACT_HEADERS;

    type Params = ();
    type Row = EntityFactRow;

    fn bind(_: &Self::Params) -> BTreeMap<String, DataValue> {
        BTreeMap::new()
    }

    fn decode(headers: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
        BaselineEntityFacts::decode(headers, row)
    }
}

pub(super) fn all_entity_facts(
    db: &impl QueryRunner,
    view: &str,
) -> Result<NamedRows, Box<dyn Error>> {
    Ok(NamedRows::new(
        ENTITY_FACT_HEADERS
            .iter()
            .map(|header| (*header).into())
            .collect(),
        db.run::<AllEntityFactsQuery>(view, ())?
            .into_iter()
            .map(EntityFactRow::into_values)
            .collect(),
    ))
}

struct SearchEntityFactsParams {
    candidate: String,
    query: String,
    limit: u32,
}

const SEARCH_ENTITY_FACT_HEADERS: &[&str] = &["id", "kind", "metadata", "rank"];

macro_rules! ranked_entity_search {
    ($source:literal) => {
        concat!(
            $source,
            "\n\
             special[id] := candidate[id, _, _], starts_with(id, 'elixir-module://')\n\
             special[id] := candidate[id, _, _], regex_matches(id, '^elixir-call://(.+)/([^/]+)/([0-9]+)$')\n\
             special[id] := candidate[id, _, _], regex_matches(id, '^elixir-call://([^/]+)/([0-9]+)$')\n\
             special[id] := candidate[id, _, _], regex_matches(id, '^(proto-method|grpc)://([^/]+)/([^/]+)$')\n\
             special[id] := candidate[id, _, _], regex_matches(id, '^.*/elixir/(.*/)?([^/]+)/([0-9]+)$')\n\
             display[id, name] := candidate[id, _, _], starts_with(id, 'elixir-module://'), name = regex_replace(id, '^elixir-module://', '')\n\
             display[id, name] := candidate[id, _, _], regex_matches(id, '^elixir-call://(.+)/([^/]+)/([0-9]+)$'), name = regex_replace(id, '^elixir-call://(.+)/([^/]+)/([0-9]+)$', '$1.$2/$3')\n\
             display[id, name] := candidate[id, _, _], regex_matches(id, '^elixir-call://([^/]+)/([0-9]+)$'), name = regex_replace(id, '^elixir-call://([^/]+)/([0-9]+)$', '$1/$2')\n\
             display[id, name] := candidate[id, _, _], regex_matches(id, '^(proto-method|grpc)://([^/]+)/([^/]+)$'), name = regex_replace(id, '^(proto-method|grpc)://([^/]*[.])?([^./]+)/([^/]+)$', '$3.$4')\n\
             display[id, name] := candidate[id, _, _], regex_matches(id, '^.*/elixir/(.*/)?([^/]+)/([0-9]+)$'), name = regex_replace(id, '^.*/elixir/(.*/)?([^/]+)/([0-9]+)$', '$2/$3')\n\
             display[id, name] := candidate[id, _, _], not special[id], regex_matches(id, '^.*[/:]([^/:]+)$'), name = regex_replace(id, '^.*[/:]([^/:]+)$', '$1')\n\
             display[id, id] := candidate[id, _, _], not special[id], not regex_matches(id, '^.*[/:]([^/:]+)$')\n\
             matched[id, kind, metadata, name] := candidate[id, kind, metadata], display[id, name], starts_with(id, $query)\n\
             matched[id, kind, metadata, name] := candidate[id, kind, metadata], display[id, name], starts_with(name, $query)\n\
             ranked[id, kind, metadata, min(rank)] := matched[id, kind, metadata, _], id = $query, rank = 0\n\
             ranked[id, kind, metadata, min(rank)] := matched[id, kind, metadata, name], name = $query, rank = 1\n\
             ranked[id, kind, metadata, min(rank)] := matched[id, kind, metadata, name], starts_with(id, $query), rank = 2\n\
             ranked[id, kind, metadata, min(rank)] := matched[id, kind, metadata, name], starts_with(name, $query), rank = 2\n\
             ?[id, kind, metadata, rank] := ranked[id, kind, metadata, rank]\n\
             :order rank, id\n\
             :limit $limit"
        )
    };
}

macro_rules! search_entity_facts_query {
    ($name:ident, $operation:literal, $script:expr) => {
        struct $name;

        impl MnesticQuery for $name {
            const OPERATION: &'static str = $operation;
            const SCRIPT: &'static str = $script;
            const HEADERS: &'static [&'static str] = SEARCH_ENTITY_FACT_HEADERS;

            type Params = SearchEntityFactsParams;
            type Row = EntityFactRow;

            fn bind(params: &Self::Params) -> BTreeMap<String, DataValue> {
                BTreeMap::from([
                    ("candidate".into(), params.candidate.as_str().into()),
                    ("query".into(), params.query.as_str().into()),
                    ("limit".into(), i64::from(params.limit).into()),
                ])
            }

            fn decode(headers: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
                BaselineEntityFacts::decode(headers, row)
            }
        }
    };
}

search_entity_facts_query!(
    SearchStateEntityFacts,
    "entity_search.state",
    ranked_entity_search!(
        "selected_state[state] := *analysis_revision{view: $view, revision}, *analysis_revision_state{view: $view, revision, state}\n\
         candidate[id, kind, metadata] := selected_state[state], *state_entity{state, id, kind, metadata}, str_includes(id, $candidate)"
    )
);
search_entity_facts_query!(
    SearchRevisionEntityFacts,
    "entity_search.revision",
    ranked_entity_search!(
        "candidate[id, kind, metadata] := *analysis_revision{view: $view, revision}, *analysis_revision_entity{view: $view, revision, id, kind, metadata}, str_includes(id, $candidate)"
    )
);
search_entity_facts_query!(
    SearchShardEntityFacts,
    "entity_search.shard",
    ranked_entity_search!(
        "candidate[id, kind, metadata] := *analysis_fact_shard_entity:by_id{id, producer, owner, version, kind, metadata}, starts_with(id, $query), *analysis_fact_shard_selection:by_owner{view: $view, owner, producer, version}\n\
         candidate[id, kind, metadata] := *analysis_fact_shard_entity_name:by_name{name, producer, owner, version, id}, starts_with(name, $query), *analysis_fact_shard_selection:by_owner{view: $view, owner, producer, version}, *analysis_fact_shard_entity:by_id{id, producer, owner, version, kind, metadata}"
    )
);
search_entity_facts_query!(
    SearchEnrichmentEntityFacts,
    "entity_search.enrichment",
    ranked_entity_search!(
        "selected_enrichment[owner] := *analysis_revision{view: $view, revision}, *analysis_revision_repository_enrichment{view: $view, revision, owner}\n\
         candidate[id, kind, metadata] := *analysis_enrichment_entity_selection{view: $view, id, owner}, selected_enrichment[owner], *enrichment_entity_contribution{view: $view, owner, id, kind, metadata}, str_includes(id, $candidate)"
    )
);

pub(super) fn search_entity_facts(
    db: &impl QueryRunner,
    view: &str,
    query: &str,
    limit: u32,
) -> Result<NamedRows, Box<dyn Error>> {
    let candidate = query
        .split(['.', '/', ':'])
        .max_by_key(|part| part.len())
        .unwrap_or(query);
    let params = || SearchEntityFactsParams {
        candidate: candidate.into(),
        query: query.into(),
        limit,
    };
    let mut rows = db.run::<SearchStateEntityFacts>(view, params())?;
    rows.extend(db.run::<SearchRevisionEntityFacts>(view, params())?);
    let mut baseline_ids = rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<BTreeSet<_>>();
    let shard = db
        .run::<SearchShardEntityFacts>(view, params())?
        .into_iter()
        .filter(|row| !baseline_ids.contains(&row.id))
        .collect::<Vec<_>>();
    baseline_ids.extend(shard.iter().map(|row| row.id.clone()));
    rows.extend(shard);
    rows.extend(
        db.run::<SearchEnrichmentEntityFacts>(view, params())?
            .into_iter()
            .filter(|row| !baseline_ids.contains(&row.id)),
    );
    Ok(NamedRows::new(
        ENTITY_FACT_HEADERS
            .iter()
            .map(|header| (*header).into())
            .collect(),
        rows.into_iter().map(EntityFactRow::into_values).collect(),
    ))
}

pub(super) fn generated_entity_ids(
    db: &impl QueryRunner,
    view: &str,
    entities: BTreeSet<String>,
) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let mut generated = BTreeSet::new();
    // ponytail: search returns at most 100 entities; batch only if these exact lookups measure slow.
    for entity in entities {
        let mut baseline = BTreeSet::new();
        let rows = db.run::<StateStructuralContext>(view, context_params(&entity))?;
        baseline.extend(rows.iter().map(ContextRow::edge_key));
        if rows.iter().any(marks_generated_origin) {
            generated.insert(entity.clone());
            continue;
        }
        let rows = db.run::<RevisionStructuralContext>(view, context_params(&entity))?;
        baseline.extend(rows.iter().map(ContextRow::edge_key));
        if rows.iter().any(marks_generated_origin) {
            generated.insert(entity.clone());
            continue;
        }
        let rows = db.run::<FactShardStructuralContext>(view, context_params(&entity))?;
        baseline.extend(rows.iter().map(ContextRow::edge_key));
        if rows.iter().any(marks_generated_origin) {
            generated.insert(entity.clone());
            continue;
        }
        if db
            .run::<EnrichmentStructuralContext>(view, context_params(&entity))?
            .iter()
            .any(|row| !baseline.contains(&row.edge_key()) && marks_generated_origin(row))
        {
            generated.insert(entity);
        }
    }
    Ok(generated)
}

fn marks_generated_origin(row: &ContextRow) -> bool {
    row.provenance == "generated"
        && matches!(
            (row.direction.as_str(), row.relation.as_str()),
            ("incoming", "defines") | ("outgoing", "field_of")
        )
}

const WORKSPACE_TOPOLOGY_HEADERS: &[&str] = &[
    "from",
    "to",
    "relation",
    "evidence",
    "confidence",
    "provenance",
];

struct WorkspaceTopologyQuery;

impl MnesticQuery for WorkspaceTopologyQuery {
    const OPERATION: &'static str = "workspace_topology";
    const SCRIPT: &'static str = concat!(
        include_str!("../../../rules/core/direct.datalog"),
        "\n?[from, to, relation, evidence, confidence, provenance] := \
             effective_observation[from, to, relation, evidence, confidence, provenance]\n\
         :order from, to, relation, evidence"
    );
    const HEADERS: &'static [&'static str] = WORKSPACE_TOPOLOGY_HEADERS;

    type Params = ();
    type Row = Vec<DataValue>;

    fn bind(_: &Self::Params) -> BTreeMap<String, DataValue> {
        BTreeMap::new()
    }

    fn decode(_: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
        Ok(row.to_vec())
    }
}

pub(super) fn workspace_topology(
    db: &impl QueryRunner,
    view: &str,
) -> Result<NamedRows, Box<dyn Error>> {
    Ok(NamedRows::new(
        WORKSPACE_TOPOLOGY_HEADERS
            .iter()
            .map(|header| (*header).into())
            .collect(),
        db.run::<WorkspaceTopologyQuery>(view, ())?,
    ))
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

struct ContextParams {
    entity: String,
}

#[derive(Debug, PartialEq)]
pub(super) struct ContextRow {
    pub(super) direction: String,
    pub(super) relation: String,
    pub(super) related: String,
    pub(super) evidence: String,
    pub(super) confidence: f64,
    pub(super) provenance: String,
}

impl ContextRow {
    fn edge_key(&self) -> (String, String, String, String) {
        (
            self.direction.clone(),
            self.relation.clone(),
            self.related.clone(),
            self.evidence.clone(),
        )
    }
}

const CONTEXT_HEADERS: &[&str] = &[
    "direction",
    "relation",
    "related",
    "evidence",
    "confidence",
    "provenance",
];

macro_rules! context_query {
    ($name:ident, $operation:literal, $script:expr) => {
        struct $name;

        impl MnesticQuery for $name {
            const OPERATION: &'static str = $operation;
            const SCRIPT: &'static str = $script;
            const HEADERS: &'static [&'static str] = CONTEXT_HEADERS;

            type Params = ContextParams;
            type Row = ContextRow;

            fn bind(params: &Self::Params) -> BTreeMap<String, DataValue> {
                BTreeMap::from([("entity".into(), params.entity.as_str().into())])
            }

            fn decode(headers: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
                decode_context(Self::OPERATION, headers, row)
            }
        }
    };
}

context_query!(
    MaterializedContextDependencies,
    "context.dependencies",
    CONTEXT_QUERY
);
context_query!(
    StateStructuralContext,
    "context.structural.state",
    include_str!("../../../rules/core/context_structural_state.datalog")
);
context_query!(
    FactShardStructuralContext,
    "context.structural.fact_shard",
    include_str!("../../../rules/core/context_structural_fact_shard.datalog")
);
context_query!(
    RevisionStructuralContext,
    "context.structural.revision",
    include_str!("../../../rules/core/context_structural_revision.datalog")
);
context_query!(
    EnrichmentStructuralContext,
    "context.structural.enrichment",
    include_str!("../../../rules/core/context_structural_enrichment.datalog")
);

fn decode_context(
    operation: &'static str,
    headers: &[String],
    row: &[DataValue],
) -> Result<ContextRow, QueryError> {
    let string = |index: usize| {
        row.get(index)
            .and_then(DataValue::get_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                QueryError::new(
                    operation,
                    format!("column {} is not a string", headers[index]),
                )
            })
    };
    Ok(ContextRow {
        direction: string(0)?,
        relation: string(1)?,
        related: string(2)?,
        evidence: string(3)?,
        confidence: row
            .get(4)
            .and_then(DataValue::get_float)
            .ok_or_else(|| QueryError::new(operation, "column confidence is not a float"))?,
        provenance: string(5)?,
    })
}

fn context_params(entity: &str) -> ContextParams {
    ContextParams {
        entity: entity.to_owned(),
    }
}

pub(super) fn context(
    db: &impl QueryRunner,
    view: &str,
    entity: &str,
) -> Result<Vec<ContextRow>, Box<dyn Error>> {
    let mut result = db.run::<MaterializedContextDependencies>(view, context_params(entity))?;
    let mut baseline_structural = BTreeSet::new();
    for rows in [
        db.run::<StateStructuralContext>(view, context_params(entity))?,
        db.run::<FactShardStructuralContext>(view, context_params(entity))?,
        db.run::<RevisionStructuralContext>(view, context_params(entity))?,
    ] {
        baseline_structural.extend(rows.iter().map(ContextRow::edge_key));
        result.extend(rows);
    }
    result.extend(
        db.run::<EnrichmentStructuralContext>(view, context_params(entity))?
            .into_iter()
            .filter(|row| !baseline_structural.contains(&row.edge_key())),
    );
    result.sort_by(|left, right| {
        left.direction
            .cmp(&right.direction)
            .then_with(|| left.relation.cmp(&right.relation))
            .then_with(|| left.related.cmp(&right.related))
            .then_with(|| left.evidence.cmp(&right.evidence))
            .then_with(|| left.confidence.total_cmp(&right.confidence))
            .then_with(|| left.provenance.cmp(&right.provenance))
    });
    result.dedup();
    Ok(result)
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

struct ResolvedDependencyParams {
    frontier: BTreeSet<String>,
    limit: i64,
}

#[derive(Debug)]
struct ResolvedDependencyRow {
    from: String,
    to: String,
    relation: String,
    evidence: String,
    confidence: f64,
    provenance: String,
}

impl ResolvedDependencyRow {
    fn into_values(self, hops: u32) -> Vec<DataValue> {
        vec![
            "edge".into(),
            "".into(),
            i64::from(hops).into(),
            self.from.into(),
            self.to.into(),
            self.relation.into(),
            self.evidence.into(),
            self.confidence.into(),
            self.provenance.into(),
        ]
    }
}

const RESOLVED_DEPENDENCY_HEADERS: &[&str] = &[
    "from",
    "to",
    "relation",
    "evidence",
    "confidence",
    "provenance",
];

struct OutgoingResolvedDependencies;

impl MnesticQuery for OutgoingResolvedDependencies {
    const OPERATION: &'static str = "traversal.outgoing.materialized";
    const SCRIPT: &'static str = "frontier[id] <- $frontier\n\
         ?[from, to, relation, evidence, confidence, provenance] := frontier[from], \
             *analysis_resolved_dependency{\
                 view: $view, from, relation, to, evidence, confidence, provenance\
             }\n\
         :order from, to, relation, evidence, confidence, provenance\n\
         :limit $limit\n\
         :reorder written";
    const HEADERS: &'static [&'static str] = RESOLVED_DEPENDENCY_HEADERS;

    type Params = ResolvedDependencyParams;
    type Row = ResolvedDependencyRow;

    fn bind(params: &Self::Params) -> BTreeMap<String, DataValue> {
        bind_resolved_dependency_params(params)
    }

    fn decode(headers: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
        decode_resolved_dependency(Self::OPERATION, headers, row)
    }
}

struct IncomingResolvedDependencies;

impl MnesticQuery for IncomingResolvedDependencies {
    const OPERATION: &'static str = "traversal.incoming.materialized";
    const SCRIPT: &'static str = "frontier[id] <- $frontier\n\
         ?[from, to, relation, evidence, confidence, provenance] := frontier[to], \
             *analysis_resolved_dependency:by_to{\
                 view: $view, to, from, relation, evidence, confidence, provenance\
             }\n\
         ?[from, to, relation, evidence, confidence, provenance] := frontier[from], \
             starts_with(from, 'grpc://'), \
             *analysis_resolved_dependency{\
                 view: $view, from, relation: 'implemented_by', to, evidence, \
                 confidence, provenance\
             }, relation = 'implemented_by'\n\
         :order from, to, relation, evidence, confidence, provenance\n\
         :limit $limit\n\
         :reorder written";
    const HEADERS: &'static [&'static str] = RESOLVED_DEPENDENCY_HEADERS;

    type Params = ResolvedDependencyParams;
    type Row = ResolvedDependencyRow;

    fn bind(params: &Self::Params) -> BTreeMap<String, DataValue> {
        bind_resolved_dependency_params(params)
    }

    fn decode(headers: &[String], row: &[DataValue]) -> Result<Self::Row, QueryError> {
        decode_resolved_dependency(Self::OPERATION, headers, row)
    }
}

fn bind_resolved_dependency_params(
    params: &ResolvedDependencyParams,
) -> BTreeMap<String, DataValue> {
    BTreeMap::from([
        ("limit".into(), params.limit.into()),
        (
            "frontier".into(),
            DataValue::List(
                params
                    .frontier
                    .iter()
                    .map(|entity| DataValue::List(vec![entity.as_str().into()]))
                    .collect(),
            ),
        ),
    ])
}

fn decode_resolved_dependency(
    operation: &'static str,
    headers: &[String],
    row: &[DataValue],
) -> Result<ResolvedDependencyRow, QueryError> {
    let string = |index: usize| {
        row.get(index)
            .and_then(DataValue::get_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                QueryError::new(
                    operation,
                    format!("column {} is not a string", headers[index]),
                )
            })
    };
    Ok(ResolvedDependencyRow {
        from: string(0)?,
        to: string(1)?,
        relation: string(2)?,
        evidence: string(3)?,
        confidence: row
            .get(4)
            .and_then(DataValue::get_float)
            .ok_or_else(|| QueryError::new(operation, "column confidence is not a float"))?,
        provenance: string(5)?,
    })
}

fn closure(
    db: &impl QueryRunner,
    view: &str,
    entity: &str,
    max_hops: u32,
    direction: TraversalDirection,
) -> Result<NamedRows, Box<dyn Error>> {
    let mut frontier = BTreeSet::from([entity.to_owned()]);
    let mut visited = frontier.clone();
    let mut rows = Vec::new();
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
        let params = ResolvedDependencyParams {
            frontier: frontier.clone(),
            limit: i64::MAX,
        };
        let result = match direction {
            TraversalDirection::Outgoing => db.run::<OutgoingResolvedDependencies>(view, params)?,
            TraversalDirection::Incoming => db.run::<IncomingResolvedDependencies>(view, params)?,
        };
        if hops == max_hops {
            if let Some(row) = result
                .into_iter()
                .find(|row| !visited.contains(next_entity(row, direction)))
            {
                rows.push(row.into_values(hops));
            }
            break;
        }
        let mut next = BTreeSet::new();
        for row in &result {
            let entity = next_entity(row, direction);
            if visited.insert(entity.to_owned()) {
                next.insert(entity.to_owned());
            }
        }
        rows.extend(result.into_iter().map(|row| row.into_values(hops)));
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(NamedRows::new(
        [
            "row_kind",
            "entity",
            "hops",
            "edge_from",
            "edge_to",
            "relation",
            "evidence",
            "confidence",
            "provenance",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        rows,
    ))
}

/// Row limits bound returned data; the deadline also bounds sorting a very wide frontier.
struct TraversalQueryBudget<'a, Q> {
    inner: &'a Q,
    deadline: std::time::Instant,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TraversalState {
    entity: String,
    visited_targets: BTreeSet<String>,
    completion_repository: Option<String>,
}

type TargetReachability = BTreeMap<String, BTreeSet<String>>;
type MultipathAcquisition = (
    NamedRows,
    BTreeSet<String>,
    BTreeSet<String>,
    TargetReachability,
);

fn target_prefix(repository: &str) -> String {
    format!("repo://{repository}/")
}

fn dependency_rows(
    rows: NamedRows,
    offset: usize,
    operation: &'static str,
) -> Result<Vec<ResolvedDependencyRow>, Box<dyn Error>> {
    rows.rows
        .iter()
        .map(|row| decode_resolved_dependency(operation, &rows.headers[offset..], &row[offset..]))
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

fn reverse_seed(
    db: &impl QueryRunner,
    view: &str,
    target: &str,
    direction: TraversalDirection,
    limit: usize,
) -> Result<Vec<ResolvedDependencyRow>, Box<dyn Error>> {
    let prefix = target_prefix(target);
    let script = match direction {
        TraversalDirection::Outgoing => {
            "?[from, to, relation, evidence, confidence, provenance] := \
                 *analysis_resolved_dependency:by_to{\
                     view: $view, to, from, relation, evidence, confidence, provenance\
                 }, starts_with(to, $prefix)\n\
             :order from, to, relation, evidence, confidence, provenance\n\
             :limit $limit\n\
             :reorder written"
        }
        TraversalDirection::Incoming => {
            "regular[from, to, relation, evidence, confidence, provenance] := \
                 *analysis_resolved_dependency{\
                     view: $view, from, relation, to, evidence, confidence, provenance\
                 }, starts_with(from, $prefix), relation != 'implemented_by'\n\
             regular[from, to, relation, evidence, confidence, provenance] := \
                 *analysis_resolved_dependency{\
                     view: $view, from, relation: 'implemented_by', to, evidence, confidence, provenance\
                 }, starts_with(from, $prefix), not starts_with(from, 'grpc://'), relation = 'implemented_by'\n\
             regular[from, to, relation, evidence, confidence, provenance] := \
                 *analysis_resolved_dependency:by_to{\
                     view: $view, to, from, relation: 'implemented_by', evidence, confidence, provenance\
                 }, starts_with(to, $prefix), starts_with(from, 'grpc://'), relation = 'implemented_by'\n\
             ?[from, to, relation, evidence, confidence, provenance] := \
                 regular[from, to, relation, evidence, confidence, provenance]\n\
             :order from, to, relation, evidence, confidence, provenance\n\
             :limit $limit\n\
             :reorder written"
        }
    };
    dependency_rows(
        db.run_query(
            script,
            BTreeMap::from([
                ("view".into(), view.into()),
                ("prefix".into(), prefix.into()),
                ("limit".into(), i64::try_from(limit)?.into()),
            ]),
        )?,
        0,
        "traversal.repository_reachability.seed",
    )
}

fn reverse_frontier(
    db: &impl QueryRunner,
    view: &str,
    frontier: &BTreeSet<String>,
    direction: TraversalDirection,
    limit: usize,
) -> Result<Vec<ResolvedDependencyRow>, Box<dyn Error>> {
    let script = match direction {
        TraversalDirection::Outgoing => {
            "frontier[id] <- $frontier\n\
             ?[from, to, relation, evidence, confidence, provenance] := frontier[to], \
                 *analysis_resolved_dependency:by_to{\
                     view: $view, to, from, relation, evidence, confidence, provenance\
                 }\n\
             :order from, to, relation, evidence, confidence, provenance\n\
             :limit $limit\n\
             :reorder written"
        }
        TraversalDirection::Incoming => {
            "frontier[id] <- $frontier\n\
             regular[from, to, relation, evidence, confidence, provenance] := frontier[from], \
                 *analysis_resolved_dependency{\
                     view: $view, from, relation, to, evidence, confidence, provenance\
                 }, relation != 'implemented_by'\n\
             regular[from, to, relation, evidence, confidence, provenance] := frontier[from], \
                 *analysis_resolved_dependency{\
                     view: $view, from, relation: 'implemented_by', to, evidence, confidence, provenance\
                 }, not starts_with(from, 'grpc://'), relation = 'implemented_by'\n\
             regular[from, to, relation, evidence, confidence, provenance] := frontier[to], \
                 *analysis_resolved_dependency:by_to{\
                     view: $view, to, from, relation: 'implemented_by', evidence, confidence, provenance\
                 }, starts_with(from, 'grpc://'), relation = 'implemented_by'\n\
             ?[from, to, relation, evidence, confidence, provenance] := \
                 regular[from, to, relation, evidence, confidence, provenance]\n\
             :order from, to, relation, evidence, confidence, provenance\n\
             :limit $limit\n\
             :reorder written"
        }
    };
    dependency_rows(
        db.run_query(
            script,
            BTreeMap::from([
                ("view".into(), view.into()),
                (
                    "frontier".into(),
                    DataValue::List(
                        frontier
                            .iter()
                            .map(|entity| DataValue::List(vec![entity.as_str().into()]))
                            .collect(),
                    ),
                ),
                ("limit".into(), i64::try_from(limit)?.into()),
            ]),
        )?,
        0,
        "traversal.repository_reachability.frontier",
    )
}

fn traversal_predecessor(
    row: &ResolvedDependencyRow,
    direction: TraversalDirection,
) -> (&str, &str) {
    match direction {
        TraversalDirection::Outgoing => (&row.from, &row.to),
        TraversalDirection::Incoming
            if row.relation == "implemented_by" && row.from.starts_with("grpc://") =>
        {
            (&row.from, &row.to)
        }
        TraversalDirection::Incoming => (&row.to, &row.from),
    }
}

fn target_reachability(
    db: &impl QueryRunner,
    view: &str,
    query: &beholder_dto::TraverseGraphQuery,
    direction: TraversalDirection,
    remaining_rows: &mut usize,
) -> Result<Option<TargetReachability>, Box<dyn Error>> {
    let mut reachable = BTreeMap::<String, BTreeSet<String>>::new();
    for target in &query.target_repositories {
        if super::semantic::repository(&query.start).as_deref() == Some(target) {
            reachable
                .entry(query.start.clone())
                .or_default()
                .insert(target.clone());
            continue;
        }
        let seed = reverse_seed(db, view, target, direction, *remaining_rows + 1)?;
        if seed.len() > *remaining_rows {
            return Ok(None);
        }
        *remaining_rows -= seed.len();
        let mut frontier = BTreeSet::<String>::new();
        let mut visited = BTreeSet::<String>::new();
        for row in seed {
            let (predecessor, destination) = traversal_predecessor(&row, direction);
            reachable
                .entry(destination.into())
                .or_default()
                .insert(target.clone());
            reachable
                .entry(predecessor.into())
                .or_default()
                .insert(target.clone());
            visited.insert(destination.into());
            if visited.insert(predecessor.into()) {
                frontier.insert(predecessor.into());
            }
        }
        for _ in 1..query.max_hops {
            if frontier.is_empty() {
                break;
            }
            let rows = reverse_frontier(db, view, &frontier, direction, *remaining_rows + 1)?;
            if rows.len() > *remaining_rows {
                return Ok(None);
            }
            *remaining_rows -= rows.len();
            let mut next = BTreeSet::new();
            for row in rows {
                let (predecessor, _) = traversal_predecessor(&row, direction);
                reachable
                    .entry(predecessor.into())
                    .or_default()
                    .insert(target.clone());
                if visited.insert(predecessor.into()) {
                    next.insert(predecessor.into());
                }
            }
            frontier = next;
        }
    }
    Ok(Some(reachable))
}

struct ForwardAcquisition {
    edges: Vec<ResolvedDependencyRow>,
    boundaries: BTreeSet<String>,
}

fn filtered_forward(
    db: &impl QueryRunner,
    view: &str,
    states: &BTreeSet<TraversalState>,
    targets: &BTreeSet<String>,
    reachable: &BTreeMap<String, BTreeSet<String>>,
    direction: TraversalDirection,
    limit: usize,
) -> Result<ForwardAcquisition, Box<dyn Error>> {
    let mut active_rows = Vec::new();
    let mut incomplete_rows = Vec::new();
    let mut missing_rows = Vec::new();
    let mut completed_rows = Vec::new();
    for (index, state) in states.iter().enumerate() {
        let index = i64::try_from(index)?;
        active_rows.push(DataValue::List(vec![
            index.into(),
            state.entity.as_str().into(),
        ]));
        if let Some(repository) = &state.completion_repository {
            completed_rows.push(DataValue::List(vec![
                index.into(),
                state.entity.as_str().into(),
                target_prefix(repository).into(),
            ]));
        } else {
            incomplete_rows.push(DataValue::List(vec![
                index.into(),
                state.entity.as_str().into(),
            ]));
            missing_rows.extend(
                targets
                    .difference(&state.visited_targets)
                    .map(|target| DataValue::List(vec![index.into(), target.as_str().into()])),
            );
        }
    }
    let reachable_rows = reachable
        .iter()
        .flat_map(|(entity, targets)| {
            targets
                .iter()
                .map(|target| DataValue::List(vec![entity.as_str().into(), target.as_str().into()]))
        })
        .collect();
    let adjacency = match direction {
        TraversalDirection::Outgoing => {
            "candidate[state, current, next, from, to, relation, evidence, confidence, provenance] := \
                 active[state, from], current = from, next = to, \
                 *analysis_resolved_dependency{\
                     view: $view, from, relation, to, evidence, confidence, provenance\
                 }"
        }
        TraversalDirection::Incoming => {
            "candidate[state, current, next, from, to, relation, evidence, confidence, provenance] := \
                 active[state, to], current = to, next = from, \
                 *analysis_resolved_dependency:by_to{\
                     view: $view, to, from, relation, evidence, confidence, provenance\
                 }, relation != 'implemented_by'\n\
             candidate[state, current, next, from, to, relation, evidence, confidence, provenance] := \
                 active[state, to], current = to, next = from, \
                 *analysis_resolved_dependency:by_to{\
                     view: $view, to, from, relation: 'implemented_by', evidence, confidence, provenance\
                 }, not starts_with(from, 'grpc://'), relation = 'implemented_by'\n\
             candidate[state, current, next, from, to, relation, evidence, confidence, provenance] := \
                 active[state, from], starts_with(from, 'grpc://'), current = from, next = to, \
                 *analysis_resolved_dependency{\
                     view: $view, from, relation: 'implemented_by', to, evidence, confidence, provenance\
                 }, relation = 'implemented_by'"
        }
    };
    let script = format!(
        "active[state, current] <- $active\n\
         incomplete[state, current] <- $incomplete\n\
         missing[state, target] <- $missing\n\
         reachable[next, target] <- $reachable\n\
         completed[state, current, prefix] <- $completed\n\
         {adjacency}\n\
         blocked[state, next] := \
             candidate[state, _, next, _, _, _, _, _, _], \
             missing[state, target], not reachable[next, target]\n\
         allowed[from, to, relation, evidence, confidence, provenance] := \
             candidate[state, _, next, from, to, relation, evidence, confidence, provenance], \
             incomplete[state, _], not blocked[state, next]\n\
         allowed[from, to, relation, evidence, confidence, provenance] := \
             candidate[state, _, next, from, to, relation, evidence, confidence, provenance], \
             completed[state, _, prefix], starts_with(next, prefix)\n\
         allowed[from, to, relation, evidence, confidence, provenance] := \
             candidate[state, _, next, from, to, relation, evidence, confidence, provenance], \
             completed[state, _, _], not starts_with(next, 'repo://')\n\
         boundary[current] := candidate[state, current, next, _, _, _, _, _, _], \
             completed[state, current, prefix], starts_with(next, 'repo://'), \
             not starts_with(next, prefix)\n\
         ?[row_kind, from, to, relation, evidence, confidence, provenance] := \
             allowed[from, to, relation, evidence, confidence, provenance], row_kind = 'edge'\n\
         ?[row_kind, from, to, relation, evidence, confidence, provenance] := \
             boundary[from], row_kind = 'boundary', to = '', relation = '', evidence = '', \
             confidence = 0.0, provenance = ''\n\
         :order row_kind, from, to, relation, evidence, confidence, provenance\n\
         :limit $limit\n\
         :reorder written"
    );
    let rows = db.run_query(
        &script,
        BTreeMap::from([
            ("view".into(), view.into()),
            ("active".into(), DataValue::List(active_rows)),
            ("incomplete".into(), DataValue::List(incomplete_rows)),
            ("missing".into(), DataValue::List(missing_rows)),
            ("reachable".into(), DataValue::List(reachable_rows)),
            ("completed".into(), DataValue::List(completed_rows)),
            ("limit".into(), i64::try_from(limit)?.into()),
        ]),
    )?;
    let mut edges = Vec::new();
    let mut boundaries = BTreeSet::new();
    for row in &rows.rows {
        match row.first().and_then(DataValue::get_str) {
            Some("edge") => edges.push(decode_resolved_dependency(
                "traversal.repository_filter.forward",
                &rows.headers[1..],
                &row[1..],
            )?),
            Some("boundary") => {
                boundaries.insert(
                    row.get(1)
                        .and_then(DataValue::get_str)
                        .ok_or("repository boundary entity is not a string")?
                        .into(),
                );
            }
            _ => return Err("unknown repository-filtered traversal row".into()),
        }
    }
    Ok(ForwardAcquisition { edges, boundaries })
}

fn filtered_multipath_rows(
    db: &impl QueryRunner,
    view: &str,
    query: &beholder_dto::TraverseGraphQuery,
    direction: TraversalDirection,
) -> Result<MultipathAcquisition, Box<dyn Error>> {
    let targets = query
        .target_repositories
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut remaining_rows = beholder_dto::MAX_TRAVERSAL_ROWS as usize;
    let Some(reachable) = target_reachability(db, view, query, direction, &mut remaining_rows)?
    else {
        return Ok((
            empty_multipath_rows(),
            BTreeSet::from([query.start.clone()]),
            BTreeSet::new(),
            BTreeMap::new(),
        ));
    };
    let mut visited_targets = BTreeSet::new();
    if let Some(repository) = super::semantic::repository(&query.start)
        && targets.contains(&repository)
    {
        visited_targets.insert(repository);
    }
    let missing = targets
        .difference(&visited_targets)
        .cloned()
        .collect::<BTreeSet<_>>();
    if !missing.is_subset(reachable.get(&query.start).unwrap_or(&BTreeSet::new())) {
        return Ok((
            empty_multipath_rows(),
            BTreeSet::new(),
            BTreeSet::new(),
            reachable,
        ));
    }
    let completion_repository = missing
        .is_empty()
        .then(|| super::semantic::repository(&query.start))
        .flatten();
    let initial = TraversalState {
        entity: query.start.clone(),
        visited_targets,
        completion_repository,
    };
    let mut found_completion = initial.completion_repository.is_some();
    let mut states = BTreeSet::from([initial.clone()]);
    let mut visited_states = BTreeSet::from([initial]);
    let mut rows = Vec::new();
    let mut incomplete = BTreeSet::new();
    let mut boundaries = BTreeSet::new();
    for hops in 0..=query.max_hops {
        if states.is_empty() {
            break;
        }
        let result = filtered_forward(
            db,
            view,
            &states,
            &targets,
            &reachable,
            direction,
            remaining_rows + states.len() + 1,
        )?;
        boundaries.extend(result.boundaries);
        if result.edges.len() > remaining_rows {
            incomplete.extend(states.iter().map(|state| state.entity.clone()));
            break;
        }
        remaining_rows -= result.edges.len();
        let mut next_states = BTreeSet::new();
        for row in &result.edges {
            let (current, next) = match direction {
                TraversalDirection::Outgoing => (row.from.as_str(), row.to.as_str()),
                TraversalDirection::Incoming
                    if row.relation == "implemented_by" && row.from.starts_with("grpc://") =>
                {
                    (row.from.as_str(), row.to.as_str())
                }
                TraversalDirection::Incoming => (row.to.as_str(), row.from.as_str()),
            };
            for state in states.iter().filter(|state| state.entity == current) {
                let mut next_state = state.clone();
                next_state.entity = next.into();
                if let Some(completion) = &state.completion_repository {
                    if super::semantic::repository(next).as_deref() != Some(completion) {
                        continue;
                    }
                } else if let Some(repository) = super::semantic::repository(next)
                    && targets.contains(&repository)
                {
                    next_state.visited_targets.insert(repository.clone());
                    if next_state.visited_targets == targets {
                        next_state.completion_repository = Some(repository);
                        found_completion = true;
                    }
                }
                let missing = targets
                    .difference(&next_state.visited_targets)
                    .cloned()
                    .collect::<BTreeSet<_>>();
                if !missing.is_empty()
                    && !missing.is_subset(reachable.get(next).unwrap_or(&BTreeSet::new()))
                {
                    continue;
                }
                if visited_states.insert(next_state.clone()) {
                    next_states.insert(next_state);
                }
            }
        }
        rows.extend(result.edges.into_iter().map(|row| row.into_values(hops)));
        if hops == query.max_hops {
            if !next_states.is_empty() {
                incomplete.extend(next_states.iter().map(|state| state.entity.clone()));
            }
            break;
        }
        states = next_states;
    }
    if !found_completion {
        rows.clear();
        boundaries.clear();
    }
    Ok((
        multipath_named_rows(rows),
        incomplete,
        boundaries,
        reachable,
    ))
}

fn empty_multipath_rows() -> NamedRows {
    multipath_named_rows(Vec::new())
}

fn multipath_named_rows(rows: Vec<Vec<DataValue>>) -> NamedRows {
    NamedRows::new(
        [
            "row_kind",
            "entity",
            "hops",
            "edge_from",
            "edge_to",
            "relation",
            "evidence",
            "confidence",
            "provenance",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        rows,
    )
}

impl<Q: QueryRunner> QueryRunner for TraversalQueryBudget<'_, Q> {
    fn run_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
    ) -> Result<NamedRows, Box<dyn Error>> {
        let remaining = self
            .deadline
            .saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "graph acquisition deadline exceeded",
            )
            .into());
        }
        self.inner.run_query(
            &format!("{script}\n:timeout {}", remaining.as_secs_f64()),
            params,
        )
    }
}

/// Acquire the boundary frontier too, so path-local cycles and depth are exact.
/// The row cap bounds materialization; incomplete frontiers never imply leaves.
pub(super) fn multipath_rows(
    db: &impl QueryRunner,
    view: &str,
    query: &beholder_dto::TraverseGraphQuery,
) -> Result<MultipathAcquisition, Box<dyn Error>> {
    let deadline = std::time::Instant::now()
        + Duration::from_millis(u64::from(beholder_dto::TRAVERSAL_ACQUISITION_TIMEOUT_MS));
    let db = TraversalQueryBudget {
        inner: db,
        deadline,
    };
    let direction = match query.direction {
        beholder_dto::GraphDirection::Dependencies => TraversalDirection::Outgoing,
        beholder_dto::GraphDirection::Dependents => TraversalDirection::Incoming,
    };
    if !query.target_repositories.is_empty() {
        return filtered_multipath_rows(&db, view, query, direction);
    }
    let mut frontier = BTreeSet::from([query.start.clone()]);
    let mut visited = frontier.clone();
    let mut rows = Vec::new();
    let mut incomplete = BTreeSet::new();
    for hops in 0..=query.max_hops {
        if let Some(destination) = &query.destination {
            frontier.remove(destination);
        }
        if frontier.is_empty() {
            break;
        }

        let remaining = beholder_dto::MAX_TRAVERSAL_ROWS as usize - rows.len();
        let params = ResolvedDependencyParams {
            frontier: frontier.clone(),
            limit: (remaining + 1) as i64,
        };
        let mut result = match direction {
            TraversalDirection::Outgoing => db.run::<OutgoingResolvedDependencies>(view, params),
            TraversalDirection::Incoming => db.run::<IncomingResolvedDependencies>(view, params),
        }
        .map_err(|error| -> Box<dyn Error> {
            if std::time::Instant::now() >= deadline {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "graph acquisition deadline exceeded",
                )
                .into()
            } else {
                Box::new(error)
            }
        })?;
        if result.len() > remaining {
            let cut = &result[remaining];
            let key = (cut.from.clone(), cut.to.clone(), cut.relation.clone());
            result.truncate(remaining);
            result.retain(|row| (&row.from, &row.to, &row.relation) != (&key.0, &key.1, &key.2));
            incomplete.extend(frontier.iter().cloned());
        }
        let mut next = BTreeSet::new();
        for row in &result {
            let entity = next_entity(row, direction);
            if visited.insert(entity.to_owned()) {
                next.insert(entity.to_owned());
            }
        }
        rows.extend(result.into_iter().map(|row| row.into_values(hops)));
        if !incomplete.is_empty() {
            incomplete.extend(next);
            break;
        }
        if hops == query.max_hops || next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok((
        multipath_named_rows(rows),
        incomplete,
        BTreeSet::new(),
        BTreeMap::new(),
    ))
}

fn next_entity(row: &ResolvedDependencyRow, direction: TraversalDirection) -> &str {
    match direction {
        TraversalDirection::Outgoing => &row.to,
        TraversalDirection::Incoming
            if row.relation == "implemented_by" && row.from.starts_with("grpc://") =>
        {
            &row.to
        }
        TraversalDirection::Incoming => &row.from,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::DIRECT_RULES;
    use crate::{SemanticStore, database::memory_database};
    use beholder_domain::{
        DependencyRelation, EntityFact, EntityKind, LogicalRepository, Observation,
        RepositoryFacts, RepositoryState, StructuralRelation, WorkspaceView,
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

    struct WrongShapeRunner;

    impl QueryRunner for WrongShapeRunner {
        fn run_query(
            &self,
            _script: &str,
            _params: BTreeMap<String, DataValue>,
        ) -> Result<NamedRows, Box<dyn Error>> {
            Ok(NamedRows::new(vec!["to".into(), "from".into()], vec![]))
        }
    }

    struct WrongScalarWidthRunner;

    impl QueryRunner for WrongScalarWidthRunner {
        fn run_query(
            &self,
            _script: &str,
            _params: BTreeMap<String, DataValue>,
        ) -> Result<NamedRows, Box<dyn Error>> {
            Ok(NamedRows::new(vec!["revision".into()], vec![Vec::new()]))
        }
    }

    struct WrongEntityHeadersRunner;

    impl QueryRunner for WrongEntityHeadersRunner {
        fn run_query(
            &self,
            _script: &str,
            _params: BTreeMap<String, DataValue>,
        ) -> Result<NamedRows, Box<dyn Error>> {
            Ok(NamedRows::new(
                vec!["kind".into(), "id".into(), "metadata".into()],
                Vec::new(),
            ))
        }
    }

    struct SummaryMetadataRunner;

    impl QueryRunner for SummaryMetadataRunner {
        fn run_query(
            &self,
            script: &str,
            _params: BTreeMap<String, DataValue>,
        ) -> Result<NamedRows, Box<dyn Error>> {
            if script.contains("count(code)") {
                return Ok(NamedRows::new(
                    vec!["severity".into(), "count(code)".into()],
                    vec![vec!["warning".into(), 2_i64.into()]],
                ));
            }
            if script.contains(":order severity, repository") {
                panic!("summary metadata must not load diagnostic details");
            }
            Ok(NamedRows::new(
                vec!["incomplete".into()],
                vec![vec![DataValue::Bool(true)]],
            ))
        }
    }

    struct UnexpectedQueryRunner;

    impl QueryRunner for UnexpectedQueryRunner {
        fn run_query(
            &self,
            _script: &str,
            _params: BTreeMap<String, DataValue>,
        ) -> Result<NamedRows, Box<dyn Error>> {
            panic!("target already visited by start must not query reverse reachability");
        }
    }

    #[test]
    fn target_reachability_skips_a_repository_visited_by_start() {
        let start = "repo://example/a/rust/lib/start";
        let query = beholder_dto::TraverseGraphQuery {
            start: start.into(),
            direction: beholder_dto::GraphDirection::Dependencies,
            destination: None,
            target_repositories: vec!["example/a".into()],
            max_hops: 8,
            max_paths: 50,
        };
        let mut remaining = beholder_dto::MAX_TRAVERSAL_ROWS as usize;

        let reachable = target_reachability(
            &UnexpectedQueryRunner,
            "main",
            &query,
            TraversalDirection::Outgoing,
            &mut remaining,
        )
        .unwrap()
        .unwrap();

        assert_eq!(reachable[start], BTreeSet::from(["example/a".into()]));
        assert_eq!(remaining, beholder_dto::MAX_TRAVERSAL_ROWS as usize);
    }

    #[test]
    fn summary_analysis_metadata_counts_without_loading_details() {
        let metadata = analysis_metadata(
            &SummaryMetadataRunner,
            "main",
            1,
            AnalysisMetadataOptions {
                include_diagnostics: false,
            },
        )
        .unwrap();

        assert_eq!(metadata.completeness, AnalysisCompleteness::Incomplete);
        assert!(metadata.diagnostics.is_empty());
        assert_eq!(metadata.diagnostic_counts.total, 2);
        assert_eq!(metadata.diagnostic_counts.warnings, 2);
    }

    #[test]
    fn typed_query_rejects_an_incorrect_output_shape() {
        let error = WrongShapeRunner
            .run::<OutgoingResolvedDependencies>(
                "main",
                ResolvedDependencyParams {
                    limit: i64::MAX,
                    frontier: BTreeSet::new(),
                },
            )
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "Mnestic query traversal.outgoing.materialized failed: unexpected output headers: \
             expected [\"from\", \"to\", \"relation\", \"evidence\", \"confidence\", \
             \"provenance\"], got [\"to\", \"from\"]"
        );
    }

    #[test]
    fn analysis_revision_rejects_an_incorrect_row_width() {
        let error = WrongScalarWidthRunner
            .run::<AnalysisRevisionQuery>("main", ())
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "Mnestic query analysis_revision failed: unexpected row width at row 0: expected 1, got 0"
        );
    }

    #[test]
    fn entity_facts_rejects_an_incorrect_output_shape() {
        let error = WrongEntityHeadersRunner
            .run::<BaselineEntityFacts>(
                "main",
                EntityFactsParams {
                    entities: BTreeSet::new(),
                },
            )
            .unwrap_err();

        assert!(
            error.to_string().starts_with(
                "Mnestic query entity_facts.baseline failed: unexpected output headers"
            )
        );
    }

    #[test]
    fn multipath_acquisition_deadline_interrupts_database_work() {
        let db = DbInstance::new("mem", "", Default::default()).unwrap();
        let transaction = db.multi_transaction(false);
        let budget = TraversalQueryBudget {
            inner: &transaction,
            deadline: std::time::Instant::now() + Duration::from_millis(10),
        };
        let error = budget.run_query(
            "n[x] := x = 0\nn[y] := n[x], y = x + 1, y < 100000000\n?[x] := n[x]\n:order -x\n:limit 1",
            BTreeMap::new(),
        ).unwrap_err();
        assert!(
            error.to_string().contains("time budget") || error.to_string().contains("deadline"),
            "{error}"
        );
        assert_eq!(
            transaction
                .run_script("?[x] <- [[1]]", BTreeMap::new())
                .unwrap()
                .rows
                .len(),
            1
        );
        transaction.abort().unwrap();
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

        let _ = observed_bound_query(
            &runner,
            QuerySpec::new("test", "main", "?[value] <- [[1]]"),
            None,
            || {
                parameter_builds.set(parameter_builds.get() + 1);
                BTreeMap::new()
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
        let entities = observations
            .iter()
            .flat_map(|observation| [&observation.from, &observation.to])
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|id| EntityFact::new(id.clone(), EntityKind::Callable, None).unwrap())
            .collect();
        RepositoryFacts {
            state: view.repository_states[0].clone(),
            analysis_identity: "analysis".into(),
            incomplete: false,
            diagnostics: Vec::new(),
            entities,
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
        let transaction = db.multi_transaction(true);
        crate::storage::rebuild_resolved_dependencies(&transaction, "diamond").unwrap();
        transaction.commit().unwrap();

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
