use beholder_domain::{AnalysisDiagnostic, Observation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct SourceAnalysisKey {
    pub(super) content_hash: [u8; 32],
    pub(super) frontend_version: &'static str,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct RepositoryAnalysisKey {
    pub(super) fingerprint: String,
    pub(super) frontend_version: &'static str,
    pub(super) resolver_version: &'static str,
    pub(super) elixir_frontend_version: &'static str,
    pub(super) elixir_resolver_version: &'static str,
    pub(super) protobuf_frontend_version: &'static str,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct RepositoryAnalysis {
    pub(super) observations: Vec<Observation>,
    pub(super) diagnostics: Vec<AnalysisDiagnostic>,
}

impl SourceAnalysisKey {
    pub(super) fn new(source: &str, frontend_version: &'static str) -> Self {
        Self {
            content_hash: Sha256::digest(source.as_bytes()).into(),
            frontend_version,
        }
    }
}
