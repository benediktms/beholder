use super::{
    SourceLanguage, TypescriptRepository, analysis::recover_syntax, graphql,
    model::TypescriptAnalysis, nestjs, nestjs_di, ts_proto,
};
use beholder_indexing::{
    AnalyzerError, AnalyzerLanguage, LanguageAnalyzer, LanguageAnalyzerBuilder, Plugin,
    PluginActivation, PluginMetadata, RepositoryEnricher, RepositoryEnrichment,
    RepositoryFactsView, RepositorySnapshot, SourceRecognitionInput, SourceRecognizer,
};

pub(super) struct TypescriptLanguage;

impl AnalyzerLanguage for TypescriptLanguage {
    type Analysis = TypescriptAnalysis;
    type Syntax = tree_sitter::Tree;
    type Repository = TypescriptRepository;
}

#[derive(Clone, Copy)]
pub(super) struct TsProtoPlugin;

impl Plugin<TypescriptLanguage> for TsProtoPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: "typescript.ts-proto".into(),
            version: "2".into(),
        }
    }

    fn activate(&self, repository: &RepositorySnapshot) -> Option<PluginActivation> {
        repository
            .inputs
            .iter()
            .filter(|input| SourceLanguage::from_path(&input.path).is_some())
            .filter_map(|input| {
                let source = std::str::from_utf8(&input.content).ok()?;
                ts_proto::is_generated_source(&input.path, source).then_some(input)
            })
            .min_by_key(|input| &input.path)
            .map(|input| PluginActivation {
                path: input.path.clone(),
                reason: "generated TypeScript source".into(),
            })
    }

    fn install(&self, builder: &mut LanguageAnalyzerBuilder<TypescriptLanguage>) {
        builder.install_source_recognizer(*self);
        builder.install_repository_enricher(*self);
    }
}

impl SourceRecognizer<TypescriptLanguage> for TsProtoPlugin {
    fn recognize(
        &self,
        input: SourceRecognitionInput<'_, TypescriptLanguage>,
        analysis: &mut TypescriptAnalysis,
    ) -> Result<(), AnalyzerError> {
        analysis.generated = ts_proto::is_generated_source(input.path, input.text);
        Ok(())
    }
}

impl RepositoryEnricher<TypescriptLanguage> for TsProtoPlugin {
    fn enrich(
        &self,
        repository: &TypescriptRepository,
        base: RepositoryFactsView<'_>,
    ) -> Result<RepositoryEnrichment, AnalyzerError> {
        let sources = repository
            .sources
            .iter()
            .map(|(path, analysis)| (path.as_path(), analysis))
            .collect::<Vec<_>>();
        let generated = ts_proto::grpc_methods(&repository.repository, &sources);
        Ok(RepositoryEnrichment {
            grpc_bindings: ts_proto::client_bindings(&generated, base.observations),
            observations: sources
                .iter()
                .flat_map(|(path, analysis)| {
                    ts_proto::message_observations(&repository.repository, analysis, path)
                })
                .collect(),
            ..Default::default()
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct NestjsPlugin;

impl Plugin<TypescriptLanguage> for NestjsPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: "typescript.nestjs".into(),
            version: "2".into(),
        }
    }

    fn activate(&self, repository: &RepositorySnapshot) -> Option<PluginActivation> {
        repository
            .inputs
            .iter()
            .filter(|input| {
                input.path.file_name().and_then(|name| name.to_str()) == Some("package.json")
            })
            .filter_map(|input| {
                let source = std::str::from_utf8(&input.content).ok()?;
                graphql::has_package(&[(&input.path, source)], "@nestjs/common").then_some(input)
            })
            .min_by_key(|input| &input.path)
            .map(|input| PluginActivation {
                path: input.path.clone(),
                reason: "package.json declares @nestjs/common".into(),
            })
    }

    fn install(&self, builder: &mut LanguageAnalyzerBuilder<TypescriptLanguage>) {
        builder.install_source_recognizer(*self);
        builder.install_repository_enricher(*self);
    }
}

impl SourceRecognizer<TypescriptLanguage> for NestjsPlugin {
    fn recognize(
        &self,
        input: SourceRecognitionInput<'_, TypescriptLanguage>,
        analysis: &mut TypescriptAnalysis,
    ) -> Result<(), AnalyzerError> {
        let recovery = recover_syntax(input.syntax.root_node(), input.text.as_bytes())?;
        for root in recovery.roots {
            let (modules, providers) = nestjs_di::extract(root, input.text.as_bytes());
            analysis.nest_modules.extend(modules);
            analysis.nest_providers.extend(providers);
        }
        Ok(())
    }
}

impl RepositoryEnricher<TypescriptLanguage> for NestjsPlugin {
    fn enrich(
        &self,
        repository: &TypescriptRepository,
        base: RepositoryFactsView<'_>,
    ) -> Result<RepositoryEnrichment, AnalyzerError> {
        let sources = repository
            .sources
            .iter()
            .map(|(path, analysis)| (path.as_path(), analysis))
            .collect::<Vec<_>>();
        let generated = ts_proto::grpc_methods(&repository.repository, &sources);
        let (grpc_bindings, diagnostics) = nestjs::bindings(
            &repository.repository,
            &sources,
            &generated,
            base.observations,
        );
        Ok(RepositoryEnrichment {
            grpc_bindings,
            diagnostics,
            ..Default::default()
        })
    }
}

pub(super) fn built_in_plugins() -> Result<LanguageAnalyzer<TypescriptLanguage>, AnalyzerError> {
    LanguageAnalyzerBuilder::new()
        .add_plugin(TsProtoPlugin)
        .add_plugin(NestjsPlugin)
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
    fn repository_plugin_activation_uses_nested_manifest_and_generated_source_evidence() {
        let repository = snapshot(&[
            ("src/main.ts", "export const main = true"),
            (
                "packages/api/package.json",
                r#"{"dependencies":{"@nestjs/common":"11.0.0"}}"#,
            ),
            (
                "packages/contracts/client.generated.ts",
                "// generated by ts-proto\nexport const client = true",
            ),
        ]);

        let plugins = built_in_plugins().unwrap();
        let active = plugins.activate(&repository, true);
        assert_eq!(
            active.identity(),
            "17:typescript.nestjs1:219:typescript.ts-proto1:2"
        );
        assert_eq!(
            plugins.source_identity(&active),
            "17:typescript.nestjs1:219:typescript.ts-proto1:2"
        );
        let evidence = active
            .plugins()
            .map(|plugin| {
                (
                    plugin.metadata.id.as_str(),
                    plugin.activation.path.as_path(),
                    plugin.activation.reason.as_str(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            evidence,
            [
                (
                    "typescript.nestjs",
                    std::path::Path::new("packages/api/package.json"),
                    "package.json declares @nestjs/common",
                ),
                (
                    "typescript.ts-proto",
                    std::path::Path::new("packages/contracts/client.generated.ts"),
                    "generated TypeScript source",
                ),
            ]
        );
    }
}
