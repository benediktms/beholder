use crate::{
    SourceLanguage, TypescriptAnalysis, analysis::analyze_core, model::Call,
    plugin::TypescriptLanguage,
};
use beholder_adapters_treesitter::recover;
use beholder_domain::UnsafeTreeRecovery;
use beholder_indexing::{
    LanguageAnalyzerBuilder, Plugin, PluginActivation, PluginMetadata, RepositorySnapshot,
    SourceRecognitionInput, SourceRecognizer,
};
use tree_sitter::{Node, Parser};

#[derive(Clone, Copy)]
pub(super) struct SveltePlugin;

impl Plugin<TypescriptLanguage> for SveltePlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: "typescript.svelte".into(),
            version: "2".into(),
        }
    }

    fn activate(&self, repository: &RepositorySnapshot) -> Option<PluginActivation> {
        repository
            .inputs
            .iter()
            .find(|input| is_svelte(&input.path))
            .map(|input| PluginActivation {
                path: input.path.clone(),
                reason: "Svelte source".into(),
            })
    }

    fn install(&self, builder: &mut LanguageAnalyzerBuilder<TypescriptLanguage>) {
        builder.install_source_recognizer(*self);
    }
}

impl SourceRecognizer<TypescriptLanguage> for SveltePlugin {
    fn recognize(
        &self,
        input: SourceRecognitionInput<'_, TypescriptLanguage>,
        analysis: &mut TypescriptAnalysis,
    ) -> Result<(), beholder_indexing::AnalyzerError> {
        if !is_svelte(input.path) {
            return Ok(());
        }

        let (source, language, error_lines) =
            extract_scripts(input.syntax.root_node(), input.text)?;
        let mut embedded = analyze_core(&source, language)?;
        embedded.calls.retain(|call| !is_rune(call));
        for definition in &mut embedded.definitions {
            definition.calls.retain(|call| !is_rune(call));
            if definition.factory.as_deref().is_some_and(rune_name) {
                definition.factory = None;
            }
        }
        embedded.parse_error_lines.extend(error_lines);
        embedded.parse_error_lines.sort_unstable();
        embedded.parse_error_lines.dedup();
        embedded.language = SourceLanguage::Svelte;
        *analysis = embedded;
        Ok(())
    }
}

fn is_svelte(path: &std::path::Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("svelte")
}

pub(super) fn embedded_source(source: &str) -> Option<String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_svelte_ng::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source, None)?;
    extract_scripts(tree.root_node(), source)
        .ok()
        .map(|(source, _, _)| source)
}

fn extract_scripts(
    root: Node<'_>,
    source: &str,
) -> Result<(String, SourceLanguage, Vec<usize>), beholder_indexing::AnalyzerError> {
    let recovery = recover(root).map_err(|_| {
        UnsafeTreeRecovery::new("Svelte", "missing syntax may change script boundaries")
    })?;
    let mut masked = source
        .bytes()
        .map(|byte| {
            if matches!(byte, b'\n' | b'\r') {
                byte
            } else {
                b' '
            }
        })
        .collect::<Vec<_>>();
    let mut language = None;
    for root in recovery.roots {
        copy_scripts(root, source.as_bytes(), &mut masked, &mut language)?;
    }
    Ok((
        String::from_utf8(masked).expect("masked Svelte source is valid UTF-8"),
        language.unwrap_or(SourceLanguage::JavaScript),
        recovery.error_lines,
    ))
}

fn copy_scripts(
    node: Node<'_>,
    source: &[u8],
    masked: &mut [u8],
    language: &mut Option<SourceLanguage>,
) -> Result<(), beholder_indexing::AnalyzerError> {
    if node.kind() == "script_element" {
        if node
            .parent()
            .is_none_or(|parent| parent.kind() != "document")
        {
            return Ok(());
        }
        let Some(script_language) = script_language(node, source) else {
            return Ok(());
        };
        if language
            .as_ref()
            .is_some_and(|language| *language != script_language)
        {
            return Err(UnsafeTreeRecovery::new(
                "Svelte",
                "multiple instance script languages cannot share one syntax tree",
            )
            .into());
        }
        *language = Some(script_language);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "raw_text" {
                masked[child.byte_range()].copy_from_slice(&source[child.byte_range()]);
            }
        }
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        copy_scripts(child, source, masked, language)?;
    }
    Ok(())
}

fn script_language(node: Node<'_>, source: &[u8]) -> Option<SourceLanguage> {
    let start_tag = node
        .named_children(&mut node.walk())
        .find(|child| child.kind() == "start_tag")?;
    let mut language = SourceLanguage::JavaScript;
    for attribute in start_tag
        .named_children(&mut start_tag.walk())
        .filter(|child| child.kind() == "attribute")
    {
        let attribute = attribute.utf8_text(source).ok()?;
        let (name, value) = attribute.split_once('=').unwrap_or((attribute, ""));
        let value = value.trim().trim_matches(['\'', '"']);
        match name.trim() {
            "module" => return None,
            "context" if value == "module" => return None,
            "lang" => {
                language = match value {
                    "js" | "javascript" => SourceLanguage::JavaScript,
                    "ts" | "typescript" => SourceLanguage::TypeScript,
                    _ => return None,
                };
            }
            _ => {}
        }
    }
    Some(language)
}

fn rune_name(name: &str) -> bool {
    matches!(
        name,
        "$state" | "$derived" | "$effect" | "$props" | "$bindable" | "$inspect" | "$host"
    )
}

fn is_rune(call: &Call) -> bool {
    (call.receiver.is_none() && rune_name(&call.name))
        || call.receiver.as_deref().is_some_and(|receiver| {
            rune_name(receiver)
                || receiver
                    .strip_prefix("$inspect")
                    .is_some_and(|suffix| suffix.trim_start().starts_with('('))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_instance_javascript_and_typescript() {
        let source = r#"
            <script module>export function serverOnly() {}</script>
            <script lang="coffee">export function unsupported() {}</script>
            <script lang="ts">export function visible(value: string) { return value }</script>
            <svelte:head><script type="application/ld+json">export function metadata() {}</script></svelte:head>
        "#;

        let analysis = crate::analyze(source, SourceLanguage::Svelte).unwrap();
        let names = analysis
            .definitions
            .iter()
            .map(|definition| definition.qualified_name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["visible"]);
    }

    #[test]
    fn uses_the_declared_instance_script_language() {
        assert!(
            crate::analyze(
                "<script>interface Hidden { run(): void }</script>",
                SourceLanguage::Svelte,
            )
            .is_err()
        );

        let analysis = crate::analyze(
            "<script lang=\"ts\">interface Visible { run(): void }</script>",
            SourceLanguage::Svelte,
        )
        .unwrap();
        assert!(
            analysis
                .definitions
                .iter()
                .any(|definition| definition.qualified_name == "Visible")
        );
    }

    #[test]
    fn keeps_instance_script_when_template_recovery_is_incomplete() {
        let source = r#"
            <script>export function visible() {}</script>
            {#if}
        "#;

        let analysis = crate::analyze(source, SourceLanguage::Svelte).unwrap();

        assert!(
            analysis
                .definitions
                .iter()
                .any(|definition| definition.qualified_name == "visible")
        );
        assert!(!analysis.parse_error_lines.is_empty());
    }

    #[test]
    fn collects_reactive_calls_without_emitting_runes() {
        let source = r#"
            <script>
              const count = $state(load());
              $: result = refresh(count);
              $effect(() => persist(count));
              $inspect (count).with(console.trace);
              api.$state();
            </script>
        "#;

        let analysis = crate::analyze(source, SourceLanguage::Svelte).unwrap();
        let calls = analysis
            .calls
            .iter()
            .chain(
                analysis
                    .definitions
                    .iter()
                    .flat_map(|definition| &definition.calls),
            )
            .collect::<Vec<_>>();

        for name in ["load", "refresh", "persist"] {
            assert!(calls.iter().any(|call| call.name == name));
        }
        assert!(
            calls
                .iter()
                .any(|call| call.receiver.as_deref() == Some("api") && call.name == "$state")
        );
        assert!(!calls.iter().any(|call| is_rune(call)));
    }
}
