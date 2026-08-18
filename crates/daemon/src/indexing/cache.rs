use beholder_domain::{AnalysisDiagnostic, EntityFact, GrpcBindingCandidate, Observation};
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
    pub(super) rust: Option<(&'static str, &'static str)>,
    pub(super) elixir: Option<(&'static str, &'static str)>,
    pub(super) typescript: Option<(&'static str, &'static str)>,
    pub(super) protobuf: Option<&'static str>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct RepositoryAnalysis {
    pub(super) entities: Vec<EntityFact>,
    #[serde(default)]
    pub(super) grpc_bindings: Vec<GrpcBindingCandidate>,
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

impl RepositoryAnalysisKey {
    pub(super) fn analysis_identity(&self) -> String {
        let mut languages = Vec::new();
        if let Some((frontend, resolver)) = self.rust {
            languages.push(format!("rust:{frontend}:{resolver}"));
        }
        if let Some((frontend, resolver)) = self.elixir {
            languages.push(format!("elixir:{frontend}:{resolver}"));
        }
        if let Some((frontend, resolver)) = self.typescript {
            languages.push(format!("typescript:{frontend}:{resolver}"));
        }
        if let Some(frontend) = self.protobuf {
            languages.push(format!("protobuf:{frontend}"));
        }
        if languages.is_empty() {
            "none".into()
        } else {
            languages.join(":")
        }
    }
}
