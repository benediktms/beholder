use super::schema::*;
use beholder_dto::{
    AnalysisCompleteness, AnalysisDiagnostic, AnalysisDiagnosticSeverity, AnalysisMetadata,
    RepositoryRevision,
};
use mnestic_engine::{DataValue, DbInstance, MultiTransaction, NamedRows, ScriptMutability};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::PathBuf;

pub(super) trait QueryRunner {
    fn run_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
    ) -> Result<NamedRows, Box<dyn Error>>;
}

impl QueryRunner for DbInstance {
    fn run_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
    ) -> Result<NamedRows, Box<dyn Error>> {
        Ok(self.run_script(script, params, ScriptMutability::Immutable)?)
    }
}

impl QueryRunner for MultiTransaction {
    fn run_query(
        &self,
        script: &str,
        params: BTreeMap<String, DataValue>,
    ) -> Result<NamedRows, Box<dyn Error>> {
        Ok(self.run_script(script, params)?)
    }
}

pub(super) fn query(
    db: &impl QueryRunner,
    view: &str,
    script: &str,
    additions: impl IntoIterator<Item = (&'static str, DataValue)>,
) -> Result<NamedRows, Box<dyn Error>> {
    let mut params = BTreeMap::from([("view".into(), view.into())]);
    params.extend(
        additions
            .into_iter()
            .map(|(name, value)| (name.into(), value)),
    );
    db.run_query(script, params)
}

pub(super) fn inspect_relations(db: &DbInstance) -> Result<NamedRows, Box<dyn Error>> {
    Ok(db.run_script("::relations", BTreeMap::new(), ScriptMutability::Immutable)?)
}

pub(super) fn inspect_revisions(db: &DbInstance) -> Result<NamedRows, Box<dyn Error>> {
    Ok(db.run_script(
        "?[view, revision, fingerprint, repository, head, state] := \
             *analysis_revision{view, revision}, \
             *analysis_fingerprint{view, fingerprint}, \
             *analysis_revision_state{view, revision, repository, state}, \
             *repository_state{fingerprint: state, repository, head}\n\
         :order view, repository",
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )?)
}

pub(super) fn analysis_revision(db: &impl QueryRunner, view: &str) -> Result<u64, Box<dyn Error>> {
    let rows = query(
        db,
        view,
        "?[revision] := *analysis_revision{view: $view, revision}",
        [],
    )?;
    Ok(rows
        .rows
        .first()
        .and_then(|row| row[0].get_int())
        .unwrap_or_default()
        .try_into()?)
}

pub(super) fn analysis_metadata(
    db: &impl QueryRunner,
    view: &str,
    revision: u64,
) -> Result<AnalysisMetadata, Box<dyn Error>> {
    let rows = query(
        db,
        view,
        "?[incomplete] := *analysis_revision_metadata{\
             view: $view, revision: $revision, incomplete\
         }",
        [("revision", i64::try_from(revision)?.into())],
    )?;
    let incomplete = rows
        .rows
        .first()
        .and_then(|row| match row.first() {
            Some(DataValue::Bool(value)) => Some(*value),
            _ => None,
        })
        .unwrap_or_default();
    let rows = query(
        db,
        view,
        &format!(
            "{DIRECT_RULES}\n\
             baseline_diagnostic[repository, code, severity, path, line, detail] := \
                 *analysis_revision_diagnostic{{\
                     view: $view, revision: $revision, repository, code, severity, path, line, detail\
                 }}, \
                 not selected_diagnostic_replacement[repository, code]\n\
             ?[repository, code, severity, path, line, detail] := \
                 baseline_diagnostic[repository, code, severity, path, line, detail]\n\
             ?[repository, code, severity, path, line, detail] := \
                 enrichment_diagnostic[repository, code, severity, path, line, detail], \
                 not baseline_diagnostic[repository, code, severity, path, line, _]\n\
             :order severity, repository, path, line, code"
        ),
        [("revision", i64::try_from(revision)?.into())],
    )?;
    let diagnostics = rows
        .rows
        .into_iter()
        .map(|row| {
            let severity = match row[2].get_str() {
                Some("known_limitation") => AnalysisDiagnosticSeverity::KnownLimitation,
                Some("warning") => AnalysisDiagnosticSeverity::Warning,
                _ => return Err("unknown stored analysis diagnostic severity".into()),
            };
            let line = u32::try_from(row[4].get_int().unwrap_or_default())?;
            let detail = row[5].get_str().unwrap_or_default();
            Ok(AnalysisDiagnostic {
                repository: row[0].get_str().unwrap_or_default().into(),
                code: row[1].get_str().unwrap_or_default().into(),
                severity,
                path: PathBuf::from(row[3].get_str().unwrap_or_default()),
                line: (line != 0).then_some(line),
                detail: (!detail.is_empty()).then(|| detail.into()),
            })
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(AnalysisMetadata {
        completeness: if incomplete {
            AnalysisCompleteness::Incomplete
        } else {
            AnalysisCompleteness::Complete
        },
        diagnostics,
    })
}

pub(super) fn repository_revision(
    db: &impl QueryRunner,
    repository: &str,
) -> Result<Option<RepositoryRevision>, Box<dyn Error>> {
    let params = BTreeMap::from([("repository".into(), repository.into())]);
    let revision = db.run_query(
        "?[source_state, analysis_identity, head, incomplete] := \
             *repository_revision{repository: $repository, source_state, analysis_identity, head, incomplete}",
        params.clone(),
    )?;
    let Some(row) = revision.rows.first() else {
        return Ok(None);
    };
    let source_state = row[0]
        .get_str()
        .ok_or("stored repository source state is not a string")?
        .to_owned();
    let analysis_identity = row[1]
        .get_str()
        .ok_or("stored repository analysis identity is not a string")?
        .to_owned();
    let head = row[2]
        .get_str()
        .ok_or("stored repository head is not a string")?;
    let incomplete = match &row[3] {
        DataValue::Bool(value) => *value,
        _ => return Err("stored repository completeness is not a boolean".into()),
    };
    let diagnostics = db.run_query(
        "?[code, severity, path, line, detail] := \
             *repository_revision_diagnostic{repository: $repository, code, severity, path, line, detail}\n\
         :order severity, path, line, code",
        params,
    )?;
    let diagnostics = diagnostics
        .rows
        .into_iter()
        .map(|row| {
            let severity = match row[1].get_str() {
                Some("known_limitation") => AnalysisDiagnosticSeverity::KnownLimitation,
                Some("warning") => AnalysisDiagnosticSeverity::Warning,
                _ => return Err("unknown stored repository diagnostic severity".into()),
            };
            let line = u32::try_from(row[3].get_int().unwrap_or_default())?;
            let detail = row[4].get_str().unwrap_or_default();
            Ok(AnalysisDiagnostic {
                repository: repository.into(),
                code: row[0].get_str().unwrap_or_default().into(),
                severity,
                path: PathBuf::from(row[2].get_str().unwrap_or_default()),
                line: (line != 0).then_some(line),
                detail: (!detail.is_empty()).then(|| detail.into()),
            })
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(Some(RepositoryRevision {
        source_state,
        head: (!head.is_empty()).then(|| head.into()),
        analysis_identity,
        analysis: AnalysisMetadata {
            completeness: if incomplete {
                AnalysisCompleteness::Incomplete
            } else {
                AnalysisCompleteness::Complete
            },
            diagnostics,
        },
    }))
}

pub(super) fn published_repository_head(
    db: &impl QueryRunner,
    view: &str,
    repository: &str,
) -> Result<Option<String>, Box<dyn Error>> {
    let rows = query(
        db,
        view,
        "?[head] := \
             *analysis_revision{view: $view, revision}, \
             *analysis_revision_state{view: $view, revision, repository: $repository, state}, \
             *repository_state{fingerprint: state, repository: $repository, head}",
        [("repository", repository.into())],
    )?;
    Ok(rows
        .rows
        .first()
        .and_then(|row| row[0].get_str())
        .filter(|head| !head.is_empty())
        .map(str::to_owned))
}

pub(super) fn entity_facts(
    db: &impl QueryRunner,
    view: &str,
    entities: &BTreeSet<String>,
) -> Result<NamedRows, Box<dyn Error>> {
    let entities = DataValue::List(
        entities
            .iter()
            .map(|id| DataValue::List(vec![id.as_str().into()]))
            .collect(),
    );
    query(
        db,
        view,
        "requested[id] <- $entities\n\
         selected_state[state] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_state{view: $view, revision, state}\n\
         selected_enrichment[owner] := *analysis_revision{view: $view, revision}, \
             *analysis_revision_repository_enrichment{view: $view, revision, owner}\n\
         baseline_id[id] := requested[id], selected_state[state], *state_entity{state, id}\n\
         baseline_id[id] := requested[id], *analysis_revision{view: $view, revision}, \
             *analysis_revision_entity{view: $view, revision, id}\n\
         baseline_id[id] := requested[id], \
             *analysis_fact_shard_selection{view: $view, producer, owner, version}, \
             *analysis_fact_shard_entity{producer, owner, version, id}\n\
         ?[id, kind, metadata] := requested[id], selected_state[state], \
             *state_entity{state, id, kind, metadata}\n\
         ?[id, kind, metadata] := requested[id], *analysis_revision{view: $view, revision}, \
             *analysis_revision_entity{view: $view, revision, id, kind, metadata}\n\
         ?[id, kind, metadata] := requested[id], \
             *analysis_fact_shard_selection{view: $view, producer, owner, version}, \
             *analysis_fact_shard_entity{producer, owner, version, id, kind, metadata}\n\
         ?[id, kind, metadata] := requested[id], \
             *analysis_enrichment_entity_selection{view: $view, id, owner}, \
             selected_enrichment[owner], \
             *enrichment_entity_contribution{view: $view, owner, id, kind, metadata}, \
             not baseline_id[id]\n\
         :order id",
        [("entities", entities)],
    )
}

pub(super) fn inspect_observations(
    db: &DbInstance,
    relation: Option<&str>,
) -> Result<NamedRows, Box<dyn Error>> {
    let (filter, params) = match relation {
        Some(relation) => (
            ", relation == $relation",
            BTreeMap::from([("relation".into(), relation.into())]),
        ),
        None => ("", BTreeMap::new()),
    };
    Ok(db.run_script(
        &format!(
            "?[state, from, relation, to, evidence, confidence, provenance] := \
                 *state_observation{{state, from, relation, to, evidence}}, \
                 *state_observation_metadata{{state, from, relation, to, confidence, provenance}}{filter}\n\
             :order relation, from, to"
        ),
        params,
        ScriptMutability::Immutable,
    )?)
}

pub(super) fn inspect_grpc_bindings(db: &DbInstance) -> Result<NamedRows, Box<dyn Error>> {
    Ok(db.run_script(
        "selected[view, revision, state, local_symbol, role, service, method, cardinality, evidence, confidence, provenance] := \
             *analysis_revision{view, revision}, \
             *analysis_revision_state{view, revision, state}, \
             *state_grpc_binding_candidate{\
                 state, local_symbol, role, service, method, evidence, cardinality, confidence, provenance\
             }\n\
         diagnostic[view, revision, local_symbol, role, service, method, evidence, code, detail] := \
             *analysis_revision_grpc_diagnostic{\
                 view, revision, local_symbol, role, service, method, evidence, code, detail\
             }\n\
         ?[view, local_symbol, role, service, method, cardinality, evidence, confidence, provenance, status, code, detail] := \
             selected[view, revision, _, local_symbol, role, service, method, cardinality, evidence, confidence, provenance], \
             diagnostic[view, revision, local_symbol, role, service, method, evidence, code, detail], \
             status = 'unmatched'\n\
         ?[view, local_symbol, role, service, method, cardinality, evidence, confidence, provenance, status, code, detail] := \
             selected[view, revision, _, local_symbol, role, service, method, cardinality, evidence, confidence, provenance], \
             not diagnostic[view, revision, local_symbol, role, service, method, evidence, _, _], \
             status = 'resolved', code = '', detail = ''\n\
         :order view, service, method, role, local_symbol",
        BTreeMap::new(),
        ScriptMutability::Immutable,
    )?)
}

pub(super) fn context(
    db: &impl QueryRunner,
    view: &str,
    entity: &str,
) -> Result<NamedRows, Box<dyn Error>> {
    query(db, view, CONTEXT_QUERY, [("entity", entity.into())])
}

pub(super) fn trace(
    db: &impl QueryRunner,
    view: &str,
    from: &str,
    _to: &str,
) -> Result<NamedRows, Box<dyn Error>> {
    closure(db, view, from, DEPENDENCY_RULES)
}

pub(super) fn impact(
    db: &impl QueryRunner,
    view: &str,
    entity: &str,
) -> Result<NamedRows, Box<dyn Error>> {
    closure(db, view, entity, IMPACT_RULES)
}

pub(super) fn dependencies(
    db: &impl QueryRunner,
    view: &str,
    entity: &str,
) -> Result<NamedRows, Box<dyn Error>> {
    closure(db, view, entity, DEPENDENCY_RULES)
}

fn closure(
    db: &impl QueryRunner,
    view: &str,
    entity: &str,
    traversal_rules: &str,
) -> Result<NamedRows, Box<dyn Error>> {
    query(
        db,
        view,
        &format!(
            "{DIRECT_RULES}\n{traversal_rules}\n\
             start[] <- [[$from]]\n\
             ?[row_kind, entity, hops, edge_from, edge_to, relation, evidence, confidence, provenance] := \
                 selected_edge[\
                     edge_from, edge_to, relation, evidence, confidence, provenance\
                 ], \
                 row_kind = 'edge', entity = '', hops = 0\n\
             :order edge_from, edge_to, relation"
        ),
        [("from", entity.into())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::DIRECT_RULES;
    use crate::{SemanticStore, database::memory_database};
    use beholder_domain::{
        DependencyRelation, LogicalRepository, Observation, RepositoryFacts, RepositoryState,
        StructuralRelation, WorkspaceView,
    };
    use mnestic_engine::ScriptMutability;
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        time::SystemTime,
    };
    fn facts(view: &WorkspaceView, observations: Vec<Observation>) -> RepositoryFacts {
        RepositoryFacts {
            state: view.repository_states[0].clone(),
            analysis_identity: "analysis".into(),
            incomplete: false,
            diagnostics: Vec::new(),
            entities: Vec::new(),
            grpc_bindings: Vec::new(),
            observations,
        }
    }
    #[test]
    fn impact_traverses_dependants() {
        let store = SemanticStore::memory().unwrap();
        let result = store
            .impact(
                "main",
                "rpc/Pricing.GetPrice",
                beholder_dto::DEFAULT_MAX_HOPS,
            )
            .unwrap();
        assert!(
            result
                .affected
                .iter()
                .any(|value| value.entity == "web/CheckoutPage")
        );
        assert!(
            !result
                .affected
                .iter()
                .any(|value| value.entity == "pricing/get_price")
        );
        let limited = store.impact("main", "rpc/Pricing.GetPrice", 1).unwrap();
        assert!(limited.traversal.truncated);
        assert!(limited.affected.iter().all(|value| value.hops == 1));
    }

    #[test]
    fn trace_chooses_a_string_predecessor() {
        let db = memory_database().unwrap();
        db.run_script(
            "?[state, from, relation, to, evidence] <- [
                    ['diamond-state', 'start', 'calls', 'left', 'left:1'],
                    ['diamond-state', 'start', 'calls', 'right', 'right:1'],
                    ['diamond-state', 'left', 'calls', 'end', 'left:2'],
                    ['diamond-state', 'right', 'calls', 'end', 'right:2'],
                 ]
                 :put state_dependency_observation {state, from, relation, to => evidence}",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .unwrap();
        db.run_script(
            "?[state, from, relation, to, confidence, provenance] :=
                     *state_dependency_observation{state, from, relation, to},
                     confidence = 1.0, provenance = 'ast'
                 :put state_observation_metadata {
                     state, from, relation, to => confidence, provenance
                 }",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .unwrap();
        db.run_script(
            "?[view, revision] <- [['diamond', 0]]
                 :put analysis_revision {view => revision}",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .unwrap();
        db.run_script(
            "?[view, revision, repository, state] <- [
                    ['diamond', 0, 'diamond', 'diamond-state']
                 ]
                 :put analysis_revision_state {view, revision, repository => state}",
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .unwrap();

        let result = crate::semantic::trace(
            "diamond",
            "start",
            "end",
            beholder_dto::DEFAULT_MAX_HOPS,
            crate::inspection::inspection_result(trace(&db, "diamond", "start", "end").unwrap()),
            crate::inspection::InspectionResult {
                headers: Vec::new(),
                rows: Vec::new(),
                next: None,
            },
        )
        .unwrap();
        assert_eq!(result.paths[0].nodes, ["start", "left", "end"]);

        let limited = crate::semantic::trace(
            "diamond",
            "start",
            "end",
            1,
            crate::inspection::inspection_result(trace(&db, "diamond", "start", "end").unwrap()),
            crate::inspection::InspectionResult {
                headers: Vec::new(),
                rows: Vec::new(),
                next: None,
            },
        )
        .unwrap();
        assert!(limited.paths.is_empty());
        assert!(limited.traversal.truncated);
    }

    #[test]
    fn typed_trace_deduplicates_graph_and_resolves_path_references() {
        let result = SemanticStore::memory()
            .unwrap()
            .trace(
                "main",
                "web/CheckoutPage",
                "cache/update_price",
                beholder_dto::DEFAULT_MAX_HOPS,
            )
            .unwrap();
        let node_ids = result
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<BTreeSet<_>>();
        let edge_ids = result
            .edges
            .iter()
            .map(|edge| edge.id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(node_ids.len(), result.nodes.len());
        assert_eq!(edge_ids.len(), result.edges.len());
        assert_eq!(result.nodes[0].kind, beholder_dto::EntityKind::Callable);
        assert!(result.edges.iter().all(|edge| {
            edge.confidence == 1.0
                && edge
                    .evidence
                    .iter()
                    .all(|evidence| evidence.source_kind == beholder_dto::EvidenceKind::Ast)
        }));
        for path in &result.paths {
            assert!(path.nodes.iter().all(|id| node_ids.contains(id.as_str())));
            assert!(path.edges.iter().all(|id| edge_ids.contains(id.as_str())));
        }
    }

    #[test]
    fn workspace_smoke() {
        let store = SemanticStore::memory().unwrap();
        let feature = query(
            &store.db,
            "feature",
            &format!(
                "{DIRECT_RULES}\n?[provider] := direct[\
                        'rpc/Pricing.GetPrice', provider, 'implemented_by', _, _, _\
                     ]"
            ),
            [],
        )
        .unwrap();
        let feature = format!("{feature:?}");
        assert!(feature.contains("pricing/get_price_v2"));
        assert!(!feature.contains("pricing/get_price\""));

        let result = store.context("main", "rpc/Pricing.GetPrice").unwrap();
        assert_eq!(result.edges.len(), 2);
        assert!(
            result
                .nodes
                .iter()
                .any(|value| value.id == "pricing/get_price")
        );
    }

    #[test]
    fn structural_facts_are_context_only() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!("beholder-structural-{unique}"));
        fs::create_dir_all(&state_dir).unwrap();
        let store = SemanticStore::persistent(&state_dir.join("beholder.db"), true).unwrap();
        let view = WorkspaceView::new(
            "structural",
            "analysis",
            vec![RepositoryState {
                repository: LogicalRepository {
                    identity: "repo".into(),
                },
                head: Some("head".into()),
                fingerprint: "state".into(),
            }],
        )
        .unwrap();
        store
            .publish(
                &view,
                &[facts(
                    &view,
                    vec![
                        Observation::structural(
                            "repo/file",
                            StructuralRelation::Defines,
                            "repo/caller",
                            "src/lib.rs:1",
                        ),
                        Observation::dependency(
                            "repo/caller",
                            DependencyRelation::Calls,
                            "repo/target",
                            "src/lib.rs:2",
                        ),
                    ],
                )],
                &[],
            )
            .unwrap();

        assert_eq!(
            store
                .context("structural", "repo/file")
                .unwrap()
                .edges
                .len(),
            1
        );
        assert!(
            store
                .trace(
                    "structural",
                    "repo/file",
                    "repo/target",
                    beholder_dto::DEFAULT_MAX_HOPS,
                )
                .unwrap()
                .paths
                .is_empty()
        );
        assert_eq!(
            store
                .trace(
                    "structural",
                    "repo/caller",
                    "repo/target",
                    beholder_dto::DEFAULT_MAX_HOPS,
                )
                .unwrap()
                .paths
                .len(),
            1
        );
        drop(store);
        fs::remove_dir_all(state_dir).unwrap();
    }
}
