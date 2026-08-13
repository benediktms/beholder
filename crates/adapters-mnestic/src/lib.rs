use beholder_domain::{FactChanges, Observation, RepositoryState, WorkspaceView};
use beholder_dto::{QueryResult, QueryValue};
use mnestic_engine::{
    DataValue, DbInstance, MultiTransaction, NamedRows, Num, ScriptMutability, ScriptRunOptions,
};
use std::{collections::BTreeMap, error::Error, path::Path, time::Instant};

const MAX_HOPS: i64 = 32;

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
        observations: &[Observation],
    ) -> Result<FactChanges, Box<dyn Error>> {
        publish_observations(&self.db, view, observations)
    }

    pub fn inspect_relations(&self) -> Result<QueryResult, Box<dyn Error>> {
        inspect_relations(&self.db).map(query_result)
    }

    pub fn inspect_revisions(&self) -> Result<QueryResult, Box<dyn Error>> {
        inspect_revisions(&self.db).map(query_result)
    }

    pub fn analysis_revision(&self, view: &str) -> Result<u64, Box<dyn Error>> {
        analysis_revision(&self.db, view)
    }

    pub fn inspect_observations(
        &self,
        relation: Option<&str>,
    ) -> Result<QueryResult, Box<dyn Error>> {
        inspect_observations(&self.db, relation).map(query_result)
    }

    pub fn context(&self, view: &str, entity: &str) -> Result<QueryResult, Box<dyn Error>> {
        context(&self.db, view, entity).map(query_result)
    }

    pub fn trace(&self, view: &str, from: &str, to: &str) -> Result<QueryResult, Box<dyn Error>> {
        trace(&self.db, view, from, to).map(query_result)
    }

    pub fn impact(&self, view: &str, entity: &str) -> Result<QueryResult, Box<dyn Error>> {
        impact(&self.db, view, entity).map(query_result)
    }

    pub fn dependencies(&self, view: &str, entity: &str) -> Result<QueryResult, Box<dyn Error>> {
        dependencies(&self.db, view, entity).map(query_result)
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

fn query_result(rows: NamedRows) -> QueryResult {
    QueryResult {
        headers: rows.headers,
        rows: rows
            .rows
            .into_iter()
            .map(|row| row.into_iter().map(query_value).collect())
            .collect(),
        next: rows.next.map(|next| Box::new(query_result(*next))),
        metadata: None,
    }
}

fn query_value(value: DataValue) -> QueryValue {
    match value {
        DataValue::Null => QueryValue::Null,
        DataValue::Bool(value) => QueryValue::Boolean(value),
        DataValue::Num(Num::Int(value)) => QueryValue::Integer(value),
        DataValue::Num(Num::Float(value)) => QueryValue::Float(value),
        DataValue::Str(value) => QueryValue::String(value.into()),
        DataValue::Bytes(value) => QueryValue::Bytes(value),
        DataValue::List(values) => QueryValue::List(values.into_iter().map(query_value).collect()),
        value => QueryValue::Other(value.to_string()),
    }
}

const CREATE_SCHEMA: &str = r#"
:create observation {
    view: String,
    from: String,
    relation: String,
    to: String,
    =>
    evidence: String,
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
?[view, from, relation, to, evidence] <- [
    ['main', 'web/CheckoutPage', 'uses', 'web/CheckoutQuery', 'CheckoutPage.tsx:12'],
    ['main', 'web/CheckoutQuery', 'selects', 'graphql/Query.checkout', 'CheckoutQuery.graphql:2'],
    ['main', 'graphql/Query.checkout', 'resolved_by', 'bff/CheckoutResolver.checkout', 'schema.ex:41'],
    ['main', 'bff/CheckoutResolver.checkout', 'calls', 'rpc/Pricing.GetPrice', 'checkout_resolver.ex:28'],
    ['main', 'rpc/Pricing.GetPrice', 'implemented_by', 'pricing/get_price', 'pricing.proto:9'],
    ['main', 'pricing/get_price', 'publishes', 'topic/pricing.updated', 'get_price.rs:18'],
    ['main', 'topic/pricing.updated', 'consumed_by', 'cache/update_price', 'consumer.rs:7'],
    ['feature', 'rpc/Pricing.GetPrice', 'implemented_by', 'pricing/get_price_v2', 'pricing.proto:9'],
]
:put observation {view, from, relation, to => evidence}
"#;

const DIRECT_RULES: &str = include_str!("../../../rules/core/direct.datalog");
const DEPENDENCY_RULES: &str = include_str!("../../../rules/core/dependencies.datalog");
const IMPACT_RULES: &str = include_str!("../../../rules/core/impact.datalog");

fn memory_database() -> Result<DbInstance, Box<dyn Error>> {
    let db = DbInstance::new("mem", "", Default::default())?;
    db.run_script(CREATE_SCHEMA, BTreeMap::new(), ScriptMutability::Mutable)?;
    db.run_script(SEED, BTreeMap::new(), ScriptMutability::Mutable)?;
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
    Ok(db)
}

fn store_observations(
    transaction: &MultiTransaction,
    view: &str,
    observations: &[Observation],
) -> Result<(), Box<dyn Error>> {
    // ponytail: one transaction, per-row writes; batch when ingestion throughput matters.
    for observation in observations {
        transaction.run_script(
            "?[view, from, relation, to, evidence] <- [[$view, $from, $relation, $to, $evidence]]\n\
             :put observation {view, from, relation, to => evidence}",
            BTreeMap::from([
                ("view".into(), view.into()),
                ("from".into(), observation.from.clone().into()),
                ("relation".into(), observation.relation.clone().into()),
                ("to".into(), observation.to.clone().into()),
                ("evidence".into(), observation.evidence.clone().into()),
            ]),
        )?;
    }
    Ok(())
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
    observations: &[Observation],
) -> Result<FactChanges, Box<dyn Error>> {
    let params = BTreeMap::from([
        ("view".into(), view.name.clone().into()),
        ("fingerprint".into(), view.fingerprint().into()),
    ]);
    let transaction = db.multi_transaction(true);
    let current = transaction.run_script(
        "?[from, relation, to, evidence] := \
             *observation{view: $view, from, relation, to, evidence}",
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
    let next = observations
        .iter()
        .map(|observation| {
            (
                (
                    observation.from.clone(),
                    observation.relation.clone(),
                    observation.to.clone(),
                ),
                observation.evidence.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let removed = current
        .keys()
        .filter(|key| !next.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    // ponytail: compare the completed view in memory and write per changed fact; stage server-side when graph size makes this scan material.
    for (from, relation, to) in &removed {
        transaction.run_script(
            "?[view, from, relation, to] <- [[$view, $from, $relation, $to]]\n\
             :rm observation {view, from, relation, to}",
            BTreeMap::from([
                ("view".into(), view.name.clone().into()),
                ("from".into(), from.clone().into()),
                ("relation".into(), relation.clone().into()),
                ("to".into(), to.clone().into()),
            ]),
        )?;
    }
    let mut changes = FactChanges {
        removed: removed.len(),
        ..FactChanges::default()
    };
    let mut changed = Vec::new();
    for ((from, relation, to), evidence) in &next {
        match current.get(&(from.clone(), relation.clone(), to.clone())) {
            None => changes.inserted += 1,
            Some(current) if current == evidence => {
                changes.unchanged += 1;
                continue;
            }
            Some(_) => changes.updated += 1,
        }
        changed.push(Observation {
            from: from.clone(),
            relation: relation.clone(),
            to: to.clone(),
            evidence: evidence.clone(),
        });
    }
    store_observations(&transaction, &view.name, &changed)?;
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
    store_repository_states(&transaction, view)?;
    transaction.commit()?;
    Ok(changes)
}

fn store_repository_states(
    transaction: &MultiTransaction,
    view: &WorkspaceView,
) -> Result<(), Box<dyn Error>> {
    for RepositoryState {
        repository,
        head,
        fingerprint,
    } in &view.repository_states
    {
        let params = BTreeMap::from([
            ("view".into(), view.name.clone().into()),
            ("repository".into(), repository.identity.clone().into()),
            ("head".into(), head.clone().unwrap_or_default().into()),
            ("state".into(), fingerprint.clone().into()),
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

fn query(
    db: &DbInstance,
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
    Ok(db.run_script(script, params, ScriptMutability::Immutable)?)
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

fn analysis_revision(db: &DbInstance, view: &str) -> Result<u64, Box<dyn Error>> {
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
            "?[view, from, relation, to, evidence] := \
                 *observation{{view, from, relation, to, evidence}}{filter}\n\
             :order relation, from, to"
        ),
        params,
        ScriptMutability::Immutable,
    )?)
}

fn context(db: &DbInstance, view: &str, entity: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        view,
        &format!(
            "{DIRECT_RULES}\n\
             ?[direction, relation, related, evidence] := \
                 direct[$entity, related, relation, evidence], direction = 'outgoing'\n\
             ?[direction, relation, related, evidence] := \
                 direct[related, $entity, relation, evidence], direction = 'incoming'\n\
             :order direction, relation, related"
        ),
        [("entity", entity.into())],
    )
}

fn trace(db: &DbInstance, view: &str, from: &str, to: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        view,
        &format!(
            "{DIRECT_RULES}\n{DEPENDENCY_RULES}\n\
             start[] <- [[$from]]\n\
             predecessor[to, smallest_by(candidate)] := distance[to, hops], hops > 0, \
                 distance[from, previous_hops], previous_hops + 1 == hops, \
                 direct[from, to, _, _], candidate = [from, from]\n\
             path[to, nodes] := predecessor[to, $from], nodes = [$from, to]\n\
             path[to, nodes] := path[from, previous_nodes], predecessor[to, from], \
                 nodes = append(previous_nodes, to)\n\
             steps[nodes, step] := path[$to, nodes], \
                 index in int_range(length(nodes) - 1), \
                 from = get(nodes, index), to = get(nodes, index + 1), \
                 direct[from, to, relation, evidence], step = [index, relation, evidence]\n\
             ?[nodes, collect(step), hops] := steps[nodes, step], hops = length(nodes) - 1"
        ),
        [
            ("from", from.into()),
            ("to", to.into()),
            ("max_hops", MAX_HOPS.into()),
        ],
    )
}

fn impact(db: &DbInstance, view: &str, entity: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        view,
        &format!(
            "{DIRECT_RULES}\n{IMPACT_RULES}\n\
             start[] <- [[$from]]\n\
             ?[affected] := distance[affected, hops], hops > 0\n:order affected"
        ),
        [("from", entity.into()), ("max_hops", MAX_HOPS.into())],
    )
}

fn dependencies(db: &DbInstance, view: &str, entity: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        view,
        &format!(
            "{DIRECT_RULES}\n{DEPENDENCY_RULES}\n\
             start[] <- [[$from]]\n\
             ?[dependency, hops] := distance[dependency, hops], hops > 0\n\
             :order dependency"
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
    use beholder_domain::LogicalRepository;
    use std::{fs, time::SystemTime};

    #[test]
    fn impact_traverses_dependants() {
        let store = SemanticStore::memory().unwrap();
        let result = store.impact("main", "rpc/Pricing.GetPrice").unwrap();
        assert!(
            result
                .rows
                .iter()
                .flatten()
                .any(|value| { value.as_str() == Some("web/CheckoutPage") })
        );
        assert!(
            !result
                .rows
                .iter()
                .flatten()
                .any(|value| { value.as_str() == Some("pricing/get_price") })
        );
    }

    #[test]
    fn trace_chooses_a_string_predecessor() {
        let db = memory_database().unwrap();
        db.run_script(
            "?[view, from, relation, to, evidence] <- [
                ['diamond', 'start', 'calls', 'left', 'left:1'],
                ['diamond', 'start', 'calls', 'right', 'right:1'],
                ['diamond', 'left', 'calls', 'end', 'left:2'],
                ['diamond', 'right', 'calls', 'end', 'right:2'],
             ]
             :put observation {view, from, relation, to => evidence}",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .unwrap();

        let result = trace(&db, "diamond", "start", "end").unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][2], 2.into());
    }

    #[test]
    fn workspace_smoke() {
        let db = memory_database().unwrap();
        let feature = query(
            &db,
            "feature",
            &format!(
                "{DIRECT_RULES}\n?[provider] := direct['rpc/Pricing.GetPrice', provider, 'implemented_by', _]"
            ),
            [],
        )
        .unwrap();
        let feature = format!("{feature:?}");
        assert!(feature.contains("pricing/get_price_v2"));
        assert!(!feature.contains("pricing/get_price\""));

        let store = SemanticStore { db };
        let result = store.context("main", "rpc/Pricing.GetPrice").unwrap();
        assert_eq!(result.rows.len(), 2);
        assert!(
            result
                .rows
                .iter()
                .flatten()
                .any(|value| { value.as_str() == Some("pricing/get_price") })
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
        let observation = |from: &str, to: &str, evidence: &str| Observation {
            from: from.into(),
            relation: "calls".into(),
            to: to.into(),
            evidence: evidence.into(),
        };

        let first = WorkspaceView::new("main", "analysis", vec![repository_state("one")]).unwrap();
        assert_eq!(
            store
                .publish(
                    &first,
                    &[
                        observation("repo/a", "repo/b", "a.rs:1"),
                        observation("repo/removed", "repo/b", "removed.rs:1"),
                    ],
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
                    &[
                        observation("repo/a", "repo/b", "a.rs:2"),
                        observation("repo/new", "repo/b", "new.rs:1"),
                    ],
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
        assert_eq!(observations.rows.len(), 2);
        assert!(!format!("{observations:?}").contains("repo/removed"));
        assert!(format!("{observations:?}").contains("a.rs:2"));
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
}
