use crate::{SourceLanguage, TypescriptAnalysis, plugin::TypescriptLanguage};
use beholder_adapters_treesitter::recover;
use beholder_domain::UnsafeTreeRecovery;
use beholder_indexing::{
    LanguageAnalyzerBuilder, Plugin, PluginActivation, PluginMetadata, RepositorySnapshot,
    SourceRecognitionInput, SourceRecognizer,
};
use tree_sitter::Node;

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

        let recovery = recover(input.syntax.root_node()).map_err(|_| {
            UnsafeTreeRecovery::new("Svelte", "missing syntax may change script boundaries")
        })?;
        let mut masked = input
            .text
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
            copy_scripts(root, input.text.as_bytes(), &mut masked);
        }

        let mut embedded = crate::analyze(
            &String::from_utf8(masked).expect("masked Svelte source is valid UTF-8"),
            SourceLanguage::TypeScript,
        )?;
        embedded.parse_error_lines.extend(recovery.error_lines);
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

fn copy_scripts(node: Node<'_>, source: &[u8], masked: &mut [u8]) {
    if node.kind() == "script_element" {
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
