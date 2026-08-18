use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceLanguage {
    JavaScript,
    Jsx,
    TypeScript,
    Tsx,
}

impl SourceLanguage {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "js" => Some(Self::JavaScript),
            "jsx" => Some(Self::Jsx),
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            _ => None,
        }
    }

    pub fn id_segment(self) -> &'static str {
        match self {
            Self::JavaScript | Self::Jsx => "javascript",
            Self::TypeScript | Self::Tsx => "typescript",
        }
    }

    pub fn cache_version(self) -> &'static str {
        match self {
            Self::JavaScript => "6-javascript",
            Self::Jsx => "6-jsx",
            Self::TypeScript => "6-typescript",
            Self::Tsx => "6-tsx",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TypescriptAnalysis {
    pub(super) language: SourceLanguage,
    pub(super) definitions: Vec<Definition>,
    pub(super) imports: Vec<Import>,
    pub(super) exports: Vec<Export>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum DefinitionKind {
    Namespace,
    Callable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Definition {
    pub(super) qualified_name: String,
    pub(super) kind: DefinitionKind,
    pub(super) line: usize,
    pub(super) calls: Vec<Call>,
    pub(super) bindings: Vec<Binding>,
    pub(super) factory: Option<String>,
    pub(super) exported: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Binding {
    pub(super) receiver: String,
    pub(super) type_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Import {
    pub(super) source: String,
    pub(super) bindings: Vec<ImportBinding>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ImportBinding {
    pub(super) imported: String,
    pub(super) local: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Export {
    pub(super) source: Option<String>,
    pub(super) local: String,
    pub(super) exported: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum CallKind {
    Direct,
    Member,
    Constructor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Call {
    pub(super) kind: CallKind,
    pub(super) receiver: Option<String>,
    pub(super) name: String,
    pub(super) line: usize,
}
