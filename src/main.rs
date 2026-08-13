use cozo::{
    DataValue, DbInstance, MultiTransaction, NamedRows, ScriptMutability, ScriptRunOptions,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, env, error::Error, fs, path::Path, time::Instant};
use tree_sitter::{Node, Parser};

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
    Ok(db)
}

fn collect_functions<'tree>(
    node: Node<'tree>,
    source: &[u8],
    functions: &mut Vec<(String, Node<'tree>)>,
) {
    if node.kind() == "function_item"
        && let Some(name) = node.child_by_field_name("name")
        && let Ok(name) = name.utf8_text(source)
    {
        functions.push((name.to_owned(), node));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_functions(child, source, functions);
    }
}

fn collect_calls(node: Node<'_>, source: &[u8], calls: &mut Vec<(String, usize)>) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && let Ok(function) = function.utf8_text(source)
        && let Some(name) = function.rsplit([':', '.']).find(|part| !part.is_empty())
    {
        calls.push((name.to_owned(), node.start_position().row + 1));
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_calls(child, source, calls);
    }
}

fn repository_identity(root: &Path) -> Result<String, Box<dyn Error>> {
    // ponytail: directory name is the local-only fallback; canonical Git remotes come with registration.
    root.canonicalize()?
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("cannot derive repository identity from {}", root.display()).into())
}

fn rust_observations(
    repository: &str,
    source: &str,
    path: &Path,
) -> Result<Vec<[String; 5]>, Box<dyn Error>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .ok_or("Rust parser returned no tree")?;
    if tree.root_node().has_error() {
        return Err(format!("failed to parse Rust source: {}", path.display()).into());
    }

    let source_bytes = source.as_bytes();
    let mut functions = Vec::new();
    collect_functions(tree.root_node(), source_bytes, &mut functions);
    let module = path
        .strip_prefix("src")
        .unwrap_or(path)
        .with_extension("")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let source_id = format!("repo://{repository}/rust/{module}");
    // ponytail: simple names cover this fixture; qualify impl methods when Rust support expands.
    let definitions = functions
        .iter()
        .map(|(name, _)| (name.clone(), format!("{source_id}/{name}")))
        .collect::<BTreeMap<_, _>>();
    let mut observations = Vec::new();

    for (name, function) in functions {
        let function_id = definitions[&name].clone();
        observations.push([
            "main".into(),
            source_id.clone(),
            "defines".into(),
            function_id.clone(),
            format!("{}:{}", path.display(), function.start_position().row + 1),
        ]);
        let mut calls = Vec::new();
        collect_calls(function, source_bytes, &mut calls);
        for (callee, line) in calls {
            observations.push([
                "main".into(),
                function_id.clone(),
                "calls".into(),
                definitions
                    .get(&callee)
                    .cloned()
                    .unwrap_or_else(|| format!("rust-call://{callee}")),
                format!("{}:{line}", path.display()),
            ]);
        }
    }
    Ok(observations)
}

fn store_observations(
    transaction: &MultiTransaction,
    observations: &[[String; 5]],
) -> Result<(), Box<dyn Error>> {
    // ponytail: one transaction, per-row writes; batch when ingestion throughput matters.
    for [view, from, relation, to, evidence] in observations {
        transaction.run_script(
            "?[view, from, relation, to, evidence] <- [[$view, $from, $relation, $to, $evidence]]\n\
             :put observation {view, from, relation, to => evidence}",
            BTreeMap::from([
                ("view".into(), view.clone().into()),
                ("from".into(), from.clone().into()),
                ("relation".into(), relation.clone().into()),
                ("to".into(), to.clone().into()),
                ("evidence".into(), evidence.clone().into()),
            ]),
        )?;
    }
    Ok(())
}

fn source_fingerprint(repository: &str, sources: &[(std::path::PathBuf, String)]) -> String {
    let mut digest = Sha256::new();
    digest.update((repository.len() as u64).to_le_bytes());
    digest.update(repository.as_bytes());
    for (path, source) in sources {
        let path = path.to_string_lossy();
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update((source.len() as u64).to_le_bytes());
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn fingerprint_matches(db: &DbInstance, fingerprint: &str) -> Result<bool, Box<dyn Error>> {
    let rows = db.run_script(
        "?[matches] := *analysis_fingerprint{view: 'main', fingerprint: stored}, \
             matches = stored == $fingerprint",
        BTreeMap::from([("fingerprint".into(), fingerprint.into())]),
        ScriptMutability::Immutable,
    )?;
    Ok(rows.rows.first().is_some_and(|row| row[0] == true.into()))
}

fn publish_observations(
    db: &DbInstance,
    observations: &[[String; 5]],
    fingerprint: &str,
) -> Result<(), Box<dyn Error>> {
    let transaction = db.multi_transaction(true);
    transaction.run_script(
        "?[view, from, relation, to] := *observation{view, from, relation, to}\n\
         :rm observation {view, from, relation, to}",
        BTreeMap::new(),
    )?;
    store_observations(&transaction, observations)?;
    transaction.run_script(
        "?[view, revision] := \
             *analysis_revision{view: 'main', revision: previous}, \
             view = 'main', revision = previous + 1\n\
         :put analysis_revision {view => revision}",
        BTreeMap::new(),
    )?;
    transaction.run_script(
        "?[view, fingerprint] <- [['main', $fingerprint]] \
         :put analysis_fingerprint {view => fingerprint}",
        BTreeMap::from([("fingerprint".into(), fingerprint.into())]),
    )?;
    transaction.commit()?;
    Ok(())
}

fn index_rust(path: &Path, database_path: &Path) -> Result<(usize, bool), Box<dyn Error>> {
    let sources = vec![(path.to_path_buf(), fs::read_to_string(path)?)];
    let repository = repository_identity(path.parent().unwrap_or_else(|| Path::new(".")))?;
    let fingerprint = source_fingerprint(&repository, &sources);
    let db = persistent_database(database_path, true)?;
    if fingerprint_matches(&db, &fingerprint)? {
        return Ok((0, false));
    }
    let observations = rust_observations(&repository, &sources[0].1, path)?;
    publish_observations(&db, &observations, &fingerprint)?;
    Ok((observations.len(), true))
}

fn rust_source_files(
    directory: &Path,
    files: &mut Vec<std::path::PathBuf>,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            rust_source_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn index_rust_repository(
    root: &Path,
    database_path: &Path,
) -> Result<(usize, bool), Box<dyn Error>> {
    let source_root = root.join("src");
    if !source_root.is_dir() {
        return Err(format!(
            "Rust source directory does not exist: {}",
            source_root.display()
        )
        .into());
    }
    let mut files = Vec::new();
    rust_source_files(&source_root, &mut files)?;
    files.sort();

    let sources = files
        .into_iter()
        .map(|path| {
            let relative_path = path.strip_prefix(root)?.to_path_buf();
            Ok((relative_path, fs::read_to_string(path)?))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let repository = repository_identity(root)?;
    let fingerprint = source_fingerprint(&repository, &sources);
    let db = persistent_database(database_path, true)?;
    if fingerprint_matches(&db, &fingerprint)? {
        return Ok((0, false));
    }

    let mut all_observations = Vec::new();
    for (path, source) in &sources {
        all_observations.extend(rust_observations(&repository, source, path)?);
    }
    publish_observations(&db, &all_observations, &fingerprint)?;
    Ok((all_observations.len(), true))
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

fn inspect_relations(db: &DbInstance) -> Result<NamedRows, Box<dyn Error>> {
    Ok(db.run_script("::relations", BTreeMap::new(), ScriptMutability::Immutable)?)
}

fn inspect_revisions(db: &DbInstance) -> Result<NamedRows, Box<dyn Error>> {
    Ok(db.run_script(
        "?[view, revision, fingerprint] := \
             *analysis_revision{view, revision}, \
             *analysis_fingerprint{view, fingerprint}\n\
         :order view",
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )?)
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
    if let [command, source, path] = args.as_slice()
        && command == "index-rust"
    {
        let (count, published) = index_rust(Path::new(source), Path::new(path))?;
        println!(
            "{}",
            if published {
                format!("indexed {count} Rust observations")
            } else {
                "unchanged; kept current analysis revision".into()
            }
        );
        return Ok(());
    }
    if let [command, root, path] = args.as_slice()
        && command == "index-rust-repo"
    {
        let (count, published) = index_rust_repository(Path::new(root), Path::new(path))?;
        println!(
            "{}",
            if published {
                format!("indexed {count} Rust observations")
            } else {
                "unchanged; kept current analysis revision".into()
            }
        );
        return Ok(());
    }
    if let [command, subject, path] = args.as_slice()
        && command == "inspect"
    {
        let db = persistent_database(Path::new(path), false)?;
        let result = match subject.as_str() {
            "relations" => inspect_relations(&db)?,
            "revisions" => inspect_revisions(&db)?,
            "observations" => inspect_observations(&db, None)?,
            _ => {
                return Err("inspect subject must be relations, revisions, or observations".into());
            }
        };
        println!("{result:#?}");
        return Ok(());
    }
    if let [command, subject, relation, path] = args.as_slice()
        && command == "inspect"
        && subject == "observations"
    {
        let db = persistent_database(Path::new(path), false)?;
        println!("{:#?}", inspect_observations(&db, Some(relation))?);
        return Ok(());
    }
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
    if let [command, entity, path] = args.as_slice()
        && (command == "context" || command == "impact" || command == "dependencies")
    {
        let db = persistent_database(Path::new(path), false)?;
        let result = match command.as_str() {
            "context" => context(&db, entity)?,
            "impact" => impact(&db, entity)?,
            "dependencies" => dependencies(&db, entity)?,
            _ => unreachable!(),
        };
        println!("{result:#?}");
        return Ok(());
    }
    if let [command, from, to, path] = args.as_slice()
        && (command == "trace" || command == "why")
    {
        let db = persistent_database(Path::new(path), false)?;
        println!("{:#?}", trace(&db, from, to)?);
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
                "usage: beholder <index-rust|index-rust-repo> <source> <database-path> | inspect <relations|revisions|observations> [relation] <database-path> | benchmark <mem|sqlite> <linear|tree|dag|corpus> <entities> <fanout> <depth> [database-path] | benchmark-query sqlite <topology> <entities> <depth> <database-path> | <context|impact|dependencies> <entity> [database-path] | <trace|why> <from> <to> [database-path]"
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

        let seed_context = context(&db, "rpc/Pricing.GetPrice").unwrap();
        assert_eq!(seed_context.rows.len(), 2);

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

        let observations = rust_observations(
            "beholder",
            include_str!("main.rs"),
            Path::new("src/main.rs"),
        )
        .expect("Beholder should parse its own Rust source");
        assert!(observations.iter().any(|row| {
            row[1] == "repo://beholder/rust/main/trace"
                && row[2] == "calls"
                && row[3] == "repo://beholder/rust/main/query"
        }));

        let path = env::temp_dir().join(format!("beholder-dogfood-{}.db", std::process::id()));
        let _ = fs::remove_file(&path);
        let (count, published) = index_rust_repository(Path::new("."), &path).unwrap();
        assert!(published && count > 0);
        assert_eq!(
            index_rust_repository(Path::new("."), &path).unwrap(),
            (0, false)
        );
        let indexed = persistent_database(&path, false).unwrap();
        let context = context(&indexed, "repo://beholder/rust/main/trace").unwrap();
        assert!(format!("{context:?}").contains("repo://beholder/rust/main/query"));
        assert!(format!("{:?}", inspect_relations(&indexed).unwrap()).contains("observation"));
        let calls = inspect_observations(&indexed, Some("calls")).unwrap();
        assert!(!calls.rows.is_empty());
        assert!(calls.rows.iter().all(|row| row[2] == "calls".into()));
        drop(indexed);

        let source = env::temp_dir().join(format!("beholder-dogfood-{}.rs", std::process::id()));
        fs::write(&source, "fn first() { second(); } fn second() {}").unwrap();
        assert!(index_rust(&source, &path).unwrap().1);
        let indexed = persistent_database(&path, false).unwrap();
        assert!(
            !inspect_observations(&indexed, Some("calls"))
                .unwrap()
                .rows
                .is_empty()
        );
        drop(indexed);

        fs::write(&source, "fn first() {}").unwrap();
        assert!(index_rust(&source, &path).unwrap().1);
        assert_eq!(index_rust(&source, &path).unwrap(), (0, false));
        let indexed = persistent_database(&path, false).unwrap();
        assert!(
            inspect_observations(&indexed, Some("calls"))
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(
            inspect_revisions(&indexed).unwrap().rows[0][1],
            DataValue::from(3_i64)
        );
        drop(indexed);
        fs::remove_file(source).unwrap();
        fs::remove_file(path).unwrap();
    }
}
