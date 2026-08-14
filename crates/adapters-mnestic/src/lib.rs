use beholder_domain::{
    DependencyOverride, FactChanges, Observation, RepositoryFacts, RepositoryState, WorkspaceView,
};
use beholder_dto::{ContextResult, DependenciesResult, ImpactResult, Revisioned, TraceResult};
use mnestic_engine::{
    DataValue, DbInstance, MultiTransaction, NamedRows, Num, ScriptMutability, ScriptRunOptions,
};
use std::{collections::BTreeMap, error::Error, path::Path, time::Instant};

mod semantic;

const MAX_HOPS: i64 = 32;

#[derive(Clone, Debug, PartialEq)]
pub enum InspectionValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<InspectionValue>),
    Other(String),
}

impl InspectionValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InspectionResult {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<InspectionValue>>,
    pub next: Option<Box<InspectionResult>>,
}

pub struct SemanticStore {
    db: DbInstance,
}

impl SemanticStore {
    pub fn memory() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            db: memory_database()?,
        })
    }

    pub fn persistent(path: &Path, initialize: bool) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            db: persistent_database(path, initialize)?,
        })
    }

    pub fn benchmark_store(storage: &str, path: Option<&str>) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            db: benchmark_database(storage, path)?,
        })
    }

    pub fn view_matches(&self, view: &WorkspaceView) -> Result<bool, Box<dyn Error>> {
        view_matches(&self.db, view)
    }

    pub fn publish(
        &self,
        view: &WorkspaceView,
        repositories: &[RepositoryFacts],
        overrides: &[DependencyOverride],
    ) -> Result<FactChanges, Box<dyn Error>> {
        publish_observations(&self.db, view, repositories, overrides)
    }

    pub fn inspect_relations(&self) -> Result<InspectionResult, Box<dyn Error>> {
        inspect_relations(&self.db).map(inspection_result)
    }

    pub fn inspect_revisions(&self) -> Result<InspectionResult, Box<dyn Error>> {
        inspect_revisions(&self.db).map(inspection_result)
    }

    pub fn inspect_observations(
        &self,
        relation: Option<&str>,
    ) -> Result<InspectionResult, Box<dyn Error>> {
        inspect_observations(&self.db, relation).map(inspection_result)
    }

    pub fn context(&self, view: &str, entity: &str) -> Result<ContextResult, Box<dyn Error>> {
        semantic::context(
            view,
            entity,
            inspection_result(context(&self.db, view, entity)?),
        )
    }

    pub fn context_snapshot(
        &self,
        view: &str,
        entity: &str,
    ) -> Result<Revisioned<ContextResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            semantic::context(
                view,
                entity,
                inspection_result(context(transaction, view, entity)?),
            )
        })
    }

    pub fn trace(&self, view: &str, from: &str, to: &str) -> Result<TraceResult, Box<dyn Error>> {
        semantic::trace(
            view,
            from,
            to,
            inspection_result(trace(&self.db, view, from, to)?),
        )
    }

    pub fn trace_snapshot(
        &self,
        view: &str,
        from: &str,
        to: &str,
    ) -> Result<Revisioned<TraceResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            semantic::trace(
                view,
                from,
                to,
                inspection_result(trace(transaction, view, from, to)?),
            )
        })
    }

    pub fn impact(&self, view: &str, entity: &str) -> Result<ImpactResult, Box<dyn Error>> {
        semantic::impact(
            view,
            entity,
            inspection_result(impact(&self.db, view, entity)?),
        )
    }

    pub fn impact_snapshot(
        &self,
        view: &str,
        entity: &str,
    ) -> Result<Revisioned<ImpactResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            semantic::impact(
                view,
                entity,
                inspection_result(impact(transaction, view, entity)?),
            )
        })
    }

    pub fn dependencies(
        &self,
        view: &str,
        entity: &str,
    ) -> Result<DependenciesResult, Box<dyn Error>> {
        semantic::dependencies(
            view,
            entity,
            inspection_result(dependencies(&self.db, view, entity)?),
        )
    }

    pub fn dependencies_snapshot(
        &self,
        view: &str,
        entity: &str,
    ) -> Result<Revisioned<DependenciesResult>, Box<dyn Error>> {
        self.snapshot(view, |transaction| {
            semantic::dependencies(
                view,
                entity,
                inspection_result(dependencies(transaction, view, entity)?),
            )
        })
    }

    fn snapshot<T>(
        &self,
        view: &str,
        read: impl FnOnce(&MultiTransaction) -> Result<T, Box<dyn Error>>,
    ) -> Result<Revisioned<T>, Box<dyn Error>> {
        let transaction = self.db.multi_transaction(false);
        let result = read(&transaction)?;
        let analysis_revision = analysis_revision(&transaction, view)?;
        transaction.abort()?;
        Ok(Revisioned {
            result,
            analysis_revision,
        })
    }

    pub fn benchmark(
        &self,
        topology: &str,
        entities: i64,
        fanout: i64,
        depth: i64,
    ) -> Result<String, Box<dyn Error>> {
        benchmark(&self.db, topology, entities, fanout, depth)
    }

    pub fn benchmark_queries(&self, topology: &str, entities: i64, depth: i64) -> String {
        benchmark_queries(&self.db, topology, entities, depth)
    }
}

#[cfg(feature = "devtools")]
pub fn explain(path: &Path, query: &str) -> Result<InspectionResult, Box<dyn Error>> {
    let db = persistent_database(path, false)?;
    db.run_script(
        &format!("::explain {{\n{query}\n}}"),
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )
    .map(inspection_result)
    .map_err(Into::into)
}

fn inspection_result(rows: NamedRows) -> InspectionResult {
    InspectionResult {
        headers: rows.headers,
        rows: rows
            .rows
            .into_iter()
            .map(|row| row.into_iter().map(inspection_value).collect())
            .collect(),
        next: rows.next.map(|next| Box::new(inspection_result(*next))),
    }
}

fn inspection_value(value: DataValue) -> InspectionValue {
    match value {
        DataValue::Null => InspectionValue::Null,
        DataValue::Bool(value) => InspectionValue::Boolean(value),
        DataValue::Num(Num::Int(value)) => InspectionValue::Integer(value),
        DataValue::Num(Num::Float(value)) => InspectionValue::Float(value),
        DataValue::Str(value) => InspectionValue::String(value.into()),
        DataValue::Bytes(value) => InspectionValue::Bytes(value),
        DataValue::List(values) => {
            InspectionValue::List(values.into_iter().map(inspection_value).collect())
        }
        value => InspectionValue::Other(value.to_string()),
    }
}

const CREATE_SCHEMA: &str = r#"
:create state_observation {
    state: String,
    from: String,
    relation: String,
    to: String,
    =>
    evidence: String,
}
"#;

const CREATE_DEPENDENCY_SCHEMA: &str = r#"
:create state_dependency_observation {
    state: String,
    from: String,
    relation: String,
    to: String,
    =>
    evidence: String,
}
"#;

const CREATE_METADATA_SCHEMA: &str = r#"
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

const CREATE_OBSERVATION_TO_INDEX: &str =
    "::index create state_observation:by_to {to, state, from, relation, evidence}";
const CREATE_METADATA_TO_INDEX: &str = "::index create state_observation_metadata:by_to \
     {to, state, from, relation, confidence, provenance}";

const CREATE_OVERRIDE_SCHEMA: &str = r#"
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

const CREATE_OVERRIDE_METADATA_SCHEMA: &str = r#"
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

const CREATE_REVISION_SCHEMA: &str = r#"
:create analysis_revision {
    view: String,
    =>
    revision: Int,
}
"#;

const CREATE_FINGERPRINT_SCHEMA: &str = r#"
:create analysis_fingerprint {
    view: String,
    =>
    fingerprint: String,
}
"#;

const CREATE_REPOSITORY_STATE_SCHEMA: &str = r#"
:create repository_state {
    fingerprint: String,
    =>
    repository: String,
    head: String,
}
"#;

const CREATE_REVISION_STATE_SCHEMA: &str = r#"
:create analysis_revision_state {
    view: String,
    revision: Int,
    repository: String,
    =>
    state: String,
}
"#;

const SEED: &str = r#"
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

const SEED_DEPENDENCIES: &str = r#"
?[state, from, relation, to, evidence] :=
    *state_observation{state, from, relation, to, evidence}
:put state_dependency_observation {state, from, relation, to => evidence}
"#;

const SEED_METADATA: &str = r#"
?[state, from, relation, to, confidence, provenance] :=
    *state_observation{state, from, relation, to},
    confidence = 1.0,
    provenance = 'ast'
:put state_observation_metadata {state, from, relation, to => confidence, provenance}
"#;

const SEED_REVISIONS: &str = r#"
?[view, revision] <- [['main', 0], ['feature', 0]]
:put analysis_revision {view => revision}
"#;

const SEED_STATES: &str = r#"
?[view, revision, repository, state] <- [
    ['main', 0, 'seed', 'seed-main'],
    ['feature', 0, 'seed', 'seed-feature'],
]
:put analysis_revision_state {view, revision, repository => state}
"#;

const DIRECT_RULES: &str = include_str!("../../../rules/core/direct.datalog");
const DEPENDENCY_RULES: &str = include_str!("../../../rules/core/dependencies.datalog");
const IMPACT_RULES: &str = include_str!("../../../rules/core/impact.datalog");
const CONTEXT_QUERY: &str = "selected_state[state] := \
         *analysis_revision{view: $view, revision}, \
         *analysis_revision_state{view: $view, revision, state}\n\
     context_override[from, relation, unresolved_to, resolved_to, evidence, confidence, provenance] := \
         *analysis_revision{view: $view, revision}, \
         *analysis_revision_dependency_override{\
             view: $view, revision, from, relation, unresolved_to, resolved_to, evidence\
         }, \
         *analysis_revision_dependency_override_metadata{\
             view: $view, revision, from, relation, unresolved_to, confidence, provenance\
         }\n\
     overridden[from, relation, unresolved_to, evidence] := \
         context_override[from, relation, unresolved_to, _, evidence, _, _]\n\
     ?[direction, relation, related, evidence, confidence, provenance] := \
         selected_state[state], \
         *state_observation{\
             state, from: $entity, relation, to: related, evidence\
         }, \
         *state_observation_metadata{\
             state, from: $entity, relation, to: related, confidence, provenance\
         }, \
         not overridden[$entity, relation, related, evidence], \
         direction = 'outgoing'\n\
     ?[direction, relation, related, evidence, confidence, provenance] := \
         context_override[\
             $entity, relation, _, related, evidence, confidence, provenance\
         ], \
         direction = 'outgoing'\n\
     ?[direction, relation, related, evidence, confidence, provenance] := \
         selected_state[state], \
         *state_observation:by_to{\
             state, from: related, relation, to: $entity, evidence\
         }, \
         *state_observation_metadata:by_to{\
             state, from: related, relation, to: $entity, confidence, provenance\
         }, \
         not overridden[related, relation, $entity, evidence], \
         direction = 'incoming'\n\
     ?[direction, relation, related, evidence, confidence, provenance] := \
         context_override[\
             related, relation, _, $entity, evidence, confidence, provenance\
         ], \
         direction = 'incoming'\n\
     :order direction, relation, related";

fn memory_database() -> Result<DbInstance, Box<dyn Error>> {
    let db = DbInstance::new("mem", "", Default::default())?;
    db.run_script(CREATE_SCHEMA, BTreeMap::new(), ScriptMutability::Mutable)?;
    db.run_script(
        CREATE_DEPENDENCY_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_METADATA_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_OBSERVATION_TO_INDEX,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_METADATA_TO_INDEX,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_OVERRIDE_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_OVERRIDE_METADATA_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_REVISION_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        CREATE_REVISION_STATE_SCHEMA,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(SEED, BTreeMap::new(), ScriptMutability::Mutable)?;
    db.run_script(
        SEED_DEPENDENCIES,
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )?;
    db.run_script(SEED_METADATA, BTreeMap::new(), ScriptMutability::Mutable)?;
    db.run_script(SEED_REVISIONS, BTreeMap::new(), ScriptMutability::Mutable)?;
    db.run_script(SEED_STATES, BTreeMap::new(), ScriptMutability::Mutable)?;
    Ok(db)
}

fn benchmark_database(storage: &str, path: Option<&str>) -> Result<DbInstance, Box<dyn Error>> {
    #[cfg(not(feature = "sqlite"))]
    let _ = path;
    match storage {
        "mem" => Ok(DbInstance::new("mem", "", Default::default())?),
        #[cfg(feature = "sqlite")]
        "sqlite" => Ok(DbInstance::new(
            "sqlite",
            path.ok_or("sqlite benchmark requires a database path")?,
            Default::default(),
        )?),
        #[cfg(not(feature = "sqlite"))]
        "sqlite" => Err("build with --features sqlite to benchmark SQLite".into()),
        _ => Err("storage must be mem or sqlite".into()),
    }
}

fn persistent_database(path: &Path, initialize: bool) -> Result<DbInstance, Box<dyn Error>> {
    if path.as_os_str().is_empty() {
        return Err("database path must not be empty".into());
    }
    let is_new = !path.exists();
    if is_new && !initialize {
        return Err(format!("database does not exist: {}", path.display()).into());
    }
    let db = benchmark_database("sqlite", path.to_str())?;
    if is_new {
        db.run_script(CREATE_SCHEMA, BTreeMap::new(), ScriptMutability::Mutable)?;
    }
    let relations = db.run_script("::relations", BTreeMap::new(), ScriptMutability::Immutable)?;
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("state_observation"))
    {
        db.run_script(CREATE_SCHEMA, BTreeMap::new(), ScriptMutability::Mutable)?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("state_observation_metadata"))
    {
        db.run_script(
            CREATE_METADATA_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("state_dependency_observation"))
    {
        db.run_script(
            CREATE_DEPENDENCY_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("analysis_revision_dependency_override_metadata"))
    {
        db.run_script(
            CREATE_OVERRIDE_METADATA_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("analysis_revision_dependency_override"))
    {
        db.run_script(
            CREATE_OVERRIDE_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("analysis_revision"))
    {
        db.run_script(
            CREATE_REVISION_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
        db.run_script(
            "?[view, revision] <- [['main', 0]] \
             :put analysis_revision {view => revision}",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("analysis_fingerprint"))
    {
        db.run_script(
            CREATE_FINGERPRINT_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
        db.run_script(
            "?[view, fingerprint] <- [['main', '']] \
             :put analysis_fingerprint {view => fingerprint}",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("repository_state"))
    {
        db.run_script(
            CREATE_REPOSITORY_STATE_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize
        && !relations
            .rows
            .iter()
            .any(|row| row[0].get_str() == Some("analysis_revision_state"))
    {
        db.run_script(
            CREATE_REVISION_STATE_SCHEMA,
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )?;
    }
    if initialize {
        let relations =
            db.run_script("::relations", BTreeMap::new(), ScriptMutability::Immutable)?;
        for (name, script) in [
            ("state_observation:by_to", CREATE_OBSERVATION_TO_INDEX),
            ("state_observation_metadata:by_to", CREATE_METADATA_TO_INDEX),
        ] {
            if !relations
                .rows
                .iter()
                .any(|row| row[0].get_str() == Some(name))
            {
                db.run_script(script, BTreeMap::new(), ScriptMutability::Mutable)?;
            }
        }
    }
    Ok(db)
}

fn store_observations(
    transaction: &MultiTransaction,
    state: &str,
    observations: &[Observation],
) -> Result<(), Box<dyn Error>> {
    // ponytail: one transaction, per-row writes; batch when ingestion throughput matters.
    for observation in observations {
        let params = BTreeMap::from([
            ("state".into(), state.into()),
            ("from".into(), observation.from.as_str().into()),
            ("relation".into(), observation.relation.as_str().into()),
            ("to".into(), observation.to.as_str().into()),
            ("evidence".into(), observation.evidence.as_str().into()),
            ("confidence".into(), observation.confidence.score().into()),
            ("provenance".into(), observation.provenance.as_str().into()),
        ]);
        transaction.run_script(
            "?[state, from, relation, to, evidence] <- [[$state, $from, $relation, $to, $evidence]]\n\
             :put state_observation {state, from, relation, to => evidence}",
            params.clone(),
        )?;
        if observation.relation.dependency().is_some() {
            transaction.run_script(
                "?[state, from, relation, to, evidence] <- [[$state, $from, $relation, $to, $evidence]]\n\
                 :put state_dependency_observation {state, from, relation, to => evidence}",
                params.clone(),
            )?;
        }
        transaction.run_script(
            "?[state, from, relation, to, confidence, provenance] <- [\
                 [$state, $from, $relation, $to, $confidence, $provenance]\
             ]\n\
             :put state_observation_metadata {\
                 state, from, relation, to => confidence, provenance\
             }",
            params,
        )?;
    }
    Ok(())
}

fn analyzed_state(analysis_identity: &str, state: &RepositoryState) -> String {
    format!(
        "{}:{}{}",
        analysis_identity.len(),
        analysis_identity,
        state.fingerprint
    )
}

fn view_matches(db: &DbInstance, view: &WorkspaceView) -> Result<bool, Box<dyn Error>> {
    let rows = db.run_script(
        "?[matches] := *analysis_fingerprint{view: $view, fingerprint: stored}, \
             matches = stored == $fingerprint",
        BTreeMap::from([
            ("view".into(), view.name.clone().into()),
            ("fingerprint".into(), view.fingerprint().into()),
        ]),
        ScriptMutability::Immutable,
    )?;
    Ok(rows.rows.first().is_some_and(|row| row[0] == true.into()))
}

fn publish_observations(
    db: &DbInstance,
    view: &WorkspaceView,
    repositories: &[RepositoryFacts],
    overrides: &[DependencyOverride],
) -> Result<FactChanges, Box<dyn Error>> {
    if repositories
        .iter()
        .any(|facts| facts.analysis_identity.is_empty())
        || repositories.len() != view.repository_states.len()
        || view.repository_states.iter().any(|state| {
            repositories
                .iter()
                .filter(|facts| facts.state == *state)
                .count()
                != 1
        })
    {
        return Err("repository facts do not match the workspace view".into());
    }
    let params = BTreeMap::from([
        ("view".into(), view.name.clone().into()),
        ("fingerprint".into(), view.fingerprint().into()),
    ]);
    let transaction = db.multi_transaction(true);
    let current = transaction.run_script(
        &format!(
            "{DIRECT_RULES}\n\
             ?[from, relation, to, evidence] := \
                 effective_observation[from, to, relation, evidence, _, _]"
        ),
        BTreeMap::from([("view".into(), view.name.clone().into())]),
    )?;
    let current = current
        .rows
        .into_iter()
        .map(|row| {
            let value = |index: usize| {
                row[index]
                    .get_str()
                    .map(str::to_owned)
                    .ok_or("observation contains a non-string value")
            };
            Ok(((value(0)?, value(1)?, value(2)?), value(3)?))
        })
        .collect::<Result<BTreeMap<_, _>, Box<dyn Error>>>()?;
    let override_targets = overrides
        .iter()
        .map(|override_| {
            (
                (
                    override_.from.as_str().to_owned(),
                    override_.relation.as_str().to_owned(),
                    override_.unresolved_to.as_str().to_owned(),
                    override_.evidence.as_str().to_owned(),
                ),
                override_.resolved_to.as_str().to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let next = repositories
        .iter()
        .flat_map(|facts| &facts.observations)
        .map(|observation| {
            let from = observation.from.as_str().to_owned();
            let relation = observation.relation.as_str().to_owned();
            let unresolved_to = observation.to.as_str().to_owned();
            let evidence = observation.evidence.as_str().to_owned();
            let to = override_targets
                .get(&(
                    from.clone(),
                    relation.clone(),
                    unresolved_to.clone(),
                    evidence.clone(),
                ))
                .cloned()
                .unwrap_or(unresolved_to);
            ((from, relation, to), evidence)
        })
        .collect::<BTreeMap<_, _>>();
    let mut changes = FactChanges::default();
    for (key, evidence) in &next {
        match current.get(key) {
            None => changes.inserted += 1,
            Some(current) if current == evidence => changes.unchanged += 1,
            Some(_) => changes.updated += 1,
        }
    }
    changes.removed = current
        .keys()
        .filter(|key| !next.contains_key(*key))
        .count();

    for facts in repositories {
        let state = analyzed_state(&facts.analysis_identity, &facts.state);
        let stored = transaction.run_script(
            "?[stored] := *repository_state{fingerprint: $state}, stored = true",
            BTreeMap::from([("state".into(), state.clone().into())]),
        )?;
        if stored.rows.is_empty() {
            store_observations(&transaction, &state, &facts.observations)?;
        }
    }
    transaction.run_script(
        "?[view, revision] := \
             *analysis_revision{view: $view, revision: previous}, \
             view = $view, revision = previous + 1\n\
         ?[view, revision] := \
             not *analysis_revision{view: $view}, view = $view, revision = 1\n\
         :put analysis_revision {view => revision}",
        params.clone(),
    )?;
    transaction.run_script(
        "?[view, fingerprint] <- [[$view, $fingerprint]] \
         :put analysis_fingerprint {view => fingerprint}",
        params,
    )?;
    store_repository_states(&transaction, view, repositories)?;
    for override_ in overrides {
        let params = BTreeMap::from([
            ("view".into(), view.name.clone().into()),
            ("from".into(), override_.from.as_str().into()),
            ("relation".into(), override_.relation.as_str().into()),
            (
                "unresolved_to".into(),
                override_.unresolved_to.as_str().into(),
            ),
            ("resolved_to".into(), override_.resolved_to.as_str().into()),
            ("evidence".into(), override_.evidence.as_str().into()),
            ("confidence".into(), override_.confidence.score().into()),
            ("provenance".into(), override_.provenance.as_str().into()),
        ]);
        transaction.run_script(
            "?[view, revision, from, relation, unresolved_to, resolved_to, evidence] := \
                 *analysis_revision{view: $view, revision}, \
                 view = $view, from = $from, relation = $relation, unresolved_to = $unresolved_to, \
                 resolved_to = $resolved_to, evidence = $evidence\n\
             :put analysis_revision_dependency_override {\
                 view, revision, from, relation, unresolved_to => resolved_to, evidence\
             }",
            params.clone(),
        )?;
        transaction.run_script(
            "?[view, revision, from, relation, unresolved_to, confidence, provenance] := \
                 *analysis_revision{view: $view, revision}, \
                 view = $view, from = $from, relation = $relation, \
                 unresolved_to = $unresolved_to, confidence = $confidence, \
                 provenance = $provenance\n\
             :put analysis_revision_dependency_override_metadata {\
                 view, revision, from, relation, unresolved_to => confidence, provenance\
             }",
            params,
        )?;
    }
    transaction.commit()?;
    Ok(changes)
}

fn store_repository_states(
    transaction: &MultiTransaction,
    view: &WorkspaceView,
    repositories: &[RepositoryFacts],
) -> Result<(), Box<dyn Error>> {
    for facts in repositories {
        let state = &facts.state;
        let params = BTreeMap::from([
            ("view".into(), view.name.clone().into()),
            (
                "repository".into(),
                state.repository.identity.clone().into(),
            ),
            ("head".into(), state.head.clone().unwrap_or_default().into()),
            (
                "state".into(),
                analyzed_state(&facts.analysis_identity, state).into(),
            ),
        ]);
        transaction.run_script(
            "?[fingerprint, repository, head] <- [[$state, $repository, $head]]\n\
             :put repository_state {fingerprint => repository, head}",
            params.clone(),
        )?;
        transaction.run_script(
            "?[view, revision, repository, state] := \
                 *analysis_revision{view: $view, revision}, \
                 view = $view, repository = $repository, state = $state\n\
             :put analysis_revision_state {view, revision, repository => state}",
            params,
        )?;
    }
    Ok(())
}

trait QueryRunner {
    fn run_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
    ) -> Result<NamedRows, Box<dyn Error>>;
}

impl QueryRunner for DbInstance {
    fn run_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
    ) -> Result<NamedRows, Box<dyn Error>> {
        Ok(self.run_script(script, params, ScriptMutability::Immutable)?)
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

fn inspect_relations(db: &DbInstance) -> Result<NamedRows, Box<dyn Error>> {
    Ok(db.run_script("::relations", BTreeMap::new(), ScriptMutability::Immutable)?)
}

fn inspect_revisions(db: &DbInstance) -> Result<NamedRows, Box<dyn Error>> {
    Ok(db.run_script(
        "?[view, revision, fingerprint, repository, head, state] := \
             *analysis_revision{view, revision}, \
             *analysis_fingerprint{view, fingerprint}, \
             *analysis_revision_state{view, revision, repository, state}, \
             *repository_state{fingerprint: state, repository, head}\n\
         :order view, repository",
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )?)
}

fn analysis_revision(db: &impl QueryRunner, view: &str) -> Result<u64, Box<dyn Error>> {
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

fn inspect_observations(
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
            "?[state, from, relation, to, evidence] := \
                 *state_observation{{state, from, relation, to, evidence}}{filter}\n\
             :order relation, from, to"
        ),
        params,
        ScriptMutability::Immutable,
    )?)
}

fn context(db: &impl QueryRunner, view: &str, entity: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(db, view, CONTEXT_QUERY, [("entity", entity.into())])
}

fn trace(
    db: &impl QueryRunner,
    view: &str,
    from: &str,
    to: &str,
) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        view,
        &format!(
            "{DIRECT_RULES}\n{DEPENDENCY_RULES}\n\
             start[] <- [[$from]]\n\
             predecessor[to, smallest_by(candidate)] := distance[to, hops], hops > 0, \
                 distance[from, previous_hops], previous_hops + 1 == hops, \
                 direct[from, to, _, _, _, _], candidate = [from, from]\n\
             path[to, nodes] := predecessor[to, $from], nodes = [$from, to]\n\
             path[to, nodes] := path[from, previous_nodes], predecessor[to, from], \
                 nodes = append(previous_nodes, to)\n\
             steps[nodes, step] := path[$to, nodes], \
                 index in int_range(length(nodes) - 1), \
                 from = get(nodes, index), to = get(nodes, index + 1), \
                 direct[from, to, relation, evidence, confidence, provenance], \
                 step = [index, relation, evidence, confidence, provenance]\n\
             ?[nodes, collect(step), hops] := steps[nodes, step], hops = length(nodes) - 1"
        ),
        [
            ("from", from.into()),
            ("to", to.into()),
            ("max_hops", MAX_HOPS.into()),
        ],
    )
}

fn impact(db: &impl QueryRunner, view: &str, entity: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        view,
        &format!(
            "{DIRECT_RULES}\n{IMPACT_RULES}\n\
             start[] <- [[$from]]\n\
             selected[node, hops] := distance[node, hops]\n\
             ?[row_kind, entity, hops, edge_from, edge_to, relation, evidence, confidence, provenance] := \
                 selected[entity, hops], hops > 0, row_kind = 'entity', \
                 edge_from = '', edge_to = '', relation = '', evidence = '', \
                 confidence = 1.0, provenance = 'ast'\n\
             ?[row_kind, entity, hops, edge_from, edge_to, relation, evidence, confidence, provenance] := \
                 selected[edge_from, _], selected[edge_to, _], \
                 direct[\
                     edge_from, edge_to, relation, evidence, confidence, provenance\
                 ], row_kind = 'edge', \
                 entity = '', hops = 0\n\
             :order row_kind, hops, entity, edge_from, edge_to, relation"
        ),
        [("from", entity.into()), ("max_hops", MAX_HOPS.into())],
    )
}

fn dependencies(
    db: &impl QueryRunner,
    view: &str,
    entity: &str,
) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        view,
        &format!(
            "{DIRECT_RULES}\n{DEPENDENCY_RULES}\n\
             start[] <- [[$from]]\n\
             selected[node, hops] := distance[node, hops]\n\
             ?[row_kind, entity, hops, edge_from, edge_to, relation, evidence, confidence, provenance] := \
                 selected[entity, hops], hops > 0, row_kind = 'entity', \
                 edge_from = '', edge_to = '', relation = '', evidence = '', \
                 confidence = 1.0, provenance = 'ast'\n\
             ?[row_kind, entity, hops, edge_from, edge_to, relation, evidence, confidence, provenance] := \
                 selected[edge_from, _], selected[edge_to, _], \
                 direct[\
                     edge_from, edge_to, relation, evidence, confidence, provenance\
                 ], row_kind = 'edge', \
                 entity = '', hops = 0\n\
             :order row_kind, hops, entity, edge_from, edge_to, relation"
        ),
        [("from", entity.into()), ("max_hops", MAX_HOPS.into())],
    )
}

fn benchmark(
    db: &DbInstance,
    topology: &str,
    entities: i64,
    fanout: i64,
    depth: i64,
) -> Result<String, Box<dyn Error>> {
    let started = Instant::now();
    db.run_script(
        "?[id] := id in int_range($entities) :create benchmark_entity {id: Int}",
        BTreeMap::from([("entities".into(), entities.into())]),
        ScriptMutability::Mutable,
    )?;
    let edge_query = match topology {
        "linear" => {
            "?[from, to, evidence] := from in int_range($entities - 1), to = from + 1, evidence = from \
             :create benchmark_edge {from: Int, to: Int => evidence: Int}"
        }
        "tree" => {
            "?[from, to, evidence] := from in int_range($entities), offset in int_range(1, $fanout + 1), \
             to = from * $fanout + offset, to < $entities, evidence = from * $fanout + offset \
             :create benchmark_edge {from: Int, to: Int => evidence: Int}"
        }
        "dag" => {
            "?[from, to, evidence] := from in int_range($entities), offset in int_range(1, $fanout + 1), \
             to = from + offset, to < $entities, evidence = from * $fanout + offset \
             :create benchmark_edge {from: Int, to: Int => evidence: Int}"
        }
        "corpus" => {
            "?[from, to, evidence] := from in int_range($entities - 1), to = from + 1, evidence = from\n\
             ?[from, to, evidence] := from in int_range($entities - 2), from % 2 == 0, to = from + 2, evidence = from\n\
             ?[from, to, evidence] := from in int_range($entities - 7), from % 10 == 0, \
                 offset in int_range(3, 8), to = from + offset, evidence = from * 10 + offset\n\
             ?[from, to, evidence] := from in int_range($entities - 158), from % 1000 == 0, \
                 offset in int_range(8, 158), to = from + offset, evidence = from * 1000 + offset\n\
             :create benchmark_edge {from: Int, to: Int => evidence: Int}"
        }
        _ => return Err("topology must be linear, tree, dag, or corpus".into()),
    };
    db.run_script(
        edge_query,
        BTreeMap::from([
            ("entities".into(), entities.into()),
            ("fanout".into(), fanout.into()),
        ]),
        ScriptMutability::Mutable,
    )?;
    let loaded_in = started.elapsed();

    Ok(format!(
        "algorithm={}, topology={topology}, entities={entities}, fanout={fanout}, depth={depth}, load={loaded_in:?}, {}",
        "bounded-distance",
        benchmark_queries(db, topology, entities, depth)
    ))
}

fn benchmark_queries(db: &DbInstance, _topology: &str, entities: i64, depth: i64) -> String {
    let params = BTreeMap::from([
        ("depth".into(), depth.into()),
        ("target".into(), depth.min(entities - 1).into()),
    ]);
    let reachability = "reachable[to, hops] := *benchmark_edge{from: 0, to}, hops = 1\n\
         reachable[to, hops] := reachable[from, previous_hops], previous_hops < $depth, \
             *benchmark_edge{from, to}, hops = previous_hops + 1\n\
         ?[count_unique(to)] := reachable[to, _]";
    let closure = timed_query(db, reachability, params.clone());
    let context = timed_query(
        db,
        "?[count(to)] := *benchmark_edge{from: 0, to}",
        BTreeMap::new(),
    );
    let trace = timed_query(
        db,
        "start[] <- [[0]]\n\
         distance[node, min(hops)] := start[node], hops = 0\n\
         distance[to, min(hops)] := distance[from, previous_hops], previous_hops < $depth, \
             *benchmark_edge{from, to}, hops = previous_hops + 1\n\
         predecessor[to, min(from)] := distance[to, hops], hops > 0, \
             distance[from, previous_hops], previous_hops + 1 == hops, \
             *benchmark_edge{from, to}\n\
         path[to, nodes] := predecessor[to, 0], nodes = [0, to]\n\
         path[to, nodes] := path[from, previous_nodes], predecessor[to, from], \
             nodes = append(previous_nodes, to)\n\
         steps[nodes, step] := path[$target, nodes], \
             index in int_range(length(nodes) - 1), \
             from = get(nodes, index), to = get(nodes, index + 1), \
             *benchmark_edge{from, to, evidence}, step = [index, evidence]\n\
         ?[nodes, collect(step), hops] := steps[nodes, step], hops = length(nodes) - 1",
        params.clone(),
    );
    let impact = timed_query(db, reachability, params);

    format!("context={context}, closure={closure}, trace={trace}, impact={impact}")
}

fn timed_query(db: &DbInstance, script: &str, params: BTreeMap<String, DataValue>) -> String {
    let started = Instant::now();
    match db.run_script_with_options(
        script,
        params,
        ScriptMutability::Immutable,
        ScriptRunOptions::new().with_timeout(5.0),
    ) {
        Ok(rows) => format!("{:?} (rows={})", started.elapsed(), rows.rows.len()),
        Err(error) => format!("{:?} ({error})", started.elapsed()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{
        Confidence, DependencyRelation, LogicalRepository, Provenance, StructuralRelation,
    };
    use std::{collections::BTreeSet, fs, time::SystemTime};

    fn facts(view: &WorkspaceView, observations: Vec<Observation>) -> RepositoryFacts {
        RepositoryFacts {
            state: view.repository_states[0].clone(),
            analysis_identity: "analysis".into(),
            observations,
        }
    }

    #[test]
    fn impact_traverses_dependants() {
        let store = SemanticStore::memory().unwrap();
        let result = store.impact("main", "rpc/Pricing.GetPrice").unwrap();
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
             ]
             :put state_dependency_observation {state, from, relation, to => evidence}",
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

        let result = trace(&db, "diamond", "start", "end").unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][2], 2.into());
    }

    #[test]
    fn typed_trace_deduplicates_graph_and_resolves_path_references() {
        let result = SemanticStore::memory()
            .unwrap()
            .trace("main", "web/CheckoutPage", "cache/update_price")
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
        let db = memory_database().unwrap();
        let feature = query(
            &db,
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

        let store = SemanticStore { db };
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
    fn publish_replaces_only_changed_facts() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-fact-replacement-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store = SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap();
        let repository_state = |fingerprint: &str| RepositoryState {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            head: Some("head".into()),
            fingerprint: fingerprint.into(),
        };
        let observation = |from: &str, to: &str, evidence: &str| {
            Observation::dependency(from, DependencyRelation::Calls, to, evidence)
        };

        let first = WorkspaceView::new("main", "analysis", vec![repository_state("one")]).unwrap();
        assert_eq!(
            store
                .publish(
                    &first,
                    &[facts(
                        &first,
                        vec![
                            observation("repo/a", "repo/b", "a.rs:1"),
                            observation("repo/removed", "repo/b", "removed.rs:1"),
                        ],
                    )],
                    &[],
                )
                .unwrap(),
            FactChanges {
                inserted: 2,
                updated: 0,
                removed: 0,
                unchanged: 0,
            }
        );

        let second = WorkspaceView::new("main", "analysis", vec![repository_state("two")]).unwrap();
        assert_eq!(
            store
                .publish(
                    &second,
                    &[facts(
                        &second,
                        vec![
                            observation("repo/a", "repo/b", "a.rs:2"),
                            observation("repo/new", "repo/b", "new.rs:1"),
                        ],
                    )],
                    &[],
                )
                .unwrap(),
            FactChanges {
                inserted: 1,
                updated: 1,
                removed: 1,
                unchanged: 0,
            }
        );
        let observations = store.inspect_observations(None).unwrap();
        assert_eq!(observations.rows.len(), 4);
        assert!(format!("{observations:?}").contains("repo/removed"));
        assert!(format!("{observations:?}").contains("a.rs:2"));
        assert!(
            store
                .context("main", "repo/removed")
                .unwrap()
                .edges
                .is_empty()
        );
        assert!(
            store
                .inspect_revisions()
                .unwrap()
                .rows
                .iter()
                .any(|row| row[1].as_i64() == Some(2))
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn repository_state_facts_are_reused_across_views() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-state-reuse-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store = SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap();
        let state = RepositoryState {
            repository: LogicalRepository {
                identity: "repo".into(),
            },
            head: Some("head".into()),
            fingerprint: "shared".into(),
        };
        let observation = Observation::dependency(
            "repo/source",
            DependencyRelation::Calls,
            "repo/target",
            "src/lib.rs:1",
        );
        for name in ["first", "second"] {
            let view =
                WorkspaceView::new(name, format!("workspace-rules:{name}"), vec![state.clone()])
                    .unwrap();
            store
                .publish(
                    &view,
                    &[RepositoryFacts {
                        state: state.clone(),
                        analysis_identity: "analysis".into(),
                        observations: vec![observation.clone()],
                    }],
                    &[],
                )
                .unwrap();
            assert_eq!(store.context(name, "repo/source").unwrap().edges.len(), 1);
        }

        assert_eq!(store.inspect_observations(None).unwrap().rows.len(), 1);
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn workspace_override_connects_selected_repository_states() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-state-join-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store = SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap();
        let source = RepositoryState {
            repository: LogicalRepository {
                identity: "source".into(),
            },
            head: Some("source-head".into()),
            fingerprint: "source-state".into(),
        };
        let target = RepositoryState {
            repository: LogicalRepository {
                identity: "target".into(),
            },
            head: Some("target-head".into()),
            fingerprint: "target-state".into(),
        };
        let view =
            WorkspaceView::new("joined", "analysis", vec![source.clone(), target.clone()]).unwrap();
        let unresolved = Observation::dependency(
            "repo://source/rust/lib/caller",
            DependencyRelation::Calls,
            "rust-call://helper",
            "src/lib.rs:1",
        );
        let resolved = "repo://target/rust/lib/helper";
        store
            .publish(
                &view,
                &[
                    RepositoryFacts {
                        state: source,
                        analysis_identity: "analysis".into(),
                        observations: vec![unresolved.clone()],
                    },
                    RepositoryFacts {
                        state: target,
                        analysis_identity: "analysis".into(),
                        observations: vec![Observation::structural(
                            "repo://target/rust/lib",
                            StructuralRelation::Defines,
                            resolved,
                            "src/lib.rs:1",
                        )],
                    },
                ],
                &[DependencyOverride {
                    from: unresolved.from,
                    relation: DependencyRelation::Calls,
                    unresolved_to: unresolved.to,
                    resolved_to: resolved.into(),
                    evidence: unresolved.evidence,
                    confidence: Confidence::Inferred,
                    provenance: Provenance::UniqueNameHeuristic,
                }],
            )
            .unwrap();

        let context = store
            .context("joined", "repo://source/rust/lib/caller")
            .unwrap();
        let edge = context
            .edges
            .iter()
            .find(|edge| edge.to == resolved)
            .unwrap();
        assert_eq!(edge.confidence, 0.6);
        assert_eq!(
            edge.evidence[0].source_kind,
            beholder_dto::EvidenceKind::Inference
        );
        assert_eq!(
            edge.evidence[0].detail.as_deref(),
            Some("unique_name_heuristic")
        );
        let context = format!("{context:?}");
        assert!(context.contains(resolved));
        assert!(!context.contains("rust-call://helper"));
        assert_eq!(
            store
                .trace("joined", "repo://source/rust/lib/caller", resolved)
                .unwrap()
                .paths
                .len(),
            1
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn query_snapshot_keeps_rows_and_revision_consistent() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-query-snapshot-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store = SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap();
        let view = WorkspaceView::new(
            "snapshot",
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
                    vec![Observation::dependency(
                        "repo/source",
                        DependencyRelation::Calls,
                        "repo/target",
                        "source.rs:1",
                    )],
                )],
                &[],
            )
            .unwrap();
        let snapshot = store.context_snapshot("snapshot", "repo/source").unwrap();

        assert_eq!(snapshot.analysis_revision, 1);
        assert!(
            snapshot
                .result
                .nodes
                .iter()
                .any(|value| value.id == "repo/target")
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
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
                .trace("structural", "repo/file", "repo/target")
                .unwrap()
                .paths
                .is_empty()
        );
        assert_eq!(
            store
                .trace("structural", "repo/caller", "repo/target")
                .unwrap()
                .paths
                .len(),
            1
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn existing_database_accepts_repository_state_facts() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-backfill-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let path = state_dir.join("beholder.db");
        let db = benchmark_database("sqlite", path.to_str()).unwrap();
        db.run_script(
            ":create observation {\
                 view: String, from: String, relation: String, to: String => evidence: String\
             }",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .unwrap();
        drop(db);

        let store = SemanticStore::persistent(&path, true).unwrap();
        let view = WorkspaceView::new(
            "legacy",
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
                .trace("legacy", "repo/caller", "repo/target")
                .unwrap()
                .paths
                .len(),
            1
        );
        assert!(
            store
                .trace("legacy", "repo/file", "repo/target")
                .unwrap()
                .paths
                .is_empty()
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }
}
