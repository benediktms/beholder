use crate::{
    SourceLanguage, TypescriptAnalysis, analysis::analyze_core, plugin::TypescriptLanguage,
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
            version: "1".into(),
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

        let (source, error_lines) = extract_scripts(input.syntax.root_node(), input.text)?;
        let mut embedded = analyze_core(&source, SourceLanguage::TypeScript)?;
        embedded.nest_modules.append(&mut analysis.nest_modules);
        embedded.nest_providers.append(&mut analysis.nest_providers);
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
        .map(|(source, _)| source)
}

fn extract_scripts(
    root: Node<'_>,
    source: &str,
) -> Result<(String, Vec<usize>), beholder_indexing::AnalyzerError> {
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
    for root in recovery.roots {
        copy_scripts(root, source.as_bytes(), &mut masked);
    }
    Ok((
        String::from_utf8(masked).expect("masked Svelte source is valid UTF-8"),
        recovery.error_lines,
    ))
}

fn copy_scripts(node: Node<'_>, source: &[u8], masked: &mut [u8]) {
    if node.kind() == "script_element" {
        if node
            .parent()
            .is_none_or(|parent| parent.kind() != "document")
            || !supported_script(node, source)
        {
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "raw_text" {
                masked[child.byte_range()].copy_from_slice(&source[child.byte_range()]);
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        copy_scripts(child, source, masked);
    }
}

fn supported_script(node: Node<'_>, source: &[u8]) -> bool {
    let Some(start_tag) = node
        .named_children(&mut node.walk())
        .find(|child| child.kind() == "start_tag")
    else {
        return false;
    };
    start_tag
        .named_children(&mut start_tag.walk())
        .filter(|child| child.kind() == "attribute")
        .all(|attribute| {
            let Some(attribute) = attribute.utf8_text(source).ok() else {
                return false;
            };
            let (name, value) = attribute.split_once('=').unwrap_or((attribute, ""));
            let value = value.trim().trim_matches(['\'', '"']);
            match name.trim() {
                "module" => false,
                "context" => value != "module",
                "lang" => matches!(value, "js" | "javascript" | "ts" | "typescript"),
                _ => true,
            }
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
}
