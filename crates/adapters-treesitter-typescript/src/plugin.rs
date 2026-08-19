use super::{TypescriptRepository, model::TypescriptAnalysis, nestjs, nestjs_di, ts_proto};
use beholder_indexing::{
    AnalyzerError, AnalyzerLanguage, LanguageAnalyzer, LanguageAnalyzerBuilder, Plugin,
    PluginMetadata, RepositoryEnricher, RepositoryEnrichment, RepositoryFactsView,
    SourceRecognitionInput, SourceRecognizer,
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
            version: "1".into(),
        }
    }

    fn install(self, builder: &mut LanguageAnalyzerBuilder<TypescriptLanguage>) {
        builder.install_source_recognizer(self);
        builder.install_repository_enricher(self);
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
            version: "1".into(),
        }
    }

    fn install(self, builder: &mut LanguageAnalyzerBuilder<TypescriptLanguage>) {
        builder.install_source_recognizer(self);
        builder.install_repository_enricher(self);
    }
}

impl SourceRecognizer<TypescriptLanguage> for NestjsPlugin {
    fn recognize(
        &self,
        input: SourceRecognitionInput<'_, TypescriptLanguage>,
        analysis: &mut TypescriptAnalysis,
    ) -> Result<(), AnalyzerError> {
        let root = input.syntax.root_node();
        let roots = if analysis.parse_error_lines.is_empty() {
            vec![root]
        } else {
            let mut cursor = root.walk();
            root.named_children(&mut cursor)
                .filter(|child| !child.has_error())
                .collect()
        };
        for root in roots {
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
