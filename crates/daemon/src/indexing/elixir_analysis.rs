use super::cache::SourceAnalysisKey;
use super::{CacheStatus, Cached, IndexScheduler};
use beholder_adapters_treesitter_elixir::{ElixirAnalysis, analyze};
use std::{fs, sync::Arc};

pub(super) fn analysis_versioned(
    scheduler: &IndexScheduler,
    source: &str,
    frontend_version: &'static str,
) -> Cached<ElixirAnalysis> {
    let key = SourceAnalysisKey::new(source, frontend_version);
    if let Some(analysis) = scheduler
        .elixir_cache
        .lock()
        .map_err(|_| "Elixir frontend cache lock poisoned")?
        .get(&key)
        .cloned()
    {
        return Ok((analysis, CacheStatus::Memory));
    }
    let path = scheduler.cache_path("elixir", &key);
    if let Ok(bytes) = fs::read(&path)
        && let Ok(analysis) = serde_json::from_slice::<ElixirAnalysis>(&bytes)
    {
        let analysis = Arc::new(analysis);
        scheduler
            .elixir_cache
            .lock()
            .map_err(|_| "Elixir frontend cache lock poisoned")?
            .insert(key, analysis.clone());
        return Ok((analysis, CacheStatus::Disk));
    }
    let analysis = Arc::new(analyze(source)?);
    if let Some(parent) = path.parent()
        && fs::create_dir_all(parent).is_ok()
        && let Ok(bytes) = serde_json::to_vec(analysis.as_ref())
    {
        let _ = fs::write(path, bytes);
    }
    let analysis = scheduler
        .elixir_cache
        .lock()
        .map_err(|_| "Elixir frontend cache lock poisoned")?
        .entry(key)
        .or_insert_with(|| analysis.clone())
        .clone();
    Ok((analysis, CacheStatus::Miss))
}
