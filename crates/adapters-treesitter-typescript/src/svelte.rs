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
use std::collections::{BTreeMap, BTreeSet};
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
        if is_svelte_module(input.path) {
            strip_runes(analysis);
            return Ok(());
        }
        if !is_svelte_component(input.path) {
            return Ok(());
        }

        let (source, language, error_lines) =
            extract_scripts(input.syntax.root_node(), input.text)?;
        let mut embedded = analyze_core(&source, language)?;
        strip_runes(&mut embedded);
        for definition in &mut embedded.definitions {
            definition.exported = false;
        }
        embedded.exports.clear();
        embedded.parse_error_lines.extend(error_lines);
        embedded.parse_error_lines.sort_unstable();
        embedded.parse_error_lines.dedup();
        embedded.language = SourceLanguage::Svelte;
        *analysis = embedded;
        Ok(())
    }
}

fn strip_runes(analysis: &mut TypescriptAnalysis) {
    let mut store_factories = BTreeSet::new();
    for binding in analysis
        .imports
        .iter()
        .filter(|import| import.source == "svelte/store")
        .flat_map(|import| &import.bindings)
    {
        if binding.imported == "*" {
            store_factories.extend(
                ["derived", "readable", "readonly", "toStore", "writable"]
                    .map(|factory| format!("{}.{factory}", binding.local)),
            );
        } else if store_factory(&binding.imported) {
            store_factories.insert(binding.local.clone());
        }
    }
    let mut store_bindings = BTreeMap::<String, BTreeSet<String>>::new();
    for definition in &analysis.definitions {
        if !definition
            .factory
            .as_ref()
            .is_some_and(|factory| store_factories.contains(factory))
        {
            continue;
        }
        let (scope, name) = definition
            .qualified_name
            .rsplit_once('/')
            .unwrap_or(("", &definition.qualified_name));
        store_bindings
            .entry(scope.to_owned())
            .or_default()
            .insert(name.to_owned());
    }
    let empty = BTreeSet::new();
    analysis
        .calls
        .retain(|call| !is_rune(call, store_bindings.get("").unwrap_or(&empty)));
    for definition in &mut analysis.definitions {
        let parent = definition
            .qualified_name
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent);
        definition.calls.retain(|call| {
            !is_rune(
                call,
                store_bindings
                    .get(&definition.qualified_name)
                    .unwrap_or(&empty),
            )
        });
        if definition.factory.as_deref().is_some_and(|factory| {
            rune_factory(factory)
                && !legacy_store(factory, store_bindings.get(parent).unwrap_or(&empty))
        }) {
            definition.factory = None;
        }
    }
}

fn is_svelte(path: &std::path::Path) -> bool {
    is_svelte_component(path) || is_svelte_module(path)
}

fn is_svelte_component(path: &std::path::Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("svelte")
}

fn is_svelte_module(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".svelte.ts") || name.ends_with(".svelte.js"))
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

fn rune_method(receiver: &str, name: &str) -> bool {
    matches!(
        (receiver, name),
        ("$state", "raw" | "snapshot" | "eager")
            | ("$derived", "by")
            | ("$effect", "pre" | "tracking" | "pending" | "root")
            | ("$props", "id")
            | ("$inspect", "trace")
    )
}

fn rune_factory(name: &str) -> bool {
    rune_name(name)
        || name
            .rsplit_once('.')
            .is_some_and(|(receiver, name)| rune_method(receiver, name))
}

fn store_factory(name: &str) -> bool {
    matches!(
        name,
        "derived" | "readable" | "readonly" | "toStore" | "writable"
    )
}

fn is_rune(call: &Call, store_bindings: &BTreeSet<String>) -> bool {
    (call.receiver.is_none() && rune_name(&call.name) && !legacy_store(&call.name, store_bindings))
        || call
            .receiver
            .as_deref()
            .is_some_and(|receiver| rune_method(receiver, &call.name) || inspect_receiver(receiver))
}

fn legacy_store(name: &str, store_bindings: &BTreeSet<String>) -> bool {
    name.strip_prefix('$')
        .is_some_and(|name| store_bindings.contains(name))
}

fn inspect_receiver(receiver: &str) -> bool {
    let Some(mut suffix) = receiver.strip_prefix("$inspect") else {
        return false;
    };
    loop {
        suffix = suffix.trim_start();
        if suffix.starts_with('(') {
            return true;
        }
        if suffix.starts_with('<') {
            let mut depth = 0;
            let mut quote = None;
            let mut escaped = false;
            let mut line_comment = false;
            let mut block_comment = false;
            let mut previous = None;
            let Some(end) = suffix.char_indices().find_map(|(index, character)| {
                if line_comment {
                    if matches!(character, '\r' | '\n') {
                        line_comment = false;
                    }
                    return None;
                }
                if block_comment {
                    if previous == Some('*') && character == '/' {
                        block_comment = false;
                        previous = None;
                    } else {
                        previous = Some(character);
                    }
                    return None;
                }
                if let Some(delimiter) = quote {
                    if escaped {
                        escaped = false;
                    } else if character == '\\' {
                        escaped = true;
                    } else if character == delimiter {
                        quote = None;
                    }
                    return None;
                }
                if previous == Some('/') && character == '/' {
                    line_comment = true;
                    previous = None;
                    return None;
                }
                if previous == Some('/') && character == '*' {
                    block_comment = true;
                    previous = None;
                    return None;
                }
                match character {
                    '\'' | '"' | '`' => {
                        quote = Some(character);
                    }
                    '<' => depth += 1,
                    '>' if !suffix[..index].ends_with('=') => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(index + character.len_utf8());
                        }
                    }
                    _ => {}
                }
                previous = Some(character);
                None
            }) else {
                return false;
            };
            suffix = &suffix[end..];
            continue;
        }
        if let Some(rest) = suffix
            .strip_prefix("/*")
            .and_then(|comment| comment.split_once("*/").map(|(_, rest)| rest))
        {
            suffix = rest;
            continue;
        }
        if let Some(rest) = suffix.strip_prefix("//").and_then(|comment| {
            comment
                .find(['\r', '\n'])
                .map(|end| &comment[end.saturating_add(1)..])
        }) {
            suffix = rest;
            continue;
        }
        return false;
    }
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
        assert!(!analysis.definitions[0].exported);
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
              const state = $state(0);
              const rawState = $state.raw(0);
              const raw = $state.raw(loadRaw());
              $: result = refresh(count);
              $effect(() => persist(count));
              $inspect (count).with(console.trace);
              $inspect /* reason */ (count).with(console.trace);
              $inspect<[number]>(count).with(console.trace);
              $inspect<(value: number) => number>(format).with(console.trace);
              $inspect<">">(count).with(console.trace);
              $inspect<Foo /* > */>(count).with(console.trace);
              $state.refresh();
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

        for name in ["load", "loadRaw", "refresh", "persist"] {
            assert!(calls.iter().any(|call| call.name == name));
        }
        assert!(
            calls
                .iter()
                .any(|call| call.receiver.as_deref() == Some("api") && call.name == "$state")
        );
        assert!(
            calls
                .iter()
                .any(|call| call.receiver.as_deref() == Some("$state") && call.name == "refresh")
        );
        assert!(!calls.iter().any(|call| {
            call.name == "with"
                && call
                    .receiver
                    .as_deref()
                    .is_some_and(|receiver| receiver.starts_with("$inspect"))
        }));
        assert!(!calls.iter().any(|call| is_rune(call, &BTreeSet::new())));
    }

    #[test]
    fn preserves_callable_legacy_store_subscriptions() {
        let analysis = crate::analyze(
            "<script>import { writable } from 'svelte/store'; const state = writable(load); const current = $state();</script>",
            SourceLanguage::Svelte,
        )
        .unwrap();

        assert!(analysis.calls.iter().any(|call| call.name == "$state"));
        assert!(analysis.definitions.iter().any(|definition| {
            definition.qualified_name == "current"
                && definition.factory.as_deref() == Some("$state")
        }));
    }

    #[test]
    fn unrelated_factories_do_not_disguise_runes_as_stores() {
        let analysis = crate::analyze(
            "<script>const state = load(); function nested() { const state = createStore(); } const reactive = $state(0);</script>",
            SourceLanguage::Svelte,
        )
        .unwrap();

        assert!(!analysis.calls.iter().any(|call| call.name == "$state"));
        assert!(analysis.definitions.iter().any(|definition| {
            definition.qualified_name == "reactive" && definition.factory.is_none()
        }));
    }

    #[test]
    fn clears_instance_export_clauses() {
        let analysis = crate::analyze(
            "<script>function save() {} export { save };</script>",
            SourceLanguage::Svelte,
        )
        .unwrap();

        assert!(analysis.exports.is_empty());
    }

    #[test]
    fn processes_svelte_rune_modules_without_clearing_exports() {
        let source = "export const state = $state.raw(0); export const other = $state(1);";
        let path = std::path::Path::new("state.svelte.ts");
        let plugins = crate::plugin::built_in_plugins().unwrap();
        let active = plugins.activate_direct(path);
        let analysis = crate::analysis::analyze_with_plugins(
            source,
            SourceLanguage::TypeScript,
            path,
            &plugins,
            &active,
        )
        .unwrap();

        assert!(
            analysis
                .definitions
                .iter()
                .all(|definition| definition.exported)
        );
        assert!(
            analysis
                .definitions
                .iter()
                .all(|definition| definition.factory.is_none())
        );
        assert!(
            !analysis
                .calls
                .iter()
                .any(|call| is_rune(call, &BTreeSet::new()))
        );
    }
}
