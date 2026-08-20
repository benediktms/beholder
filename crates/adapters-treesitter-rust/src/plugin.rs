use super::{RustAnalysis, analysis::collect_tree_sitter_functions, model::RustRepository, tonic};
use beholder_indexing::{
    AnalyzerError, AnalyzerLanguage, LanguageAnalyzer, LanguageAnalyzerBuilder, Plugin,
    PluginActivation, PluginMetadata, RepositoryEnricher, RepositoryEnrichment,
    RepositoryFactsView, RepositorySnapshot, SourceRecognitionInput, SourceRecognizer,
};
use std::{collections::BTreeSet, path::Path};
use toml::Value;

pub(super) struct RustLanguage;

impl AnalyzerLanguage for RustLanguage {
    type Analysis = RustAnalysis;
    type Syntax = tree_sitter::Tree;
    type Repository = RustRepository;
}

#[derive(Clone, Copy)]
struct TonicPlugin;

impl Plugin<RustLanguage> for TonicPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: "rust.tonic".into(),
            version: "1".into(),
        }
    }

    fn activate(&self, repository: &RepositorySnapshot) -> Option<PluginActivation> {
        let manifests = repository
            .inputs
            .iter()
            .filter(|input| {
                input
                    .path
                    .file_name()
                    .is_some_and(|name| name == "Cargo.toml")
            })
            .filter_map(|input| {
                let source = std::str::from_utf8(&input.content).ok()?;
                toml::from_str::<Value>(source)
                    .ok()
                    .map(|manifest| (input, manifest))
            })
            .collect::<Vec<_>>();
        manifests
            .iter()
            .filter(|(input, manifest)| {
                let manifest_dir = input.path.parent().unwrap_or_else(|| Path::new(""));
                let workspace_aliases = manifests
                    .iter()
                    .filter(|(candidate, candidate_manifest)| {
                        candidate_manifest.get("workspace").is_some()
                            && candidate.path.parent().is_some_and(|workspace_dir| {
                                manifest_dir.starts_with(workspace_dir)
                            })
                    })
                    .max_by_key(|(candidate, _)| {
                        candidate
                            .path
                            .parent()
                            .map_or(0, |path| path.components().count())
                    })
                    .map(|(_, workspace)| workspace_dependency_aliases(workspace))
                    .unwrap_or_default();
                manifest_uses_tonic(manifest, &workspace_aliases)
            })
            .map(|(input, _)| *input)
            .min_by_key(|input| &input.path)
            .map(|input| PluginActivation {
                path: input.path.clone(),
                reason: "Cargo.toml declares tonic dependency".into(),
            })
    }

    fn install(&self, builder: &mut LanguageAnalyzerBuilder<RustLanguage>) {
        builder.install_source_recognizer(*self);
        builder.install_repository_enricher(*self);
    }
}

fn workspace_dependency_aliases(manifest: &Value) -> BTreeSet<String> {
    manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(Value::as_table)
        .into_iter()
        .flatten()
        .filter(|(name, dependency)| dependency_package(name, dependency) == "tonic")
        .map(|(name, _)| name.clone())
        .collect()
}

fn dependency_package<'a>(name: &'a str, dependency: &'a Value) -> &'a str {
    dependency
        .get("package")
        .and_then(Value::as_str)
        .unwrap_or(name)
}

fn manifest_uses_tonic(manifest: &Value, workspace_aliases: &BTreeSet<String>) -> bool {
    let mut dependency_tables = ["dependencies", "dev-dependencies", "build-dependencies"]
        .into_iter()
        .filter_map(|section| manifest.get(section).and_then(Value::as_table))
        .collect::<Vec<_>>();
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        dependency_tables.extend(
            targets
                .values()
                .filter_map(Value::as_table)
                .flat_map(|target| {
                    ["dependencies", "dev-dependencies", "build-dependencies"]
                        .into_iter()
                        .filter_map(|section| target.get(section).and_then(Value::as_table))
                }),
        );
    }
    dependency_tables.into_iter().any(|dependencies| {
        dependencies.iter().any(|(name, dependency)| {
            dependency_package(name, dependency) == "tonic"
                || (workspace_aliases.contains(name)
                    && dependency
                        .get("workspace")
                        .and_then(Value::as_bool)
                        .unwrap_or(false))
        })
    })
}

impl SourceRecognizer<RustLanguage> for TonicPlugin {
    fn recognize(
        &self,
        input: SourceRecognitionInput<'_, RustLanguage>,
        analysis: &mut RustAnalysis,
    ) -> Result<(), AnalyzerError> {
        if input.syntax.root_node().has_error() || !analysis.parse_error_lines.is_empty() {
            return Ok(());
        }
        let root = input.syntax.root_node();
        let mut functions = Vec::new();
        collect_tree_sitter_functions(root, input.text.as_bytes(), &mut Vec::new(), &mut functions);
        analysis.tonic = tonic::analyze(root, input.text.as_bytes(), &functions);
        Ok(())
    }
}

impl RepositoryEnricher<RustLanguage> for TonicPlugin {
    fn enrich(
        &self,
        repository: &RustRepository,
        _: RepositoryFactsView<'_>,
    ) -> Result<RepositoryEnrichment, AnalyzerError> {
        let sources = repository
            .sources
            .iter()
            .map(|(path, analysis)| (path.as_path(), analysis))
            .collect::<Vec<_>>();
        let (grpc_bindings, diagnostics) = tonic::bindings(&repository.repository, &sources);
        Ok(RepositoryEnrichment {
            grpc_bindings,
            diagnostics,
            ..Default::default()
        })
    }
}

pub(super) fn built_in_plugins() -> Result<LanguageAnalyzer<RustLanguage>, AnalyzerError> {
    LanguageAnalyzerBuilder::new()
        .add_plugin(TonicPlugin)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::{LogicalRepository, RepositoryState};
    use beholder_indexing::{InputKind, RepositoryInput};
    use std::{path::PathBuf, sync::Arc};

    fn snapshot(inputs: &[(&str, &str)]) -> RepositorySnapshot {
        RepositorySnapshot {
            base: PathBuf::from("repo"),
            state: RepositoryState {
                repository: LogicalRepository {
                    identity: "example/repo".into(),
                },
                head: None,
                fingerprint: "state".into(),
            },
            inputs: inputs
                .iter()
                .map(|(path, content)| RepositoryInput {
                    path: PathBuf::from(path),
                    content: Arc::from(content.as_bytes()),
                    kind: InputKind::Source,
                })
                .collect(),
        }
    }

    #[test]
    fn activates_from_nested_cargo_workspace_dependency() {
        let plugins = built_in_plugins().unwrap();
        let active = plugins.activate(
            &snapshot(&[
                (
                    "Cargo.toml",
                    "[workspace.dependencies]\ngrpc = { package = \"tonic\", version = \"0.14\" }",
                ),
                (
                    "crates/api/Cargo.toml",
                    "[dependencies]\ngrpc.workspace = true",
                ),
                (
                    "crates/api/src/lib.rs",
                    "grpc::include_proto!(\"example.v1\");",
                ),
            ]),
            true,
        );

        let plugin = active.plugins().next().unwrap();
        assert_eq!(plugin.metadata.id, "rust.tonic");
        assert_eq!(
            plugin.activation.path,
            PathBuf::from("crates/api/Cargo.toml")
        );
        assert_eq!(
            plugin.activation.reason,
            "Cargo.toml declares tonic dependency"
        );
        assert!(
            plugins
                .activate(
                    &snapshot(&[
                        (
                            "Cargo.toml",
                            "[workspace.dependencies]\ngrpc = { package = \"tonic\", version = \"0.14\" }",
                        ),
                        (
                            "tools/Cargo.toml",
                            "[workspace]\n[workspace.dependencies]\ngrpc = \"1\"",
                        ),
                        (
                            "tools/app/Cargo.toml",
                            "[dependencies]\ngrpc.workspace = true",
                        ),
                        ("tools/app/src/lib.rs", "fn main() {}"),
                    ]),
                    true,
                )
                .plugins()
                .next()
                .is_none()
        );
        assert!(
            plugins
                .activate(&snapshot(&[("README.md", "tonic::include_proto!")]), false)
                .plugins()
                .next()
                .is_none()
        );
    }
}
