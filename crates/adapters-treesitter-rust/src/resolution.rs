use beholder_domain::{
    Confidence, DependencyOverride, DependencyRelation, EntityId, Observation, Provenance,
    SemanticRelation, StructuralRelation,
};
use std::collections::BTreeMap;

pub fn resolve_repository_calls(observations: &mut [Observation]) -> Vec<DependencyOverride> {
    let mut definitions = BTreeMap::<String, Option<String>>::new();
    for observation in observations.iter().filter(|observation| {
        observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
            && observation.to.as_str().contains("/rust/")
    }) {
        let Some(name) = observation.to.as_str().rsplit('/').next() else {
            continue;
        };
        definitions
            .entry(name.to_owned())
            .and_modify(|candidate| {
                if candidate.as_deref() != Some(observation.to.as_str()) {
                    *candidate = None;
                }
            })
            .or_insert_with(|| Some(observation.to.as_str().to_owned()));
    }
    let mut overrides = Vec::new();
    for observation in observations.iter_mut().filter(|observation| {
        observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
    }) {
        if let Some(name) = observation.to.as_str().strip_prefix("rust-call://")
            && let Some(Some(target)) = definitions.get(name)
        {
            overrides.push(DependencyOverride {
                from: observation.from.clone(),
                relation: DependencyRelation::Calls,
                unresolved_to: observation.to.clone(),
                resolved_to: EntityId::from(target.clone()),
                evidence: observation.evidence.clone(),
                confidence: Confidence::Inferred,
                provenance: Provenance::UniqueNameHeuristic,
            });
            observation.to = EntityId::from(target.clone());
            observation.confidence = Confidence::Inferred;
            observation.provenance = Provenance::UniqueNameHeuristic;
        }
    }
    overrides
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze, diagnostics_from_analysis, observations};
    use beholder_domain::{AnalysisDiagnostic, AnalysisDiagnosticSeverity};
    use std::path::Path;

    #[test]
    fn workspace_smoke() {
        let observations = observations(
            "beholder",
            "fn first() { second(); } fn second() {}",
            Path::new("src/lib.rs"),
        )
        .unwrap();
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://beholder/rust/lib/first"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && observation.to.as_str() == "repo://beholder/rust/lib/second"
                && observation.confidence == Confidence::Exact
                && observation.provenance == Provenance::Ast
        }));

        let mut ambiguous = vec![
            Observation::dependency(
                "repo://beholder/rust/caller",
                DependencyRelation::Calls,
                "rust-call://helper",
                "src/lib.rs:1",
            ),
            Observation::structural(
                "repo://beholder/rust/one",
                StructuralRelation::Defines,
                "repo://beholder/rust/one/helper",
                "src/one.rs:1",
            ),
            Observation::structural(
                "repo://beholder/rust/two",
                StructuralRelation::Defines,
                "repo://beholder/rust/two/helper",
                "src/two.rs:1",
            ),
        ];
        resolve_repository_calls(&mut ambiguous);
        assert_eq!(ambiguous[0].to.as_str(), "rust-call://helper");
        assert_eq!(ambiguous[0].confidence, Confidence::Exact);
        assert_eq!(ambiguous[0].provenance, Provenance::Ast);
    }

    #[test]
    fn marks_unique_name_resolution_as_inferred() {
        let mut observations = vec![
            Observation::dependency(
                "repo://beholder/rust/caller",
                DependencyRelation::Calls,
                "rust-call://helper",
                "src/caller.rs:1",
            ),
            Observation::structural(
                "repo://beholder/rust/helper",
                StructuralRelation::Defines,
                "repo://beholder/rust/helper/helper",
                "src/helper.rs:1",
            ),
        ];

        let overrides = resolve_repository_calls(&mut observations);

        assert_eq!(
            observations[0].to.as_str(),
            "repo://beholder/rust/helper/helper"
        );
        assert_eq!(observations[0].confidence, Confidence::Inferred);
        assert_eq!(observations[0].provenance, Provenance::UniqueNameHeuristic);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].confidence, Confidence::Inferred);
        assert_eq!(overrides[0].provenance, Provenance::UniqueNameHeuristic);
    }

    #[test]
    fn does_not_treat_sibling_module_names_as_exact() {
        let mut observations = observations(
            "beholder",
            "mod one { fn caller() { helper(); } } mod two { fn helper() {} }",
            Path::new("src/lib.rs"),
        )
        .unwrap();
        let call = observations
            .iter()
            .find(|observation| observation.from.as_str().ends_with("/one/caller"))
            .unwrap();
        assert_eq!(call.to.as_str(), "rust-call://helper");
        assert_eq!(call.confidence, Confidence::Exact);

        resolve_repository_calls(&mut observations);
        let call = observations
            .iter()
            .find(|observation| observation.from.as_str().ends_with("/one/caller"))
            .unwrap();
        assert!(call.to.as_str().ends_with("/two/helper"));
        assert_eq!(call.confidence, Confidence::Inferred);
        assert_eq!(call.provenance, Provenance::UniqueNameHeuristic);
    }

    #[test]
    fn qualifies_scoped_function_ids() {
        let observations = observations(
            "beholder",
            "mod nested { fn run() {} } struct One; struct Two; \
             impl One { fn run() {} } impl Two { fn run() {} }",
            Path::new("crates/example/src/lib.rs"),
        )
        .unwrap();
        let definitions = observations
            .iter()
            .filter(|observation| {
                observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
            })
            .map(|observation| observation.to.as_str())
            .collect::<Vec<_>>();

        assert!(definitions.contains(&"repo://beholder/rust/crates/example/src/lib/nested/run"));
        assert!(definitions.contains(&"repo://beholder/rust/crates/example/src/lib/impl/One/run"));
        assert!(definitions.contains(&"repo://beholder/rust/crates/example/src/lib/impl/Two/run"));
    }

    #[test]
    fn leaves_receiver_methods_unresolved() {
        let source = "fn is_valid_hash(s: &str) -> bool { \
                 s.chars().all(|c| c.is_ascii()) \
             } \
             struct InMemoryOutboxRepository; \
             impl InMemoryOutboxRepository { fn all(&self) {} }";
        let mut observations = observations(
            "repo-link",
            source,
            Path::new("crates/domain-task/src/hash.rs"),
        )
        .unwrap();

        resolve_repository_calls(&mut observations);
        let calls = observations
            .iter()
            .filter(|observation| {
                observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                    && observation.from.as_str().ends_with("/is_valid_hash")
            })
            .map(|observation| observation.to.as_str())
            .collect::<Vec<_>>();

        assert!(calls.contains(&"rust-method://all"));
        assert!(calls.contains(&"rust-method://chars"));
        assert!(calls.contains(&"rust-method://is_ascii"));
        assert!(
            observations
                .iter()
                .filter(|observation| { observation.to.as_str().starts_with("rust-method://") })
                .all(|observation| {
                    observation.confidence == Confidence::Exact
                        && observation.provenance == Provenance::Ast
                })
        );
        assert!(
            !calls
                .iter()
                .any(|target| { target.ends_with("/impl/InMemoryOutboxRepository/all") })
        );
        let diagnostics = diagnostics_from_analysis(
            &analyze(source).unwrap(),
            Path::new("crates/domain-task/src/hash.rs"),
        );
        assert_eq!(
            diagnostics,
            vec![AnalysisDiagnostic {
                code: "rust.receiver_method_resolution_unavailable".into(),
                severity: AnalysisDiagnosticSeverity::KnownLimitation,
                path: Path::new("crates/domain-task/src/hash.rs").into(),
                line: Some(1),
                detail: Some("3 receiver method calls are indexed without type resolution".into(),),
            }]
        );
    }
}
