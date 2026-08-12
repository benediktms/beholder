use cozo::{DataValue, DbInstance, NamedRows, ScriptMutability};
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

path[from, to, nodes, relations, evidence] :=
    direct[from, to, relation, proof],
    nodes = [from, to],
    relations = [relation],
    evidence = [proof]

path[from, to, nodes, relations, evidence] :=
    path[from, via, previous_nodes, previous_relations, previous_evidence],
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
            "{RULES}\n?[nodes, relations, evidence, hops] := \
             path[$from, $to, nodes, relations, evidence], hops = length(relations)\n\
             :order hops\n:limit 1"
        ),
        [("from", from.into()), ("to", to.into())],
    )
}

fn impact(db: &DbInstance, entity: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        &format!("{RULES}\n?[affected] := path[$entity, affected, _, _, _]\n:order affected"),
        [("entity", entity.into())],
    )
}

fn dependencies(db: &DbInstance, entity: &str) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        &format!(
            "{RULES}\n?[dependency, min(hops)] := \
             path[$entity, dependency, _, relations, _], hops = length(relations)\n\
             :order dependency"
        ),
        [("entity", entity.into())],
    )
}

fn benchmark(db: &DbInstance, entities: i64, fanout: i64) -> Result<String, Box<dyn Error>> {
    let started = Instant::now();
    db.run_script(
        "?[id] := id in int_range($entities) :create benchmark_entity {id: Int}",
        BTreeMap::from([("entities".into(), entities.into())]),
        ScriptMutability::Mutable,
    )?;
    db.run_script(
        "?[from, to] := from in int_range($entities), offset in int_range(1, $fanout + 1), \
         to = (from + offset) % $entities \
         :create benchmark_edge {from: Int, to: Int}",
        BTreeMap::from([
            ("entities".into(), entities.into()),
            ("fanout".into(), fanout.into()),
        ]),
        ScriptMutability::Mutable,
    )?;
    let loaded_in = started.elapsed();

    let started = Instant::now();
    let reachable = db.run_script(
        "reachable[to] := *benchmark_edge{from: 0, to}\n\
         reachable[to] := reachable[from], *benchmark_edge{from, to}\n\
         ?[count_unique(to)] := reachable[to]",
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )?;

    Ok(format!(
        "{entities} entities, {} relationships; loaded in {loaded_in:?}; recursive query in {:?}; {reachable:?}",
        entities * fanout,
        started.elapsed()
    ))
}

fn main() -> Result<(), Box<dyn Error>> {
    let db = database()?;
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args == ["benchmark"] {
        println!("{}", benchmark(&db, 100_000, 10)?);
        return Ok(());
    }
    let result =
        match args.as_slice() {
            [command, entity] if command == "context" => context(&db, entity)?,
            [command, from, to] if command == "trace" || command == "why" => trace(&db, from, to)?,
            [command, entity] if command == "impact" => impact(&db, entity)?,
            [command, entity] if command == "dependencies" => dependencies(&db, entity)?,
            [] => trace(&db, "web/CheckoutPage", "cache/update_price")?,
            _ => return Err(
                "usage: beholder benchmark | <context|impact|dependencies> <entity> | <trace|why> <from> <to>"
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

        let benchmark = benchmark(&db, 100, 10).unwrap();
        assert!(benchmark.contains("100 entities, 1000 relationships"));
        assert!(benchmark.contains("100]]"));
    }
}
