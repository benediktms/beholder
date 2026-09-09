use crate::{
    SourceLanguage, TypescriptAnalysis, analysis::analyze_core, model::Call,
    plugin::TypescriptLanguage,
};
use beholder_adapters_treesitter::recover;
use beholder_domain::{
    ConditionArmKind, ConditionConstruct, EvidenceContext, SourceExcerpt, SourcePosition,
    SourceRange, UnsafeTreeRecovery,
};
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
            version: "3".into(),
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
        let mut template_errors = Vec::new();
        collect_template_calls(
            input.syntax.root_node(),
            input.text,
            language,
            &mut embedded.calls,
            &mut template_errors,
        );
        strip_runes(&mut embedded);
        for definition in &mut embedded.definitions {
            definition.exported = false;
        }
        embedded.exports.clear();
        embedded.parse_error_lines.extend(error_lines);
        embedded.parse_error_lines.extend(template_errors);
        embedded.parse_error_lines.sort_unstable();
        embedded.parse_error_lines.dedup();
        embedded.language = SourceLanguage::Svelte;
        *analysis = embedded;
        Ok(())
    }
}

fn source_position(source: &str, offset: usize) -> Option<SourcePosition> {
    let prefix = source.get(..offset)?;
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    Some(SourcePosition {
        line: line.try_into().ok()?,
        character: prefix[line_start..]
            .encode_utf16()
            .count()
            .try_into()
            .ok()?,
    })
}

fn byte_range(source: &str, start: usize, end: usize) -> Option<SourceRange> {
    Some(SourceRange {
        start: source_position(source, start)?,
        end: source_position(source, end)?,
    })
}

fn excerpt(node: Node<'_>, source: &str) -> Option<SourceExcerpt> {
    Some(SourceExcerpt {
        text: node.utf8_text(source.as_bytes()).ok()?.into(),
        range: byte_range(source, node.start_byte(), node.end_byte())?,
    })
}

fn contains(outer: Node<'_>, inner: Node<'_>) -> bool {
    outer.start_byte() <= inner.start_byte() && inner.end_byte() <= outer.end_byte()
}

fn start_child<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    node.named_children(&mut node.walk())
        .find(|child| child.kind() == kind)
}

fn preceding_condition(branch: Node<'_>) -> Option<Node<'_>> {
    let mut sibling = branch.prev_named_sibling();
    while let Some(candidate) = sibling {
        match candidate.kind() {
            "else_if_block" => {
                return start_child(candidate, "else_if_start")?.child_by_field_name("condition");
            }
            "if_start" => return candidate.child_by_field_name("condition"),
            _ => sibling = candidate.prev_named_sibling(),
        }
    }
    None
}

fn template_if_context(
    selection: Node<'_>,
    expression: Node<'_>,
    source: &str,
) -> Option<EvidenceContext> {
    let mut branch = expression;
    while branch.parent() != Some(selection) {
        branch = branch.parent()?;
    }
    let start = start_child(selection, "if_start")?;
    let (arm, condition, arm_range) = match branch.kind() {
        "if_start" => return None,
        "else_if_block" => {
            let branch_start = start_child(branch, "else_if_start")?;
            let condition = branch_start.child_by_field_name("condition")?;
            if contains(branch_start, expression) {
                return None;
            }
            (
                ConditionArmKind::ElseIf,
                condition,
                byte_range(source, branch_start.end_byte(), branch.end_byte())?,
            )
        }
        "else_block" => {
            let branch_start = start_child(branch, "else_start")?;
            (
                ConditionArmKind::Else,
                preceding_condition(branch)?,
                byte_range(source, branch_start.end_byte(), branch.end_byte())?,
            )
        }
        _ => {
            let end = selection
                .named_children(&mut selection.walk())
                .find(|child| matches!(child.kind(), "else_if_block" | "else_block" | "if_end"))?
                .start_byte();
            (
                ConditionArmKind::Then,
                start.child_by_field_name("condition")?,
                byte_range(source, start.end_byte(), end)?,
            )
        }
    };
    Some(EvidenceContext::ConditionArm {
        construct: ConditionConstruct::TemplateIf,
        arm,
        condition: excerpt(condition, source),
        arm_range,
    })
}

fn template_contexts(expression: Node<'_>, source: &str) -> Vec<EvidenceContext> {
    let mut contexts = Vec::new();
    let mut ancestor = expression.parent();
    while let Some(candidate) = ancestor {
        if candidate.kind() == "if_statement" {
            contexts.extend(template_if_context(candidate, expression, source));
        }
        ancestor = candidate.parent();
    }
    contexts.reverse();
    contexts
}

fn translate_position(position: &mut SourcePosition, base: SourcePosition) {
    if position.line == 0 {
        position.character += base.character;
    }
    position.line += base.line;
}

fn translate_range(range: &mut SourceRange, base: SourcePosition) {
    translate_position(&mut range.start, base);
    translate_position(&mut range.end, base);
}

fn translate_excerpt(excerpt: &mut SourceExcerpt, base: SourcePosition) {
    translate_range(&mut excerpt.range, base);
}

fn translate_context(context: &mut EvidenceContext, base: SourcePosition) {
    match context {
        EvidenceContext::ConditionArm {
            condition,
            arm_range,
            ..
        } => {
            if let Some(condition) = condition {
                translate_excerpt(condition, base);
            }
            translate_range(arm_range, base);
        }
        EvidenceContext::PatternArm {
            selector,
            pattern,
            guard,
            arm_range,
            ..
        } => {
            for excerpt in [selector, pattern, guard].into_iter().flatten() {
                translate_excerpt(excerpt, base);
            }
            translate_range(arm_range, base);
        }
        EvidenceContext::CallableClause {
            signature,
            guard,
            definition_range,
            ..
        } => {
            translate_excerpt(signature, base);
            if let Some(guard) = guard {
                translate_excerpt(guard, base);
            }
            translate_range(definition_range, base);
        }
    }
}

fn translate_call(call: &mut Call, base: SourcePosition) {
    call.line += base.line as usize;
    let mut start = SourcePosition {
        line: call.start_line,
        character: call.start_character,
    };
    let mut end = SourcePosition {
        line: call.end_line,
        character: call.end_character,
    };
    translate_position(&mut start, base);
    translate_position(&mut end, base);
    (call.start_line, call.start_character) = (start.line, start.character);
    (call.end_line, call.end_character) = (end.line, end.character);
    if let Some(range) = &mut call.range {
        translate_range(range, base);
    }
    for context in &mut call.contexts {
        translate_context(context, base);
    }
    call.scope_start = 0;
    call.scope_end = 0;
}

fn template_expression(node: Node<'_>) -> bool {
    node.kind() == "svelte_raw_text"
        && node.parent().is_some_and(|parent| {
            matches!(parent.kind(), "each_start")
                .then(|| parent.child_by_field_name("identifier") == Some(node))
                .unwrap_or_else(|| {
                    matches!(
                        parent.kind(),
                        "expression"
                            | "await_start"
                            | "if_start"
                            | "else_if_start"
                            | "html_tag"
                            | "key_start"
                            | "const_tag"
                            | "debug_tag"
                            | "render_tag"
                    )
                })
        })
}

fn collect_template_calls(
    node: Node<'_>,
    source: &str,
    language: SourceLanguage,
    calls: &mut Vec<Call>,
    error_lines: &mut Vec<usize>,
) {
    if template_expression(node) {
        let base = source_position(source, node.start_byte()).expect("Svelte node is in source");
        let contexts = template_contexts(node, source);
        match node
            .utf8_text(source.as_bytes())
            .ok()
            .and_then(|expression| analyze_core(expression, language).ok())
        {
            Some(mut analysis) => {
                error_lines.extend(
                    analysis
                        .parse_error_lines
                        .drain(..)
                        .map(|line| base.line as usize + line),
                );
                let nested_calls = analysis
                    .definitions
                    .into_iter()
                    .flat_map(|definition| definition.calls);
                for mut call in analysis.calls.into_iter().chain(nested_calls) {
                    translate_call(&mut call, base);
                    call.contexts.splice(0..0, contexts.iter().cloned());
                    calls.push(call);
                }
            }
            None => error_lines.push(base.line as usize + 1),
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_template_calls(child, source, language, calls, error_lines);
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
    analysis
        .calls
        .retain(|call| !is_rune(call, "", &store_bindings));
    for definition in &mut analysis.definitions {
        let scope = definition.qualified_name.clone();
        let parent = scope.rsplit_once('/').map_or("", |(parent, _)| parent);
        definition
            .calls
            .retain(|call| !is_rune(call, &scope, &store_bindings));
        if definition.factory.as_deref().is_some_and(|factory| {
            rune_factory(factory) && !legacy_store(factory, parent, &store_bindings)
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

fn is_rune(call: &Call, scope: &str, store_bindings: &BTreeMap<String, BTreeSet<String>>) -> bool {
    (call.receiver.is_none()
        && rune_name(&call.name)
        && !legacy_store(&call.name, scope, store_bindings))
        || call
            .receiver
            .as_deref()
            .is_some_and(|receiver| rune_method(receiver, &call.name) || inspect_receiver(receiver))
}

fn legacy_store(
    name: &str,
    mut scope: &str,
    store_bindings: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    let Some(name) = name.strip_prefix('$') else {
        return false;
    };
    loop {
        if store_bindings
            .get(scope)
            .is_some_and(|bindings| bindings.contains(name))
        {
            return true;
        }
        if scope.is_empty() {
            return false;
        }
        scope = scope.rsplit_once('/').map_or("", |(parent, _)| parent);
    }
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
    use beholder_domain::{DependencyRelation, SemanticRelation};

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
    fn template_calls_are_component_owned_without_tag_references() {
        let source = r#"<script>function scripted() { load(); }</script>
            <Widget on:click={outer(inner())}>{items.map(item => render(format(item)))}</Widget>
            {#each loadItems() as item}{show(item)}{/each}
            {#await loadPromise()}{:then value}{present(value)}{/await}
            {#key keyFor()}{keyed()}{/key}"#;
        let analysis = crate::analyze(source, SourceLanguage::Svelte).unwrap();
        let observations = crate::observations_from_analysis(
            "example",
            &analysis,
            source,
            std::path::Path::new("src/view.svelte"),
        );
        let calls = observations
            .iter()
            .filter(|observation| {
                observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
            })
            .collect::<Vec<_>>();

        for name in [
            "outer",
            "inner",
            "map",
            "render",
            "format",
            "loadItems",
            "show",
            "loadPromise",
            "present",
            "keyFor",
            "keyed",
        ] {
            assert!(calls.iter().any(|observation| {
                observation.from.as_str() == "repo://example/svelte/src/view"
                    && observation.to.as_str().ends_with(&format!("/{name}"))
            }));
        }
        assert!(calls.iter().any(|observation| {
            observation.from.as_str() == "repo://example/svelte/src/view/scripted"
                && observation.to.as_str().ends_with("/load")
        }));
        assert!(!calls.iter().any(|observation| {
            observation.to.as_str().ends_with("/Widget")
                || observation.to.as_str().contains("on:click")
        }));
    }

    #[test]
    fn template_if_arms_and_ternaries_keep_original_utf16_ranges() {
        let source = r#"<p>😀</p>
{#if deciding()}
  {thenCall()}
{:else if alternate()}
  {choice() ? consequence() : alternative()}
{:else}
  {fallback()}
{/if}"#;
        let analysis = crate::analyze(source, SourceLanguage::Svelte).unwrap();
        let call = |name: &str| {
            analysis
                .calls
                .iter()
                .find(|call| call.name == name)
                .unwrap()
        };

        assert!(!call("deciding").contexts.iter().any(|context| matches!(
            context,
            EvidenceContext::ConditionArm {
                construct: ConditionConstruct::TemplateIf,
                ..
            }
        )));
        assert!(!call("alternate").contexts.iter().any(|context| matches!(
            context,
            EvidenceContext::ConditionArm {
                construct: ConditionConstruct::TemplateIf,
                arm: ConditionArmKind::ElseIf,
                ..
            }
        )));
        assert!(matches!(
            call("thenCall").contexts.as_slice(),
            [EvidenceContext::ConditionArm {
                construct: ConditionConstruct::TemplateIf,
                arm: ConditionArmKind::Then,
                ..
            }]
        ));
        assert!(matches!(
            call("consequence").contexts.as_slice(),
            [
                EvidenceContext::ConditionArm {
                    construct: ConditionConstruct::TemplateIf,
                    arm: ConditionArmKind::ElseIf,
                    ..
                },
                EvidenceContext::ConditionArm {
                    construct: ConditionConstruct::Ternary,
                    arm: ConditionArmKind::Consequence,
                    ..
                }
            ]
        ));
        assert!(matches!(
            call("fallback").contexts.as_slice(),
            [EvidenceContext::ConditionArm {
                construct: ConditionConstruct::TemplateIf,
                arm: ConditionArmKind::Else,
                condition: Some(SourceExcerpt { text, .. }),
                ..
            }] if text == "alternate()"
        ));
        let range = call("thenCall").range.as_ref().unwrap();
        assert_eq!(range.start.line, 2);
        assert_eq!(range.start.character, 3);
        assert_eq!(range.end.character - range.start.character, 10);
    }

    #[test]
    fn malformed_template_expression_preserves_other_calls_and_reports_its_line() {
        let source = "{valid()}\n{broken(}\n{alsoValid()}";
        let analysis = crate::analyze(source, SourceLanguage::Svelte).unwrap();

        assert!(analysis.calls.iter().any(|call| call.name == "valid"));
        assert!(analysis.calls.iter().any(|call| call.name == "alsoValid"));
        assert!(analysis.parse_error_lines.contains(&2));
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
        assert!(!calls.iter().any(|call| is_rune(call, "", &BTreeMap::new())));
    }

    #[test]
    fn preserves_callable_legacy_store_subscriptions() {
        let analysis = crate::analyze(
            "<script>import { writable } from 'svelte/store'; const state = writable(load); const current = $state(); function invoke() { return $state(); }</script>",
            SourceLanguage::Svelte,
        )
        .unwrap();

        assert!(analysis.calls.iter().any(|call| call.name == "$state"));
        assert!(analysis.definitions.iter().any(|definition| {
            definition.qualified_name == "current"
                && definition.factory.as_deref() == Some("$state")
        }));
        assert!(analysis.definitions.iter().any(|definition| {
            definition.qualified_name == "invoke"
                && definition.calls.iter().any(|call| call.name == "$state")
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
                .any(|call| is_rune(call, "", &BTreeMap::new()))
        );
    }
}
