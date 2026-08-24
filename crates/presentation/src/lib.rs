use beholder_dto::{ContextResult, DependenciesResult, ImpactResult, TraceResult, WhyResult};
use serde::Serialize;

mod context;
mod path;
mod projection;
mod traversal;

use context::context_human;
use path::{trace_human, why_human};
use projection::raw;
pub use projection::{Visibility, visibility};
use traversal::{dependencies_human, impact_human};

#[cfg(test)]
use beholder_dto::{
    EntityKind, EntityOrigin, EntityRef, EvidenceRef, SemanticEdge, SemanticPath, TraversalMetadata,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputMode {
    Human,
    Json,
    JsonPretty,
    Raw,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderOptions {
    pub mode: OutputMode,
    pub include_tests: bool,
    pub include_diagnostics: bool,
}

impl From<OutputMode> for RenderOptions {
    fn from(mode: OutputMode) -> Self {
        Self {
            mode,
            include_tests: false,
            include_diagnostics: false,
        }
    }
}

pub fn context(
    result: &ContextResult,
    options: RenderOptions,
) -> Result<String, serde_json::Error> {
    match options.mode {
        OutputMode::Json => json(result, false),
        OutputMode::JsonPretty => json(result, true),
        OutputMode::Raw => Ok(raw(
            &result.schema,
            &result.metadata,
            &result.nodes,
            &result.edges,
            &[],
            None,
        )),
        OutputMode::Human => Ok(context_human(
            result,
            options.include_tests,
            options.include_diagnostics,
        )),
    }
}

pub fn dependencies(
    result: &DependenciesResult,
    options: RenderOptions,
) -> Result<String, serde_json::Error> {
    match options.mode {
        OutputMode::Json => json(result, false),
        OutputMode::JsonPretty => json(result, true),
        OutputMode::Raw => Ok(raw(
            &result.schema,
            &result.metadata,
            &result.nodes,
            &result.edges,
            &[],
            Some(&result.traversal),
        )),
        OutputMode::Human => Ok(dependencies_human(
            result,
            options.include_tests,
            options.include_diagnostics,
        )),
    }
}

pub fn impact(result: &ImpactResult, options: RenderOptions) -> Result<String, serde_json::Error> {
    match options.mode {
        OutputMode::Json => json(result, false),
        OutputMode::JsonPretty => json(result, true),
        OutputMode::Raw => Ok(raw(
            &result.schema,
            &result.metadata,
            &result.nodes,
            &result.edges,
            &[],
            Some(&result.traversal),
        )),
        OutputMode::Human => Ok(impact_human(
            result,
            options.include_tests,
            options.include_diagnostics,
        )),
    }
}

pub fn trace(result: &TraceResult, options: RenderOptions) -> Result<String, serde_json::Error> {
    match options.mode {
        OutputMode::Json => json(result, false),
        OutputMode::JsonPretty => json(result, true),
        OutputMode::Raw => Ok(raw(
            &result.schema,
            &result.metadata,
            &result.nodes,
            &result.edges,
            &result.paths,
            Some(&result.traversal),
        )),
        OutputMode::Human => Ok(trace_human(
            result,
            options.include_tests,
            options.include_diagnostics,
        )),
    }
}

pub fn why(result: &WhyResult, options: RenderOptions) -> Result<String, serde_json::Error> {
    match options.mode {
        OutputMode::Json => json(result, false),
        OutputMode::JsonPretty => json(result, true),
        OutputMode::Raw => Ok(raw(
            &result.schema,
            &result.metadata,
            &result.nodes,
            &result.edges,
            &result.paths,
            Some(&result.traversal),
        )),
        OutputMode::Human => Ok(why_human(
            result,
            options.include_tests,
            options.include_diagnostics,
        )),
    }
}

fn json(value: &impl Serialize, pretty: bool) -> Result<String, serde_json::Error> {
    if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        projection::{entity_names, name},
        traversal::display_names,
    };
    use beholder_dto::{
        AnalysisCompleteness, AnalysisDiagnostic, AnalysisDiagnosticSeverity, AnalysisMetadata,
        CONTEXT_SCHEMA_V1, ContextResult, DEPENDENCIES_SCHEMA_V2, DependenciesResult, EntityQuery,
        EvidenceKind, Freshness, IMPACT_SCHEMA_V2, ImpactRef, ImpactResult, PathQuery,
        QueryMetadata, RelationKind, TRACE_SCHEMA_V2,
    };

    fn traversal() -> TraversalMetadata {
        TraversalMetadata {
            max_hops: 32,
            truncated: false,
        }
    }

    fn trace_result() -> TraceResult {
        TraceResult {
            schema: TRACE_SCHEMA_V2.into(),
            metadata: QueryMetadata {
                revision: 42,
                view: "main".into(),
                freshness: Freshness {
                    stale: false,
                    indexing: false,
                    dirty_repositories: Vec::new(),
                    enriching_repositories: Vec::new(),
                },
                analysis: Default::default(),
            },
            query: PathQuery {
                from: "a".into(),
                to: "rpc".into(),
            },
            traversal: traversal(),
            nodes: vec![
                entity("a", "CheckoutPage", EntityKind::Callable, false),
                entity("generated", "PricingClient", EntityKind::Callable, true),
                entity("rpc", "Pricing.GetPrice", EntityKind::Rpc, false),
            ],
            edges: vec![
                edge("e1", "a", "generated", "calls", "src/checkout.rs", 12),
                edge("e2", "generated", "rpc", "calls_rpc", "generated.rs", 8),
            ],
            paths: vec![SemanticPath {
                nodes: vec!["a".into(), "generated".into(), "rpc".into()],
                edges: vec!["e1".into(), "e2".into()],
            }],
        }
    }

    fn entity(id: &str, name: &str, kind: EntityKind, generated: bool) -> EntityRef {
        EntityRef {
            id: id.into(),
            kind,
            name: name.into(),
            repository: Some("repo".into()),
            origin: if generated {
                EntityOrigin::Generated
            } else {
                EntityOrigin::Source
            },
            test: false,
            metadata: None,
        }
    }

    fn edge(id: &str, from: &str, to: &str, kind: &str, path: &str, line: u32) -> SemanticEdge {
        SemanticEdge {
            id: id.into(),
            from: from.into(),
            to: to.into(),
            kind: RelationKind::try_from(kind).unwrap(),
            confidence: 1.0,
            evidence: vec![EvidenceRef {
                source_kind: EvidenceKind::Ast,
                repository: Some("repo".into()),
                path: Some(path.into()),
                line: Some(line),
                detail: None,
            }],
        }
    }

    #[test]
    fn trace_json_is_versioned_and_compact_output_is_deterministic() {
        let mut result = trace_result();
        let json = trace(&result, OutputMode::Json.into()).unwrap();
        assert!(json.starts_with(r#"{"schema":"beholder.trace.v2","revision":42,"view":"main""#));
        assert!(json.contains(r#""traversal":{"max_hops":32,"truncated":false}"#));
        assert!(!json.contains(r#""analysis""#));
        assert_eq!(
            trace(&result, OutputMode::Human.into()).unwrap(),
            "repo · CheckoutPage\n  → repo · Pricing.GetPrice [calls_rpc]\n\n1 hop · 1 repositories · confidence 1.00\ntraversal complete · max depth 32\nview main · revision 42 · stale=false · indexing=false"
        );

        result.metadata.freshness.indexing = true;
        result.metadata.freshness.enriching_repositories = vec!["repo".into()];
        assert!(
            trace(&result, OutputMode::Human.into())
                .unwrap()
                .ends_with("stale=false · indexing=true · enriching=repo")
        );
    }

    #[test]
    fn incomplete_analysis_is_visible_and_deterministic() {
        let mut result = trace_result();
        result.metadata.analysis = AnalysisMetadata {
            completeness: AnalysisCompleteness::Incomplete,
            diagnostics: vec![
                AnalysisDiagnostic {
                    code: "z-last".into(),
                    severity: AnalysisDiagnosticSeverity::Warning,
                    repository: "repo".into(),
                    path: "src/z.ts".into(),
                    line: None,
                    detail: None,
                },
                AnalysisDiagnostic {
                    code: "a-first".into(),
                    severity: AnalysisDiagnosticSeverity::KnownLimitation,
                    repository: "repo".into(),
                    path: "src/a.ts".into(),
                    line: Some(7),
                    detail: Some("recovered".into()),
                },
            ],
        };

        let human = trace(&result, OutputMode::Human.into()).unwrap();
        assert!(human.contains("analysis incomplete · 2 diagnostics"));
        assert!(!human.contains("known_limitation repo src/a.ts:7 a-first recovered"));
        let verbose = trace(
            &result,
            RenderOptions {
                mode: OutputMode::Human,
                include_tests: false,
                include_diagnostics: true,
            },
        )
        .unwrap();
        assert!(verbose.contains("known_limitation repo src/a.ts:7 a-first recovered"));
        let raw = trace(&result, OutputMode::Raw.into()).unwrap();
        assert!(raw.contains("incomplete=true"));
        assert!(raw.contains("\ndiagnostics\n"));
        assert!(raw.find("a-first").unwrap() < raw.find("z-last").unwrap());
        let json = trace(&result, OutputMode::Json.into()).unwrap();
        assert!(json.contains(r#""completeness":"incomplete""#));
        assert!(json.contains(r#""code":"a-first""#));
    }

    #[test]
    fn incomplete_traversal_is_explicit() {
        let mut result = trace_result();
        result.paths.clear();
        result.traversal.max_hops = 1;
        result.traversal.truncated = true;
        let output = trace(&result, OutputMode::Human.into()).unwrap();
        assert!(output.contains("traversal incomplete · depth limit 1 reached"));
    }

    #[test]
    fn raw_retains_hidden_supporting_nodes_and_why_retains_evidence() {
        let result = trace_result();
        let raw = trace(&result, OutputMode::Raw.into()).unwrap();
        assert!(raw.contains("generated [Callable] origin=Generated"));
        let why_result = WhyResult::from(result);
        let human = why(&why_result, OutputMode::Human.into()).unwrap();
        assert!(human.contains("src/checkout.rs:12"));
        assert!(human.contains("generated.rs:8"));
        for mode in [OutputMode::Json, OutputMode::JsonPretty, OutputMode::Raw] {
            let output = why(&why_result, mode.into()).unwrap();
            assert!(output.contains("42"));
            assert!(output.contains("main"));
        }
    }

    #[test]
    fn freshness_and_revision_survive_every_renderer() {
        let trace_result = trace_result();
        let root = trace_result.nodes[0].clone();
        let context_result = ContextResult {
            schema: CONTEXT_SCHEMA_V1.into(),
            metadata: trace_result.metadata.clone(),
            query: EntityQuery {
                entity: root.id.clone(),
            },
            root: root.clone(),
            nodes: trace_result.nodes.clone(),
            edges: trace_result.edges.clone(),
        };
        let dependencies_result = DependenciesResult {
            schema: DEPENDENCIES_SCHEMA_V2.into(),
            metadata: trace_result.metadata.clone(),
            query: EntityQuery {
                entity: root.id.clone(),
            },
            traversal: traversal(),
            root: root.clone(),
            dependencies: Vec::new(),
            nodes: trace_result.nodes.clone(),
            edges: trace_result.edges.clone(),
        };
        let impact_result = ImpactResult {
            schema: IMPACT_SCHEMA_V2.into(),
            metadata: trace_result.metadata.clone(),
            query: EntityQuery {
                entity: root.id.clone(),
            },
            traversal: traversal(),
            root,
            affected: Vec::new(),
            nodes: trace_result.nodes.clone(),
            edges: trace_result.edges.clone(),
        };
        let why_result = WhyResult::from(trace_result.clone());
        for mode in [
            OutputMode::Human,
            OutputMode::Json,
            OutputMode::JsonPretty,
            OutputMode::Raw,
        ] {
            for output in [
                context(&context_result, mode.into()).unwrap(),
                dependencies(&dependencies_result, mode.into()).unwrap(),
                impact(&impact_result, mode.into()).unwrap(),
                trace(&trace_result, mode.into()).unwrap(),
                why(&why_result, mode.into()).unwrap(),
            ] {
                assert!(output.contains("42"));
                assert!(output.contains("main"));
            }
        }
    }

    #[test]
    fn context_labels_incoming_calls_from_the_root_perspective() {
        let trace = trace_result();
        let result = ContextResult {
            schema: CONTEXT_SCHEMA_V1.into(),
            metadata: trace.metadata,
            query: EntityQuery {
                entity: "generated".into(),
            },
            root: trace.nodes[1].clone(),
            nodes: trace.nodes,
            edges: trace.edges,
        };

        assert!(
            context(&result, OutputMode::Human.into())
                .unwrap()
                .contains("← repo · CheckoutPage [called by]")
        );
    }

    #[test]
    fn context_names_include_repository_and_containing_scope() {
        let elixir = entity(
            "repo://repo/elixir/Example.Client/run/1",
            "run/1",
            EntityKind::Callable,
            false,
        );
        let rust = entity(
            "repo://repo/rust/src/client/impl/Client/run",
            "run",
            EntityKind::Callable,
            false,
        );
        let nodes = [elixir, rust];
        let names = entity_names(&nodes);

        assert_eq!(
            name(&names, "repo://repo/elixir/Example.Client/run/1"),
            "repo · Example.Client · run/1"
        );
        assert_eq!(
            name(&names, "repo://repo/rust/src/client/impl/Client/run"),
            "repo · Client · run"
        );
    }

    #[test]
    fn prefab_dependencies_show_direct_script_types() {
        let root = entity(
            "repo://repo/unity-prefab/Assets/Player.prefab",
            "Player.prefab",
            EntityKind::UnityPrefab,
            false,
        );
        let script = entity(
            "repo://repo/csharp/Assembly-CSharp/Assets/Player/Game/Player",
            "Player",
            EntityKind::Namespace,
            false,
        );
        let mut external = entity(
            "unity://Unity.InputSystem/UnityEngine/InputSystem/PlayerInput",
            "PlayerInput",
            EntityKind::Namespace,
            false,
        );
        external.repository = None;
        external.origin = EntityOrigin::ExternalDependency;
        let result = DependenciesResult {
            schema: DEPENDENCIES_SCHEMA_V2.into(),
            metadata: QueryMetadata::completed("main", 42),
            query: EntityQuery {
                entity: root.id.clone(),
            },
            traversal: traversal(),
            root: root.clone(),
            dependencies: vec![
                beholder_dto::DependencyRef {
                    entity: script.id.clone(),
                    hops: 1,
                },
                beholder_dto::DependencyRef {
                    entity: external.id.clone(),
                    hops: 1,
                },
            ],
            nodes: vec![root, script, external],
            edges: Vec::new(),
        };

        let output = dependencies(&result, OutputMode::Human.into()).unwrap();
        assert!(output.contains("Game · Player"));
        assert!(output.contains("cross-repository · PlayerInput"));
        assert!(output.contains("2 dependencies"));
    }

    #[test]
    fn impact_hides_tests_by_default_and_groups_included_tests_by_file() {
        let mut test = entity(
            "repo/rust/src/tests/test_checkout",
            "test_checkout",
            EntityKind::Callable,
            false,
        );
        test.test = true;
        let root = entity("root", "checkout", EntityKind::Callable, false);
        let result = ImpactResult {
            schema: IMPACT_SCHEMA_V2.into(),
            metadata: QueryMetadata::completed("main", 42),
            query: EntityQuery {
                entity: root.id.clone(),
            },
            traversal: traversal(),
            root: root.clone(),
            affected: vec![ImpactRef {
                entity: test.id.clone(),
                hops: 1,
            }],
            nodes: vec![root, test.clone()],
            edges: vec![edge(
                "e1",
                &test.id,
                "root",
                "calls",
                "src/checkout/tests.rs",
                9,
            )],
        };

        let compact = impact(&result, OutputMode::Human.into()).unwrap();
        assert!(!compact.contains("test_checkout"));
        assert!(compact.contains("1 tests hidden"));

        let included = impact(
            &result,
            RenderOptions {
                mode: OutputMode::Human,
                include_tests: true,
                include_diagnostics: false,
            },
        )
        .unwrap();
        assert!(included.contains("src/checkout/tests.rs\n    - repo · test_checkout"));
    }

    #[test]
    fn impact_names_always_include_repository_and_containing_scope() {
        let first = entity(
            "repo://repo/rust/crates/cli/src/commands/analyses/run/impl/AnalysisCtx/new",
            "new",
            EntityKind::Callable,
            false,
        );
        let second = entity(
            "repo://repo/rust/crates/cli/src/commands/artifacts/run/impl/ArtifactCtx/new",
            "new",
            EntityKind::Callable,
            false,
        );

        let nodes = [first.clone(), second.clone()];
        let names = entity_names(&nodes);
        assert_eq!(
            display_names(&[&first, &second], &names),
            ["repo · AnalysisCtx · new", "repo · ArtifactCtx · new"]
        );
    }

    #[test]
    fn duplicate_labels_expand_to_the_shortest_unique_scope() {
        let first = entity(
            "repo://repo/rust/crates/one/src/client/run",
            "run",
            EntityKind::Callable,
            false,
        );
        let second = entity(
            "repo://repo/rust/crates/two/src/client/run",
            "run",
            EntityKind::Callable,
            false,
        );
        let nodes = [first, second];
        let names = entity_names(&nodes);

        assert_eq!(
            names.values().cloned().collect::<Vec<_>>(),
            ["repo · one/src/client · run", "repo · two/src/client · run"]
        );
    }
}
