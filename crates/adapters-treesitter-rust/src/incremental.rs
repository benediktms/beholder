use crate::{
    FRONTEND_VERSION, RustAnalysis,
    analysis::{analyze_with_plugins, source_entity_id},
    plugin::built_in_plugins,
};
use beholder_indexing::{ActivePlugins, AnalyzerError};
use rayon::prelude::*;
use salsa::Setter;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FileId {
    repository: String,
    path: PathBuf,
}

#[salsa::input]
struct SourceFile {
    #[returns(ref)]
    repository: String,
    #[returns(ref)]
    path: PathBuf,
    #[returns(ref)]
    text: String,
    #[returns(ref)]
    active_plugins: ActivePlugins,
    #[returns(ref)]
    cached_analysis: Option<RustAnalysis>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AnalysisFailure {
    UnsafeRecovery,
    Failed(String),
}

impl AnalysisFailure {
    fn from_error(error: AnalyzerError) -> Self {
        if error
            .downcast_ref::<beholder_domain::UnsafeTreeRecovery>()
            .is_some()
        {
            Self::UnsafeRecovery
        } else {
            Self::Failed(error.to_string())
        }
    }

    fn into_error(self) -> AnalyzerError {
        match self {
            Self::UnsafeRecovery => beholder_domain::UnsafeTreeRecovery::new(
                "Rust",
                "incremental parse recovery was unsafe",
            )
            .into(),
            Self::Failed(detail) => detail.into(),
        }
    }
}

#[salsa::tracked(returns(clone))]
fn parse_file(
    db: &dyn salsa::Database,
    source: SourceFile,
) -> Result<RustAnalysis, AnalysisFailure> {
    if let Some(cached) = source.cached_analysis(db) {
        return Ok(cached.clone());
    }
    let plugins = built_in_plugins().map_err(AnalysisFailure::from_error)?;
    analyze_with_plugins(
        source.text(db),
        source.path(db),
        &plugins,
        source.active_plugins(db),
    )
    .map_err(AnalysisFailure::from_error)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SymbolSummary {
    pub(super) id: String,
    pub(super) interface_hash: [u8; 32],
    pub(super) body_hash: [u8; 32],
    pub(super) calls: Vec<(String, bool)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FileSummary {
    pub(super) source: String,
    pub(super) symbols: Vec<SymbolSummary>,
    pub(super) incomplete: bool,
}

#[salsa::tracked(returns(clone))]
fn file_summary(
    db: &dyn salsa::Database,
    source: SourceFile,
) -> Result<FileSummary, AnalysisFailure> {
    let analysis = parse_file(db, source)?;
    let source_id = source_entity_id(source.repository(db), source.path(db));
    Ok(FileSummary {
        source: source_id.clone(),
        symbols: analysis
            .functions()
            .map(|function| SymbolSummary {
                id: format!("{source_id}/{}", function.qualified_name()),
                interface_hash: function.interface_hash(),
                body_hash: function.body_hash(),
                calls: function
                    .calls()
                    .map(|call| (call.name().to_owned(), call.receiver_method()))
                    .collect(),
            })
            .collect(),
        incomplete: !analysis.parse_error_lines.is_empty(),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FactShard {
    pub(super) owner: String,
    pub(super) interface_hash: [u8; 32],
    pub(super) body_hash: [u8; 32],
    pub(super) calls: Vec<(String, bool)>,
}

#[salsa::tracked(returns(clone))]
fn fact_shards(
    db: &dyn salsa::Database,
    source: SourceFile,
) -> Result<Vec<FactShard>, AnalysisFailure> {
    Ok(file_summary(db, source)?
        .symbols
        .into_iter()
        .map(|symbol| FactShard {
            owner: symbol.id,
            interface_hash: symbol.interface_hash,
            body_hash: symbol.body_hash,
            calls: symbol.calls,
        })
        .collect())
}

#[derive(Clone, Copy)]
pub(super) enum CacheStatus {
    Memory,
    Disk,
    Miss,
}

pub(super) struct IncrementalAnalysis {
    pub(super) analysis: Arc<RustAnalysis>,
    pub(super) status: CacheStatus,
}

pub(super) struct IncrementalRust {
    db: salsa::DatabaseImpl,
    files: BTreeMap<FileId, SourceFile>,
    cache_dir: PathBuf,
}

struct PreparedSource {
    db: salsa::DatabaseImpl,
    path: PathBuf,
    source: SourceFile,
    status: CacheStatus,
    cache_path: PathBuf,
}

impl IncrementalRust {
    pub(super) fn new(cache_dir: PathBuf) -> Self {
        Self {
            db: salsa::DatabaseImpl::new(),
            files: BTreeMap::new(),
            cache_dir,
        }
    }

    pub(super) fn analyze_many(
        &mut self,
        repository: &str,
        sources: &[(&Path, &str)],
        active_plugins: &ActivePlugins,
        source_plugins: &str,
    ) -> Vec<(PathBuf, Result<IncrementalAnalysis, AnalyzerError>)> {
        let prepared = sources
            .iter()
            .map(|(path, text)| {
                let id = FileId {
                    repository: repository.to_owned(),
                    path: (*path).to_owned(),
                };
                let cache_path = self.cache_path(path, text, source_plugins);
                let (source, status) = if let Some(source) = self.files.get(&id).copied() {
                    let changed = source.text(&self.db) != *text
                        || source.active_plugins(&self.db) != active_plugins;
                    if changed {
                        source.set_text(&mut self.db).to((*text).to_owned());
                        source
                            .set_active_plugins(&mut self.db)
                            .to(active_plugins.clone());
                        source.set_cached_analysis(&mut self.db).to(None);
                        (source, CacheStatus::Miss)
                    } else {
                        (source, CacheStatus::Memory)
                    }
                } else {
                    let cached = fs::read(&cache_path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                    let status = if cached.is_some() {
                        CacheStatus::Disk
                    } else {
                        CacheStatus::Miss
                    };
                    let source = SourceFile::new(
                        &self.db,
                        repository.to_owned(),
                        (*path).to_owned(),
                        (*text).to_owned(),
                        active_plugins.clone(),
                        cached,
                    );
                    self.files.insert(id, source);
                    (source, status)
                };
                ((*path).to_owned(), source, status, cache_path)
            })
            .collect::<Vec<_>>();
        let prepared = prepared
            .into_iter()
            .map(|(path, source, status, cache_path)| PreparedSource {
                db: self.db.clone(),
                path,
                source,
                status,
                cache_path,
            })
            .collect::<Vec<_>>();
        prepared
            .into_par_iter()
            .map(|prepared| {
                let result = (|| {
                    let analysis = Arc::new(
                        parse_file(&prepared.db, prepared.source)
                            .map_err(AnalysisFailure::into_error)?,
                    );
                    let _ = fact_shards(&prepared.db, prepared.source)
                        .map_err(AnalysisFailure::into_error)?;
                    if matches!(prepared.status, CacheStatus::Miss)
                        && let Some(parent) = prepared.cache_path.parent()
                        && fs::create_dir_all(parent).is_ok()
                        && let Ok(bytes) = serde_json::to_vec(analysis.as_ref())
                    {
                        let _ = fs::write(&prepared.cache_path, bytes);
                    }
                    Ok(IncrementalAnalysis {
                        analysis,
                        status: prepared.status,
                    })
                })();
                (prepared.path, result)
            })
            .collect()
    }

    fn cache_path(&self, path: &Path, source: &str, source_plugins: &str) -> PathBuf {
        let mut digest = Sha256::new();
        for part in [
            FRONTEND_VERSION.as_bytes(),
            path.as_os_str().as_encoded_bytes(),
            source.as_bytes(),
            source_plugins.as_bytes(),
        ] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part);
        }
        self.cache_dir
            .join(format!("{}.json", hex(digest.finalize().into())))
    }
}

fn hex(key: [u8; 32]) -> String {
    key.into_iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[salsa::db]
    #[derive(Clone)]
    struct TestDb {
        storage: salsa::Storage<Self>,
        logs: Arc<Mutex<Vec<String>>>,
    }

    impl TestDb {
        fn new() -> Self {
            let logs = Arc::<Mutex<Vec<String>>>::default();
            Self {
                storage: salsa::Storage::new(Some(Box::new({
                    let logs = Arc::clone(&logs);
                    move |event| {
                        if let salsa::EventKind::WillExecute { .. } = event.kind {
                            logs.lock().unwrap().push(format!("{event:?}"));
                        }
                    }
                }))),
                logs,
            }
        }

        fn take_logs(&self) -> Vec<String> {
            std::mem::take(&mut *self.logs.lock().unwrap())
        }
    }

    #[salsa::db]
    impl salsa::Database for TestDb {}

    #[test]
    fn semantic_changes_propagate_only_while_outputs_change() {
        let mut db = TestDb::new();
        let active = ActivePlugins::default();
        let source = SourceFile::new(
            &db,
            "beholder".into(),
            "src/lib.rs".into(),
            "pub fn run(value: u32) -> u32 { helper(value) }\nfn helper(value: u32) -> u32 { value }".into(),
            active,
            None,
        );
        let initial = fact_shards(&db, source).unwrap();
        db.take_logs();

        source.set_text(&mut db).to(
            "// formatting only\n\npub fn run(value: u32) -> u32 { helper(value) }\nfn helper(value: u32) -> u32 { value }".into(),
        );
        assert_eq!(fact_shards(&db, source).unwrap(), initial);
        let comment_logs = db.take_logs().join("\n");
        assert!(comment_logs.contains("parse_file"));
        assert!(comment_logs.contains("file_summary"));
        assert!(!comment_logs.contains("fact_shards"));

        source.set_text(&mut db).to(
            "pub fn run(value: u32) -> u32 { helper(value) + 1 }\nfn helper(value: u32) -> u32 { value }".into(),
        );
        let body_changed = fact_shards(&db, source).unwrap();
        assert_eq!(body_changed[0].interface_hash, initial[0].interface_hash);
        assert_ne!(body_changed[0].body_hash, initial[0].body_hash);
        assert_eq!(body_changed[1], initial[1]);

        source.set_text(&mut db).to(
            "pub fn run(value: u64) -> u32 { helper(value as u32) + 1 }\nfn helper(value: u32) -> u32 { value }".into(),
        );
        let interface_changed = fact_shards(&db, source).unwrap();
        assert_ne!(
            interface_changed[0].interface_hash,
            body_changed[0].interface_hash
        );
        assert_eq!(interface_changed[1], body_changed[1]);
    }

    #[test]
    fn updates_after_an_unchanged_sibling() {
        let cache = std::env::temp_dir().join(format!(
            "beholder-rust-incremental-test-{}",
            std::process::id()
        ));
        let mut incremental = IncrementalRust::new(cache.clone());
        let active = ActivePlugins::default();
        let first = Path::new("src/first.rs");
        let second = Path::new("src/second.rs");
        incremental.analyze_many(
            "beholder",
            &[(first, "fn first() {}"), (second, "fn before() {}")],
            &active,
            "",
        );

        let updated = incremental.analyze_many(
            "beholder",
            &[(first, "fn first() {}"), (second, "fn after() {}")],
            &active,
            "",
        );

        assert!(updated.into_iter().all(|(_, result)| result.is_ok()));
        fs::remove_dir_all(cache).unwrap();
    }
}
