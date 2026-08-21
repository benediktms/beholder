use super::{
    analysis::{arguments, call_target, text},
    grpc,
    model::{ElixirAnalysis, ElixirRepository},
};
use beholder_indexing::{
    AnalyzerError, AnalyzerLanguage, LanguageAnalyzer, LanguageAnalyzerBuilder, Plugin,
    PluginActivation, PluginMetadata, RepositoryEnricher, RepositoryEnrichment,
    RepositoryFactsView, RepositorySnapshot, SourceRecognitionInput, SourceRecognizer,
};
use std::path::Path;
use tree_sitter::{Node, Parser};

pub(super) struct ElixirLanguage;

impl AnalyzerLanguage for ElixirLanguage {
    type Analysis = ElixirAnalysis;
    type Syntax = tree_sitter::Tree;
    type Repository = ElixirRepository;
}

#[derive(Clone, Copy)]
struct GrpcElixirPlugin;

impl Plugin<ElixirLanguage> for GrpcElixirPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: "elixir.grpc-elixir".into(),
            version: "1".into(),
        }
    }

    fn activate(&self, repository: &RepositorySnapshot) -> Option<PluginActivation> {
        repository
            .inputs
            .iter()
            .filter_map(|input| {
                let source = std::str::from_utf8(&input.content).ok()?;
                let reason = if is_mix_manifest(&input.path) && manifest_declares_grpc(source) {
                    "mix.exs declares :grpc dependency"
                } else if is_elixir_source(&input.path) && grpc_source_evidence(source) {
                    "Elixir source uses grpc-elixir"
                } else {
                    return None;
                };
                Some((input, reason))
            })
            .min_by_key(|(input, _)| &input.path)
            .map(|(input, reason)| PluginActivation {
                path: input.path.clone(),
                reason: reason.into(),
            })
    }

    fn install(&self, builder: &mut LanguageAnalyzerBuilder<ElixirLanguage>) {
        builder.install_source_recognizer(*self);
        builder.install_repository_enricher(*self);
    }
}

impl SourceRecognizer<ElixirLanguage> for GrpcElixirPlugin {
    fn recognize(
        &self,
        input: SourceRecognitionInput<'_, ElixirLanguage>,
        analysis: &mut ElixirAnalysis,
    ) -> Result<(), AnalyzerError> {
        grpc::recognize(input.syntax.root_node(), input.text.as_bytes(), analysis);
        Ok(())
    }
}

impl RepositoryEnricher<ElixirLanguage> for GrpcElixirPlugin {
    fn enrich(
        &self,
        repository: &ElixirRepository,
        _: RepositoryFactsView<'_>,
    ) -> Result<RepositoryEnrichment, AnalyzerError> {
        let sources = repository
            .sources
            .iter()
            .map(|(path, analysis)| (path.as_path(), analysis))
            .collect::<Vec<_>>();
        let (grpc_bindings, diagnostics) = grpc::bindings(&repository.repository, &sources);
        Ok(RepositoryEnrichment {
            grpc_bindings,
            diagnostics,
            ..Default::default()
        })
    }
}

pub(super) fn built_in_plugins() -> Result<LanguageAnalyzer<ElixirLanguage>, AnalyzerError> {
    LanguageAnalyzerBuilder::new()
        .add_plugin(GrpcElixirPlugin)
        .build()
}

fn is_elixir_source(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "ex" | "exs"))
}

fn is_mix_manifest(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("mix.exs")
}

fn syntax(source: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_elixir::LANGUAGE.into())
        .ok()?;
    parser.parse(source, None)
}

fn manifest_declares_grpc(source: &str) -> bool {
    syntax(source).is_some_and(|tree| {
        any_node(tree.root_node(), &mut |node| {
            node.kind() == "tuple"
                && node
                    .named_child(0)
                    .and_then(|dependency| text(dependency, source.as_bytes()))
                    == Some(":grpc")
        })
    })
}

fn grpc_source_evidence(source: &str) -> bool {
    syntax(source).is_some_and(|tree| {
        any_node(tree.root_node(), &mut |node| {
            node.kind() == "call"
                && call_target(node, source.as_bytes()) == Some("use")
                && arguments(node)
                    .and_then(|arguments| arguments.named_child(0))
                    .and_then(|module| text(module, source.as_bytes()))
                    .is_some_and(|module| {
                        matches!(module, "GRPC.Service" | "GRPC.Server" | "GRPC.Stub")
                    })
        })
    })
}

fn any_node<'tree>(node: Node<'tree>, predicate: &mut impl FnMut(Node<'tree>) -> bool) -> bool {
    if predicate(node) {
        return true;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| any_node(child, predicate))
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
    fn activates_from_nested_mix_manifest() {
        let repository = snapshot(&[
            ("apps/api/lib/api.ex", "defmodule Api do end"),
            ("apps/api/mix.exs", "defp deps, do: [{ :grpc, \"~> 1.0\" }]"),
        ]);

        let plugins = built_in_plugins().unwrap();
        let active = plugins.activate(&repository, true);
        assert_eq!(active.identity(), "18:elixir.grpc-elixir1:1");
        assert_eq!(plugins.source_identity(&active), "18:elixir.grpc-elixir1:1");
        let plugin = active.plugins().next().unwrap();
        assert_eq!(plugin.activation.path, Path::new("apps/api/mix.exs"));
        assert_eq!(
            plugin.activation.reason,
            "mix.exs declares :grpc dependency"
        );
    }

    #[test]
    fn activates_from_generated_source_without_manifest() {
        let repository = snapshot(&[(
            "lib/pricing.pb.ex",
            "defmodule Pricing.Service do\n  use GRPC.Service, name: \"pricing\"\nend",
        )]);

        let active = built_in_plugins().unwrap().activate(&repository, true);
        assert_eq!(active.identity(), "18:elixir.grpc-elixir1:1");
        assert_eq!(
            active.plugins().next().unwrap().activation.reason,
            "Elixir source uses grpc-elixir"
        );
    }

    #[test]
    fn does_not_activate_without_elixir_or_grpc_evidence() {
        let plugins = built_in_plugins().unwrap();
        assert!(
            plugins
                .activate(&snapshot(&[("src/main.rs", "fn main() {}")]), false)
                .plugins()
                .next()
                .is_none()
        );
        assert!(
            plugins
                .activate(
                    &snapshot(&[("lib/example.ex", "defmodule Example do end")]),
                    true,
                )
                .plugins()
                .next()
                .is_none()
        );
    }
}
