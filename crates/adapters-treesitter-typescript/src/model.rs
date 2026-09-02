use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

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
            Self::JavaScript => "21-javascript",
            Self::Jsx => "22-jsx",
            Self::TypeScript => "22-typescript",
            Self::Tsx => "22-tsx",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TypescriptAnalysis {
    pub(super) language: SourceLanguage,
    pub(super) calls: Vec<Call>,
    pub(super) definitions: Vec<Definition>,
    pub(super) imports: Vec<Import>,
    pub(super) exports: Vec<Export>,
    pub(super) string_constants: Vec<StringConstant>,
    pub(super) graphql_documents: Vec<GraphqlDocument>,
    pub(super) nest_modules: Vec<NestModule>,
    pub(super) nest_providers: Vec<NestProvider>,
    #[serde(default)]
    pub(super) generated: bool,
    pub(super) parse_error_lines: Vec<usize>,
}

impl TypescriptAnalysis {
    pub(super) fn semantic_shape(&self) -> Self {
        let mut analysis = self.clone();
        for call in &mut analysis.calls {
            call.clear_position();
            call.scope_start = 0;
            call.scope_end = 0;
        }
        for definition in &mut analysis.definitions {
            definition.line = 0;
            let mut scopes = definition
                .calls
                .iter()
                .map(|call| (call.scope_start, call.scope_end))
                .chain(
                    definition
                        .bindings
                        .iter()
                        .map(|binding| (binding.scope_start, binding.scope_end)),
                )
                .chain(
                    definition
                        .alias_bindings
                        .iter()
                        .map(|binding| (binding.scope_start, binding.scope_end)),
                )
                .chain(
                    definition
                        .alias_bindings
                        .iter()
                        .map(|binding| (binding.source_scope_start, binding.source_scope_end)),
                )
                .chain(
                    definition
                        .factory_bindings
                        .iter()
                        .map(|binding| (binding.scope_start, binding.scope_end)),
                )
                .collect::<Vec<_>>();
            scopes.sort_unstable();
            scopes.dedup();
            let normalize_scope = |scope: (usize, usize)| {
                let index = scopes.binary_search(&scope).unwrap_or_default();
                let parent = scopes
                    .iter()
                    .enumerate()
                    .filter(|(_, candidate)| {
                        **candidate != scope && candidate.0 <= scope.0 && scope.1 <= candidate.1
                    })
                    .min_by_key(|(_, candidate)| candidate.1 - candidate.0)
                    .map_or(0, |(parent, _)| parent + 1);
                (index, parent)
            };
            let alias_positions = definition
                .alias_bindings
                .iter()
                .map(|binding| (binding.line, binding.character))
                .collect::<Vec<_>>();
            for call in &mut definition.calls {
                let alias_order = alias_positions
                    .iter()
                    .filter(|position| **position <= (call.line, call.start_character))
                    .count();
                (call.scope_start, call.scope_end) =
                    normalize_scope((call.scope_start, call.scope_end));
                call.clear_position();
                call.line = alias_order;
            }
            for binding in &mut definition.bindings {
                (binding.scope_start, binding.scope_end) =
                    normalize_scope((binding.scope_start, binding.scope_end));
            }
            for binding in &mut definition.alias_bindings {
                binding.line = 0;
                binding.character = 0;
                (binding.scope_start, binding.scope_end) =
                    normalize_scope((binding.scope_start, binding.scope_end));
                (binding.source_scope_start, binding.source_scope_end) =
                    normalize_scope((binding.source_scope_start, binding.source_scope_end));
            }
            for binding in &mut definition.factory_bindings {
                (binding.scope_start, binding.scope_end) =
                    normalize_scope((binding.scope_start, binding.scope_end));
            }
        }
        for document in &mut analysis.graphql_documents {
            document.line = 0;
        }
        analysis.parse_error_lines = (!analysis.parse_error_lines.is_empty())
            .then_some(0)
            .into_iter()
            .collect();
        analysis
    }
}

impl Call {
    fn clear_position(&mut self) {
        self.line = 0;
        self.start_line = 0;
        self.start_character = 0;
        self.end_line = 0;
        self.end_character = 0;
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct GraphqlDocument {
    pub(super) binding: String,
    pub(super) source: String,
    pub(super) line: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TypescriptRepository {
    pub(super) repository: String,
    pub(super) sources: Vec<(PathBuf, Arc<TypescriptAnalysis>)>,
    pub(super) manifests: Vec<(PathBuf, String)>,
    pub(super) configs: Vec<(PathBuf, String)>,
}

impl TypescriptRepository {
    pub fn new<A>(
        repository: impl Into<String>,
        sources: Vec<(PathBuf, A)>,
        manifests: Vec<(PathBuf, String)>,
        configs: Vec<(PathBuf, String)>,
    ) -> Self
    where
        A: Into<Arc<TypescriptAnalysis>>,
    {
        Self {
            repository: repository.into(),
            sources: sources
                .into_iter()
                .map(|(path, analysis)| (path, analysis.into()))
                .collect(),
            manifests,
            configs,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
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
    pub(super) alias_bindings: Vec<AliasBinding>,
    pub(super) factory_bindings: Vec<FactoryBinding>,
    pub(super) factory: Option<String>,
    #[serde(default)]
    pub(super) factory_callback: Option<String>,
    #[serde(default)]
    pub(super) callback_return_type: Option<String>,
    pub(super) base: Option<String>,
    pub(super) return_type: Option<String>,
    pub(super) exported: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Binding {
    pub(super) receiver: String,
    pub(super) type_name: String,
    pub(super) injection_token: Option<String>,
    #[serde(default)]
    pub(super) scope_start: usize,
    #[serde(default)]
    pub(super) scope_end: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct NestModule {
    pub(super) name: String,
    pub(super) imports: Vec<String>,
    pub(super) providers: Vec<String>,
    pub(super) members: Vec<String>,
    pub(super) exports: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct NestProvider {
    pub(super) name: String,
    pub(super) token: String,
    pub(super) implementation: String,
    pub(super) existing: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct AliasBinding {
    pub(super) receiver: String,
    pub(super) source: String,
    #[serde(default)]
    pub(super) line: usize,
    #[serde(default)]
    pub(super) character: u32,
    #[serde(default)]
    pub(super) scope_start: usize,
    #[serde(default)]
    pub(super) scope_end: usize,
    #[serde(default)]
    pub(super) source_scope_start: usize,
    #[serde(default)]
    pub(super) source_scope_end: usize,
    #[serde(default)]
    pub(super) conditional: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct FactoryBinding {
    pub(super) receiver: String,
    pub(super) factory: String,
    #[serde(default)]
    pub(super) scope_start: usize,
    #[serde(default)]
    pub(super) scope_end: usize,
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
    #[serde(default)]
    pub(super) returned_context: Option<String>,
    #[serde(default)]
    pub(super) returned_receiver: Option<String>,
    pub(super) name: String,
    pub(super) arguments: Vec<String>,
    pub(super) type_arguments: Vec<String>,
    pub(super) line: usize,
    #[serde(default)]
    pub(super) start_line: u32,
    #[serde(default)]
    pub(super) start_character: u32,
    #[serde(default)]
    pub(super) end_line: u32,
    #[serde(default)]
    pub(super) end_character: u32,
    #[serde(default)]
    pub(super) scope_start: usize,
    #[serde(default)]
    pub(super) scope_end: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct StringConstant {
    pub(super) name: String,
    pub(super) value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_retains_shared_source_analysis() {
        let analysis = Arc::new(
            crate::analyze("export function run() {}", SourceLanguage::TypeScript).unwrap(),
        );
        let repository = TypescriptRepository::new(
            "example",
            vec![(PathBuf::from("src/index.ts"), analysis.clone())],
            vec![],
            vec![],
        );

        assert!(Arc::ptr_eq(&analysis, &repository.sources[0].1));
    }
}
