use super::{
    dotnet_di,
    model::{CsharpAnalysis, CsharpRepository},
    resolution::CsharpSource,
};
use beholder_indexing::{
    AnalyzerError, AnalyzerLanguage, LanguageAnalyzer, LanguageAnalyzerBuilder, Plugin,
    PluginActivation, PluginMetadata, RepositoryEnricher, RepositoryEnrichment,
    RepositoryFactsView, RepositorySnapshot,
};
use quick_xml::{Reader, events::Event};

pub(super) struct CsharpLanguage;

impl AnalyzerLanguage for CsharpLanguage {
    type Analysis = CsharpAnalysis;
    type Syntax = tree_sitter::Tree;
    type Repository = CsharpRepository;
}

#[derive(Clone, Copy)]
struct DotnetDiPlugin;

impl Plugin<CsharpLanguage> for DotnetDiPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: "csharp.dotnet-di".into(),
            version: "1".into(),
        }
    }

    fn activate(&self, repository: &RepositorySnapshot) -> Option<PluginActivation> {
        let project_evidence = repository
            .inputs
            .iter()
            .filter(|input| {
                input
                    .path
                    .extension()
                    .is_some_and(|extension| extension == "csproj")
            })
            .filter_map(|input| {
                let source = std::str::from_utf8(&input.content).ok()?;
                dotnet_project_evidence(source).map(|reason| (input, reason))
            })
            .min_by_key(|(input, _)| &input.path)
            .map(|(input, reason)| PluginActivation {
                path: input.path.clone(),
                reason,
            });
        project_evidence.or_else(|| {
            repository
                .inputs
                .iter()
                .filter(|input| {
                    input
                        .path
                        .extension()
                        .is_some_and(|extension| extension == "cs")
                })
                .filter_map(|input| {
                    let source = std::str::from_utf8(&input.content).ok()?;
                    has_dotnet_di_source_evidence(source).then_some(input)
                })
                .min_by_key(|input| &input.path)
                .map(|input| PluginActivation {
                    path: input.path.clone(),
                    reason: "C# source uses .NET dependency-injection APIs".into(),
                })
        })
    }

    fn install(&self, builder: &mut LanguageAnalyzerBuilder<CsharpLanguage>) {
        builder.install_repository_enricher(*self);
    }
}

impl RepositoryEnricher<CsharpLanguage> for DotnetDiPlugin {
    fn enrich(
        &self,
        repository: &CsharpRepository,
        _: RepositoryFactsView<'_>,
    ) -> Result<RepositoryEnrichment, AnalyzerError> {
        let sources = repository
            .sources
            .iter()
            .map(|source| CsharpSource {
                path: &source.path,
                assembly: &source.assembly,
                analysis: &source.analysis,
            })
            .collect::<Vec<_>>();
        Ok(RepositoryEnrichment {
            observations: dotnet_di::observations(
                &repository.repository,
                &repository.projects,
                &sources,
            ),
            ..Default::default()
        })
    }
}

fn dotnet_project_evidence(source: &str) -> Option<String> {
    let mut reader = Reader::from_str(source);
    reader.config_mut().trim_text(true);
    loop {
        let event = reader.read_event().ok()?;
        match event {
            Event::Start(event) | Event::Empty(event) => {
                let name = event.name();
                let Some(value) = event
                    .attributes()
                    .filter_map(Result::ok)
                    .find(|attribute| {
                        matches!(attribute.key.as_ref(), b"Include" | b"Update" | b"Sdk")
                    })
                    .and_then(|attribute| attribute.unescape_value().ok())
                else {
                    continue;
                };
                if name.as_ref() == b"Project"
                    && value
                        .split(';')
                        .any(|sdk| sdk.trim() == "Microsoft.NET.Sdk.Web")
                {
                    return Some(".csproj uses Microsoft.NET.Sdk.Web".into());
                }
                if name.as_ref() == b"PackageReference"
                    && (value.starts_with("Microsoft.Extensions.DependencyInjection")
                        || value == "Microsoft.AspNetCore.App")
                {
                    return Some(format!(".csproj references {value}"));
                }
                if name.as_ref() == b"FrameworkReference" && value == "Microsoft.AspNetCore.App" {
                    return Some(".csproj references Microsoft.AspNetCore.App".into());
                }
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

fn has_dotnet_di_source_evidence(source: &str) -> bool {
    const COLLECTION_REGISTRATIONS: [&str; 6] = [
        "AddSingleton",
        "AddScoped",
        "AddTransient",
        "TryAddSingleton",
        "TryAddScoped",
        "TryAddTransient",
    ];
    let collection_registration = COLLECTION_REGISTRATIONS
        .iter()
        .any(|registration| source.contains(registration))
        && (source.contains("IServiceCollection") || source.contains(".Services."));
    let descriptor_registration = source.contains("ServiceDescriptor.")
        && ["Singleton", "Scoped", "Transient"]
            .iter()
            .any(|registration| source.contains(registration));
    collection_registration || descriptor_registration
}

pub(super) fn built_in_plugins() -> Result<LanguageAnalyzer<CsharpLanguage>, AnalyzerError> {
    LanguageAnalyzerBuilder::new()
        .add_plugin(DotnetDiPlugin)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze, model::CsharpRepositorySource, parse_project};
    use beholder_domain::{
        DependencyRelation, LogicalRepository, RepositoryState, SemanticRelation,
    };
    use beholder_indexing::{ActivePlugins, InputKind, RepositoryInput};
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
    fn activates_from_project_and_source_evidence() {
        let plugins = built_in_plugins().unwrap();
        let active = plugins.activate(
            &snapshot(&[(
                "src/App.csproj",
                r#"<Project><ItemGroup><PackageReference Include="Microsoft.Extensions.DependencyInjection" Version="9.0.0" /></ItemGroup></Project>"#,
            )]),
            true,
        );
        let plugin = active.plugins().next().unwrap();
        assert_eq!(plugin.metadata.id, "csharp.dotnet-di");
        assert_eq!(plugins.source_identity(&active), "");
        assert_eq!(plugin.activation.path, PathBuf::from("src/App.csproj"));
        assert_eq!(
            plugin.activation.reason,
            ".csproj references Microsoft.Extensions.DependencyInjection"
        );

        let active = plugins.activate(
            &snapshot(&[(
                "src/Setup.cs",
                "void Configure(IServiceCollection services) { services.AddScoped<IClock, Clock>(); }",
            )]),
            true,
        );
        assert_eq!(
            active.plugins().next().unwrap().activation.reason,
            "C# source uses .NET dependency-injection APIs"
        );
    }

    #[test]
    fn ignores_unrelated_csharp_and_absent_languages() {
        let plugins = built_in_plugins().unwrap();
        assert!(
            plugins
                .activate(
                    &snapshot(&[("src/Game.csproj", "<Project Sdk=\"Godot.NET.Sdk\" />")]),
                    true,
                )
                .plugins()
                .next()
                .is_none()
        );
        assert!(
            plugins
                .activate(
                    &snapshot(&[(
                        "src/App.csproj",
                        "<Project Sdk=\"Microsoft.NET.Sdk.Web\" />",
                    )]),
                    false,
                )
                .plugins()
                .next()
                .is_none()
        );
    }

    #[test]
    fn contributes_di_facts_only_when_active() {
        let source = "interface IClock {} sealed class Clock : IClock {} static class Setup { static void Configure(IServiceCollection services) { services.AddSingleton<IClock, Clock>(); } }";
        let repository = CsharpRepository {
            repository: "example/repo".into(),
            projects: vec![
                parse_project(PathBuf::from("App/App.csproj").as_path(), "<Project />").unwrap(),
            ],
            sources: vec![CsharpRepositorySource {
                path: PathBuf::from("App/Setup.cs"),
                assembly: "App".into(),
                analysis: analyze(source).unwrap(),
            }],
        };
        let plugins = built_in_plugins().unwrap();
        let inactive = plugins
            .enrich(
                &repository,
                RepositoryFactsView {
                    entities: &[],
                    observations: &[],
                },
                &ActivePlugins::default(),
            )
            .unwrap();
        assert!(inactive.observations.is_empty());

        let active = plugins.activate_direct(PathBuf::from("App/App.csproj").as_path());
        let enrichment = plugins
            .enrich(
                &repository,
                RepositoryFactsView {
                    entities: &[],
                    observations: &[],
                },
                &active,
            )
            .unwrap();
        assert_eq!(enrichment.observations.len(), 2);
        assert!(enrichment.observations.iter().any(|observation| {
            observation.relation == SemanticRelation::Dependency(DependencyRelation::ResolvedBy)
        }));
        assert!(enrichment.observations.iter().any(|observation| {
            observation.relation == SemanticRelation::Dependency(DependencyRelation::Selects)
        }));
    }
}
