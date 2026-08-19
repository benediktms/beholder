#[cfg(test)]
use beholder_adapters_treesitter_csharp::CsharpProject;
#[cfg(test)]
use beholder_adapters_treesitter_typescript::TypescriptRepository;
use beholder_domain::{AnalysisDiagnostic, EntityFact, GrpcBindingCandidate, Observation};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use sha2::{Digest, Sha256};

#[cfg(test)]
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
    pub(super) csharp: Option<(&'static str, &'static str)>,
    pub(super) typescript: Option<(&'static str, &'static str)>,
    pub(super) graphql: Option<&'static str>,
    pub(super) protobuf: Option<&'static str>,
}

#[cfg(test)]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct RepositoryAnalysis {
    #[serde(default)]
    pub(super) incomplete: bool,
    #[serde(default)]
    pub(super) csharp_projects: Vec<CsharpProject>,
    pub(super) entities: Vec<EntityFact>,
    #[serde(default)]
    pub(super) grpc_bindings: Vec<GrpcBindingCandidate>,
    pub(super) observations: Vec<Observation>,
    pub(super) diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub(super) typescript: Option<TypescriptRepository>,
}

#[cfg(test)]
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
        if let Some((frontend, resolver)) = self.csharp {
            languages.push(format!("csharp:{frontend}:{resolver}"));
        }
        if let Some((frontend, resolver)) = self.typescript {
            languages.push(format!("typescript:{frontend}:{resolver}"));
        }
        if let Some(frontend) = self.protobuf {
            languages.push(format!("protobuf:{frontend}"));
        }
        if let Some(frontend) = self.graphql {
            languages.push(format!("graphql:{frontend}"));
        }
        if languages.is_empty() {
            "none".into()
        } else {
            languages.join(":")
        }
    }
}
