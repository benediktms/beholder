use mnestic_engine::{DataValue, DbInstance, ScriptMutability, ScriptRunOptions};
use std::{collections::BTreeMap, error::Error, time::Instant};

pub(super) fn benchmark(
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

pub(super) fn benchmark_queries(
    db: &DbInstance,
    _topology: &str,
    entities: i64,
    depth: i64,
) -> String {
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

pub(super) fn timed_query(
    db: &DbInstance,
    script: &str,
    params: BTreeMap<String, DataValue>,
) -> String {
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
