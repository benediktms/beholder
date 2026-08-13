use beholder_adapters_git::{repository_identity, source_fingerprint};
use beholder_adapters_mnestic::SemanticStore;
use beholder_adapters_treesitter_rust::{observations, resolve_repository_calls, source_files};
use std::{env, error::Error, fs, path::Path};

fn index_rust(path: &Path, database_path: &Path) -> Result<(usize, bool), Box<dyn Error>> {
    let sources = vec![(path.to_path_buf(), fs::read_to_string(path)?)];
    let repository = repository_identity(path.parent().unwrap_or_else(|| Path::new(".")))?;
    let fingerprint = source_fingerprint(&repository, &sources);
    let store = SemanticStore::persistent(database_path, true)?;
    if store.fingerprint_matches(&fingerprint)? {
        return Ok((0, false));
    }
    let observations = observations(&repository, &sources[0].1, path)?;
    store.publish(&observations, &fingerprint)?;
    Ok((observations.len(), true))
}

fn index_rust_repository(
    root: &Path,
    database_path: &Path,
) -> Result<(usize, bool), Box<dyn Error>> {
    if !root.is_dir() {
        return Err(format!("repository does not exist: {}", root.display()).into());
    }
    let mut files = Vec::new();
    source_files(root, &mut files)?;
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
    let store = SemanticStore::persistent(database_path, true)?;
    if store.fingerprint_matches(&fingerprint)? {
        return Ok((0, false));
    }

    let mut all_observations = Vec::new();
    for (path, source) in &sources {
        all_observations.extend(observations(&repository, source, path)?);
    }
    resolve_repository_calls(&mut all_observations);
    store.publish(&all_observations, &fingerprint)?;
    Ok((all_observations.len(), true))
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
        let store = SemanticStore::persistent(Path::new(path), false)?;
        let result = match subject.as_str() {
            "relations" => store.inspect_relations()?,
            "revisions" => store.inspect_revisions()?,
            "observations" => store.inspect_observations(None)?,
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
        let store = SemanticStore::persistent(Path::new(path), false)?;
        println!("{:#?}", store.inspect_observations(Some(relation))?);
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
        let store = SemanticStore::benchmark_store(storage, rest.first().map(String::as_str))?;
        println!(
            "{}",
            store.benchmark(topology, entities.parse()?, fanout.parse()?, depth.parse()?)?
        );
        return Ok(());
    }
    if let [command, storage, topology, entities, depth, path] = args.as_slice()
        && command == "benchmark-query"
    {
        let store = SemanticStore::benchmark_store(storage, Some(path))?;
        println!(
            "{}",
            store.benchmark_queries(topology, entities.parse()?, depth.parse()?)
        );
        return Ok(());
    }
    if let [command, entity, path] = args.as_slice()
        && (command == "context" || command == "impact" || command == "dependencies")
    {
        let store = SemanticStore::persistent(Path::new(path), false)?;
        let result = match command.as_str() {
            "context" => store.context(entity)?,
            "impact" => store.impact(entity)?,
            "dependencies" => store.dependencies(entity)?,
            _ => unreachable!(),
        };
        println!("{result:#?}");
        return Ok(());
    }
    if let [command, from, to, path] = args.as_slice()
        && (command == "trace" || command == "why")
    {
        let store = SemanticStore::persistent(Path::new(path), false)?;
        println!("{:#?}", store.trace(from, to)?);
        return Ok(());
    }
    let store = SemanticStore::memory()?;
    let result = match args.as_slice() {
        [command, entity] if command == "context" => store.context(entity)?,
        [command, from, to] if command == "trace" || command == "why" => {
            store.trace(from, to)?
        }
        [command, entity] if command == "impact" => store.impact(entity)?,
        [command, entity] if command == "dependencies" => store.dependencies(entity)?,
        [] => store.trace("web/CheckoutPage", "cache/update_price")?,
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
    fn workspace_smoke() {
        let store = SemanticStore::memory().unwrap();

        let result = store
            .trace("web/CheckoutPage", "cache/update_price")
            .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert!(format!("{result:?}").contains("CheckoutPage.tsx:12"));

        assert_eq!(store.impact("rpc/Pricing.GetPrice").unwrap().rows.len(), 3);
        assert_eq!(store.context("rpc/Pricing.GetPrice").unwrap().rows.len(), 2);
        assert_eq!(
            store
                .dependencies("rpc/Pricing.GetPrice")
                .unwrap()
                .rows
                .len(),
            3
        );

        let path = env::temp_dir().join(format!("beholder-dogfood-{}.db", std::process::id()));
        let _ = fs::remove_file(&path);
        let (count, published) = index_rust_repository(Path::new("."), &path).unwrap();
        assert!(published && count > 0);
        assert_eq!(
            index_rust_repository(Path::new("."), &path).unwrap(),
            (0, false)
        );
        let indexed = SemanticStore::persistent(&path, false).unwrap();
        let entity = "repo://beholder/rust/crates/adapters-mnestic/src/lib/trace";
        assert!(format!("{:?}", indexed.context(entity).unwrap()).contains("query"));
        assert!(format!("{:?}", indexed.inspect_relations().unwrap()).contains("observation"));
        let calls = indexed.inspect_observations(Some("calls")).unwrap();
        assert!(!calls.rows.is_empty());
        assert!(
            calls
                .rows
                .iter()
                .all(|row| row[2].as_str() == Some("calls"))
        );
        drop(indexed);

        let source = env::temp_dir().join(format!("beholder-dogfood-{}.rs", std::process::id()));
        fs::write(&source, "fn first() { second(); } fn second() {}").unwrap();
        assert!(index_rust(&source, &path).unwrap().1);
        fs::write(&source, "fn first() {}").unwrap();
        assert!(index_rust(&source, &path).unwrap().1);
        assert_eq!(index_rust(&source, &path).unwrap(), (0, false));
        let indexed = SemanticStore::persistent(&path, false).unwrap();
        assert!(
            indexed
                .inspect_observations(Some("calls"))
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(
            indexed.inspect_revisions().unwrap().rows[0][1].as_i64(),
            Some(3)
        );
        drop(indexed);
        fs::remove_file(source).unwrap();
        fs::remove_file(path).unwrap();
    }
}
