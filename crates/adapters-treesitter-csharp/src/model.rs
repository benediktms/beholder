use beholder_domain::{
    CallableClauseRole, Evidence, EvidenceContext, EvidencePayload, SourceExcerpt, SourceRange,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::project::CsharpProject;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CsharpAnalysis {
    pub(super) definitions: Vec<Definition>,
    pub(super) parse_error_lines: Vec<usize>,
}

pub(super) struct CsharpRepository {
    pub(super) repository: String,
    pub(super) projects: Vec<CsharpProject>,
    pub(super) sources: Vec<CsharpRepositorySource>,
}

pub(super) struct CsharpRepositorySource {
    pub(super) path: PathBuf,
    pub(super) assembly: String,
    pub(super) analysis: CsharpAnalysis,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum DefinitionKind {
    Namespace,
    Type,
    Callable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Definition {
    pub(super) qualified_name: String,
    pub(super) kind: DefinitionKind,
    pub(super) return_type: Option<String>,
    pub(super) base_types: Vec<String>,
    pub(super) is_static: bool,
    pub(super) line: usize,
    #[serde(default)]
    pub(super) signature: Option<SourceExcerpt>,
    #[serde(default)]
    pub(super) definition_range: Option<SourceRange>,
    pub(super) parameters: Vec<Parameter>,
    pub(super) locals: Vec<Binding>,
    pub(super) calls: Vec<Call>,
}

impl Definition {
    pub(super) fn callable_context(&self, role: CallableClauseRole) -> Option<EvidenceContext> {
        Some(EvidenceContext::CallableClause {
            role,
            signature: self.signature.clone()?,
            guard: None,
            definition_range: self.definition_range.clone()?,
        })
    }

    pub(super) fn evidence(&self, path: &std::path::Path) -> Evidence {
        Evidence::structured(EvidencePayload {
            path: Some(path.display().to_string()),
            line: u32::try_from(self.line).ok(),
            detail: None,
            range: self.definition_range.clone(),
            contexts: self
                .callable_context(CallableClauseRole::Declaration)
                .into_iter()
                .collect(),
        })
        .expect("tree-sitter emits valid C# definition evidence ranges")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Binding {
    pub(super) name: String,
    pub(super) type_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Parameter {
    pub(super) name: String,
    pub(super) type_name: String,
    pub(super) is_extension: bool,
    pub(super) is_optional: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Argument {
    pub(super) name: Option<String>,
    pub(super) expression: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum CallKind {
    Direct,
    Member,
    Constructor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Call {
    pub(super) expression: String,
    pub(super) kind: CallKind,
    pub(super) receiver: Option<String>,
    pub(super) name: String,
    pub(super) type_arguments: Vec<String>,
    pub(super) arguments: Vec<Argument>,
    pub(super) line: usize,
    #[serde(default)]
    pub(super) range: Option<SourceRange>,
    #[serde(default)]
    pub(super) contexts: Vec<EvidenceContext>,
}

impl Call {
    pub(super) fn evidence(
        &self,
        path: &std::path::Path,
        selected_target: Option<EvidenceContext>,
    ) -> Evidence {
        let mut contexts = self.contexts.clone();
        contexts.extend(selected_target);
        Evidence::structured(EvidencePayload {
            path: Some(path.display().to_string()),
            line: u32::try_from(self.line).ok(),
            detail: None,
            range: self.range.clone(),
            contexts,
        })
        .expect("tree-sitter emits valid C# call evidence ranges")
    }
}
