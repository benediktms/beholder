use cozo::{DataValue, DbInstance, NamedRows, ScriptMutability, ScriptRunOptions};
use std::{collections::BTreeMap, env, error::Error, time::Instant};

const MAX_HOPS: i64 = 32;

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

const RULES: &str = r#"
direct[from, to, relation, evidence] :=
    *observation{view: 'main', from, relation, to, evidence},
    $view == 'main'

direct[from, to, relation, evidence] :=
    *observation{view: 'main', from, relation, to, evidence},
    $view != 'main',
    not *observation{view: $view, from, relation}

direct[from, to, relation, evidence] :=
    *observation{view: $view, from, relation, to, evidence},
    $view != 'main'
"#;

const DISTANCE_RULES: &str = r#"
distance[node, min(hops)] := start[node], hops = 0
distance[to, min(hops)] :=
    distance[from, previous_hops],
    previous_hops < $max_hops,
    direct[from, to, _, _],
    hops = previous_hops + 1
"#;

fn database() -> Result<DbInstance, Box<dyn Error>> {
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

fn query(
    db: &DbInstance,
    script: &str,
    additions: impl IntoIterator<Item = (&'static str, DataValue)>,
) -> Result<NamedRows, Box<dyn Error>> {
    let mut params = BTreeMap::from([("view".into(), "main".into())]);
    params.extend(
        additions
            .into_iter()
            .map(|(name, value)| (name.into(), value)),
    );
    Ok(db.run_script(script, params, ScriptMutability::Immutable)?)
}

fn context(db: &DbInstance, entity: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        &format!(
            "{RULES}\n\
             ?[direction, relation, related, evidence] := \
                 direct[$entity, related, relation, evidence], direction = 'outgoing'\n\
             ?[direction, relation, related, evidence] := \
                 direct[related, $entity, relation, evidence], direction = 'incoming'\n\
             :order direction, relation, related"
        ),
        [("entity", entity.into())],
    )
}

fn trace(db: &DbInstance, from: &str, to: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        &format!(
            "{RULES}\n{DISTANCE_RULES}\n\
             start[] <- [[$from]]\n\
             predecessor[to, min(from)] := distance[to, hops], hops > 0, \
                 distance[from, previous_hops], previous_hops + 1 == hops, \
                 direct[from, to, _, _]\n\
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

fn impact(db: &DbInstance, entity: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        &format!(
            "{RULES}\n{DISTANCE_RULES}\nstart[] <- [[$from]]\n\
             ?[affected] := distance[affected, hops], hops > 0\n:order affected"
        ),
        [("from", entity.into()), ("max_hops", MAX_HOPS.into())],
    )
}

fn dependencies(db: &DbInstance, entity: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        &format!(
            "{RULES}\n{DISTANCE_RULES}\n\
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

fn main() -> Result<(), Box<dyn Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if let [
        command,
        storage,
        topology,
        entities,
        fanout,
        depth,
        rest @ ..,
    ] = args.as_slice()
        && command == "benchmark"
    {
        let db = benchmark_database(storage, rest.first().map(String::as_str))?;
        println!(
            "{}",
            benchmark(
                &db,
                topology,
                entities.parse()?,
                fanout.parse()?,
                depth.parse()?
            )?
        );
        return Ok(());
    }
    if let [command, storage, topology, entities, depth, path] = args.as_slice()
        && command == "benchmark-query"
    {
        let db = benchmark_database(storage, Some(path))?;
        println!(
            "{}",
            benchmark_queries(&db, topology, entities.parse()?, depth.parse()?)
        );
        return Ok(());
    }
    let db = database()?;
    let result =
        match args.as_slice() {
            [command, entity] if command == "context" => context(&db, entity)?,
            [command, from, to] if command == "trace" || command == "why" => trace(&db, from, to)?,
            [command, entity] if command == "impact" => impact(&db, entity)?,
            [command, entity] if command == "dependencies" => dependencies(&db, entity)?,
            [] => trace(&db, "web/CheckoutPage", "cache/update_price")?,
            _ => return Err(
                "usage: beholder benchmark <mem|sqlite> <linear|tree|dag|corpus> <entities> <fanout> <depth> [database-path] | benchmark-query sqlite <topology> <entities> <depth> <database-path> | <context|impact|dependencies> <entity> | <trace|why> <from> <to>"
                    .into(),
            ),
        };
    println!("{result:#?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_phase_zero_queries_and_applies_view_overrides() {
        let db = database().unwrap();

        let trace = trace(&db, "web/CheckoutPage", "cache/update_price").unwrap();
        assert_eq!(trace.rows.len(), 1);
        assert!(format!("{trace:?}").contains("CheckoutPage.tsx:12"));

        let impact = impact(&db, "rpc/Pricing.GetPrice").unwrap();
        assert_eq!(impact.rows.len(), 3);
        assert!(format!("{impact:?}").contains("cache/update_price"));

        let context = context(&db, "rpc/Pricing.GetPrice").unwrap();
        assert_eq!(context.rows.len(), 2);

        let dependencies = dependencies(&db, "rpc/Pricing.GetPrice").unwrap();
        assert_eq!(dependencies.rows.len(), 3);

        let feature = query(
            &db,
            &format!("{RULES}\n?[provider] := direct['rpc/Pricing.GetPrice', provider, 'implemented_by', _]"),
            [("view", "feature".into())],
        )
        .unwrap();
        let feature = format!("{feature:?}");
        assert!(feature.contains("pricing/get_price_v2"));
        assert!(!feature.contains("pricing/get_price\""));

        let linear_benchmark = benchmark(&db, "linear", 100, 1, 10).unwrap();
        assert!(linear_benchmark.contains("topology=linear"));
        assert!(!linear_benchmark.contains("query_timeout"));

        let dag = DbInstance::new("mem", "", Default::default()).unwrap();
        let dag_benchmark = benchmark(&dag, "dag", 1_000, 10, 6).unwrap();
        assert!(dag_benchmark.contains("algorithm=bounded-distance"));
        assert!(!dag_benchmark.contains("query_timeout"));
    }
}
