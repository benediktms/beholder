use cozo::{DataValue, DbInstance, NamedRows, ScriptMutability, ScriptRunOptions};
use std::{collections::BTreeMap, env, error::Error, time::Instant};

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

const PATH_RULES: &str = r#"
path[to, nodes, relations, evidence] :=
    direct[$from, to, relation, proof],
    nodes = [$from, to],
    relations = [relation],
    evidence = [proof]

path[to, nodes, relations, evidence] :=
    path[via, previous_nodes, previous_relations, previous_evidence],
    direct[via, to, relation, proof],
    not is_in(to, previous_nodes),
    nodes = append(previous_nodes, to),
    relations = append(previous_relations, relation),
    evidence = append(previous_evidence, proof)
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
            "{RULES}\n{PATH_RULES}\n?[nodes, relations, evidence, hops] := \
             path[$to, nodes, relations, evidence], hops = length(relations)\n\
             :order hops\n:limit 1"
        ),
        [("from", from.into()), ("to", to.into())],
    )
}

fn impact(db: &DbInstance, entity: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        &format!("{RULES}\n{PATH_RULES}\n?[affected] := path[affected, _, _, _]\n:order affected"),
        [("from", entity.into())],
    )
}

fn dependencies(db: &DbInstance, entity: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        &format!(
            "{RULES}\n{PATH_RULES}\n?[dependency, min(hops)] := \
             path[dependency, _, relations, _], hops = length(relations)\n\
             :order dependency"
        ),
        [("from", entity.into())],
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
        "mode={}, topology={topology}, entities={entities}, fanout={fanout}, depth={depth}, load={loaded_in:?}, {}",
        if cfg!(feature = "parallel") {
            "parallel"
        } else {
            "single"
        },
        benchmark_queries(db, topology, entities, depth)
    ))
}

fn benchmark_queries(db: &DbInstance, _topology: &str, entities: i64, depth: i64) -> String {
    let params = BTreeMap::from([
        ("depth".into(), depth.into()),
        ("target".into(), depth.min(entities - 1).into()),
    ]);
    let rules = "path[to, nodes, evidence, hops] := \
                     *benchmark_edge{from: 0, to, evidence: proof}, \
                     nodes = [0, to], evidence = [proof], hops = 1\n\
                 path[to, nodes, evidence, hops] := \
                     path[via, previous_nodes, previous_evidence, previous_hops], \
                     previous_hops < $depth, *benchmark_edge{from: via, to, evidence: proof}, \
                     not is_in(to, previous_nodes), nodes = append(previous_nodes, to), \
                     evidence = append(previous_evidence, proof), hops = previous_hops + 1";

    let closure = timed_query(
        db,
        "reachable[to, hops] := *benchmark_edge{from: 0, to}, hops = 1\n\
         reachable[to, hops] := reachable[from, previous_hops], previous_hops < $depth, \
             *benchmark_edge{from, to}, hops = previous_hops + 1\n\
         ?[count_unique(to)] := reachable[to, _]",
        params.clone(),
    );
    let context = timed_query(
        db,
        "?[count(to)] := *benchmark_edge{from: 0, to}",
        BTreeMap::new(),
    );
    let trace = timed_query(
        db,
        &format!(
            "{rules}\n?[nodes, evidence, hops] := path[$target, nodes, evidence, hops]\n\
             :order hops\n:limit 1"
        ),
        params.clone(),
    );
    let impact = timed_query(
        db,
        &format!("{rules}\n?[count_unique(to)] := path[to, _, _, _]"),
        params,
    );

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

        let benchmark = benchmark(&db, "linear", 100, 1, 10).unwrap();
        assert!(benchmark.contains("topology=linear"));
        assert!(!benchmark.contains("query_timeout"));
    }
}
