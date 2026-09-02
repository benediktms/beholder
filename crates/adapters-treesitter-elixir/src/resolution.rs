use super::analysis::{call_observations, module_target};
use super::model::*;
use crate::analyze;
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, Confidence, DependencyOverride,
    DependencyRelation, EntityFact, EntityId, EntityKind, Observation, Provenance,
    SemanticRelation, StructuralRelation,
};
use std::collections::{BTreeMap, BTreeSet};
use std::{error::Error, path::Path};

pub fn observations_from_analysis(
    repository: &str,
    analysis: &ElixirAnalysis,
    source: &str,
    path: &Path,
) -> Vec<Observation> {
    let source_id = format!(
        "repo://{repository}/elixir-source/{}",
        path.to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/")
    );
    let definitions = analysis
        .modules
        .iter()
        .flat_map(|module| {
            let module_id = format!("repo://{repository}/elixir/{}", module.name);
            module
                .functions
                .iter()
                .map(move |function| format!("{module_id}/{}/{}", function.name, function.arity))
        })
        .collect::<BTreeSet<_>>();
    let mut observations = Vec::new();
    for module in &analysis.modules {
        let module_id = format!("repo://{repository}/elixir/{}", module.name);
        observations.push(Observation::structural(
            source_id.clone(),
            StructuralRelation::Defines,
            module_id.clone(),
            format!("{}:{}", path.display(), module.line),
        ));
        if let Some(enclosing_module) = &module.enclosing_module {
            observations.push(Observation::structural(
                format!("repo://{repository}/elixir/{enclosing_module}"),
                StructuralRelation::Defines,
                module_id.clone(),
                format!("{}:{}", path.display(), module.line),
            ));
        }
        observations.extend(module.functions.iter().map(|function| {
            Observation::structural(
                module_id.clone(),
                StructuralRelation::Defines,
                format!("{module_id}/{}/{}", function.name, function.arity),
                format!("{}:{}", path.display(), function.line),
            )
        }));
        observations.extend(module.callbacks.iter().map(|callback| {
            Observation::structural(
                module_id.clone(),
                StructuralRelation::Defines,
                format!("{module_id}/callback/{}/{}", callback.name, callback.arity),
                format!("{}:{}", path.display(), callback.line),
            )
        }));
        observations.extend(module.struct_fields.iter().map(|field| {
            Observation::structural(
                format!("{module_id}/field/{}", field.name),
                StructuralRelation::FieldOf,
                module_id.clone(),
                format!("{}:{}", path.display(), field.line),
            )
        }));
        observations.extend(call_observations(
            repository,
            &module.name,
            &module.functions,
            &definitions,
            path,
            false,
        ));
        for function in &module.functions {
            let function_id = format!("{module_id}/{}/{}", function.name, function.arity);
            let mut targets = BTreeSet::new();
            for r#use in &function.struct_uses {
                let target = r#use.module.clone();
                if targets.insert(target.clone()) {
                    observations.push(Observation::dependency(
                        function_id.clone(),
                        DependencyRelation::Uses,
                        module_target(&target),
                        format!("{}:{}", path.display(), r#use.line),
                    ));
                }
            }
        }
        observations.extend(module.implements.iter().map(|implementation| {
            Observation::dependency(
                module_id.clone(),
                DependencyRelation::Implements,
                module_target(&implementation.name),
                format!("{}:{}", path.display(), implementation.line),
            )
        }));
        observations.extend(module.references.iter().map(|reference| {
            Observation::dependency(
                module_id.clone(),
                match reference.kind {
                    ElixirModuleReferenceKind::Behaviour => DependencyRelation::Implements,
                    ElixirModuleReferenceKind::Import => DependencyRelation::Imports,
                    ElixirModuleReferenceKind::Require => DependencyRelation::Requires,
                    ElixirModuleReferenceKind::Use => DependencyRelation::Uses,
                },
                module_target(&reference.name),
                format!("{}:{}", path.display(), reference.line),
            )
        }));
    }
    if is_generated_source(path, source) {
        for observation in &mut observations {
            observation.provenance = Provenance::Generated;
        }
    }
    observations
}

pub fn entities_from_analysis(
    repository: &str,
    analysis: &ElixirAnalysis,
    path: &Path,
) -> Vec<EntityFact> {
    let source_id = format!(
        "repo://{repository}/elixir-source/{}",
        path.to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/")
    );
    let mut entities = vec![EntityFact::new(source_id, EntityKind::Namespace, None).unwrap()];
    for module in &analysis.modules {
        let module_id = format!("repo://{repository}/elixir/{}", module.name);
        entities.push(EntityFact::new(module_id.clone(), EntityKind::Namespace, None).unwrap());
        for function in &module.functions {
            entities.push(
                EntityFact::new(
                    format!("{module_id}/{}/{}", function.name, function.arity),
                    EntityKind::Callable,
                    None,
                )
                .unwrap(),
            );
        }
        for callback in &module.callbacks {
            entities.push(
                EntityFact::new(
                    format!("{module_id}/callback/{}/{}", callback.name, callback.arity),
                    EntityKind::Callable,
                    None,
                )
                .unwrap(),
            );
        }
    }
    entities
}

fn is_generated_source(path: &Path, source: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".pb.ex"))
        || source
            .lines()
            .take(20)
            .any(|line| line.contains("Autogenerated by"))
}

pub fn diagnostics_from_analysis(
    analysis: &ElixirAnalysis,
    path: &Path,
) -> Vec<AnalysisDiagnostic> {
    analysis
        .parse_error_lines
        .iter()
        .map(|line| AnalysisDiagnostic {
            code: "elixir.parse_recovery".into(),
            severity: AnalysisDiagnosticSeverity::Warning,
            path: path.into(),
            line: u32::try_from(*line).ok(),
            detail: Some("tree-sitter discarded an invalid Elixir syntax unit".into()),
        })
        .chain(analysis.modules.iter().flat_map(|module| {
            module
                .references
                .iter()
                .filter(|reference| reference.kind == ElixirModuleReferenceKind::Use)
                .map(|reference| AnalysisDiagnostic {
                    code: "elixir.macro_expansion_incomplete".into(),
                    severity: AnalysisDiagnosticSeverity::KnownLimitation,
                    path: path.into(),
                    line: u32::try_from(reference.line).ok(),
                    detail: Some(format!(
                        "use {} is indexed without compiler macro expansion",
                        reference.name
                    )),
                })
        }))
        .collect()
}

pub fn generated_observations(
    repository: &str,
    sources: &[(&Path, &ElixirAnalysis)],
    observations: &[Observation],
) -> Vec<Observation> {
    let mut macros = BTreeMap::<&str, Option<(&Path, &ElixirModule)>>::new();
    for (path, analysis) in sources {
        for module in &analysis.modules {
            if module.using_functions.is_empty() && module.using_implements.is_empty() {
                continue;
            }
            macros
                .entry(&module.name)
                .and_modify(|candidate| *candidate = None)
                .or_insert(Some((path, module)));
        }
    }

    let mut definition_edges = observations
        .iter()
        .filter(|observation| {
            observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
        })
        .map(|observation| {
            (
                observation.from.as_str().to_owned(),
                observation.to.as_str().to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut definitions = definition_edges
        .iter()
        .map(|(_, definition)| definition.clone())
        .collect::<BTreeSet<_>>();
    let mut generated = Vec::new();
    for (_, analysis) in sources {
        for module in &analysis.modules {
            let module_id = format!("repo://{repository}/elixir/{}", module.name);
            for reference in module
                .references
                .iter()
                .filter(|reference| reference.kind == ElixirModuleReferenceKind::Use)
            {
                let target = reference.name.clone();
                let Some(Some((path, macro_module))) = macros.get(target.as_str()) else {
                    continue;
                };
                let mut emitted_functions = Vec::new();
                for function in &macro_module.using_functions {
                    let function_id = format!("{module_id}/{}/{}", function.name, function.arity);
                    if definition_edges.insert((module_id.clone(), function_id.clone())) {
                        definitions.insert(function_id.clone());
                        emitted_functions.push(function.clone());
                        generated.push(Observation::generated(
                            module_id.clone(),
                            StructuralRelation::Defines,
                            function_id,
                            format!("{}:{}", path.display(), function.line),
                        ));
                    }
                }
                generated.extend(call_observations(
                    repository,
                    &module.name,
                    &emitted_functions,
                    &definitions,
                    path,
                    true,
                ));
                generated.extend(macro_module.using_implements.iter().map(|implementation| {
                    let mut observation = Observation::dependency(
                        module_id.clone(),
                        DependencyRelation::Implements,
                        module_target(&implementation.name),
                        format!("{}:{}", path.display(), implementation.line),
                    );
                    observation.provenance = Provenance::Generated;
                    observation
                }));
            }
        }
    }
    generated
}

pub fn generated_entities(observations: &[Observation]) -> Vec<EntityFact> {
    observations
        .iter()
        .filter(|observation| observation.provenance == Provenance::Generated)
        .filter(|observation| {
            observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
        })
        .map(|observation| {
            EntityFact::new(observation.to.clone(), EntityKind::Callable, None).unwrap()
        })
        .collect()
}

fn workspace_module_definitions(
    observations: &[Observation],
) -> BTreeMap<String, Option<EntityId>> {
    let mut definitions = BTreeMap::<String, Option<EntityId>>::new();
    for observation in observations.iter().filter(|observation| {
        observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
    }) {
        let Some(name) = observation
            .to
            .as_str()
            .rsplit_once("/elixir/")
            .map(|(_, name)| name)
            .filter(|name| !name.contains('/'))
        else {
            continue;
        };
        definitions
            .entry(name.into())
            .and_modify(|candidate| {
                if candidate.as_ref().map(EntityId::as_str) != Some(observation.to.as_str()) {
                    *candidate = None;
                }
            })
            .or_insert_with(|| Some(observation.to.clone()));
    }
    definitions
}

pub fn resolve_workspace_modules(observations: &[Observation]) -> Vec<DependencyOverride> {
    let definitions = workspace_module_definitions(observations);
    observations
        .iter()
        .filter_map(|observation| {
            let relation = match observation.relation {
                SemanticRelation::Dependency(relation @ DependencyRelation::Implements)
                | SemanticRelation::Dependency(relation @ DependencyRelation::Imports)
                | SemanticRelation::Dependency(relation @ DependencyRelation::Requires)
                | SemanticRelation::Dependency(relation @ DependencyRelation::Uses) => relation,
                _ => return None,
            };
            let name = observation.to.as_str().strip_prefix("elixir-module://")?;
            let target = definitions.get(name)?.as_ref()?;
            Some(DependencyOverride {
                from: observation.from.clone(),
                relation,
                unresolved_to: observation.to.clone(),
                resolved_to: target.clone(),
                evidence: observation.evidence.clone(),
                confidence: Confidence::Exact,
                provenance: Provenance::Ast,
            })
        })
        .collect()
}

fn dynamic_dispatch_candidates(observations: &[Observation]) -> Vec<(&Observation, EntityId)> {
    let behaviour_owners = workspace_module_definitions(observations);
    let mut callback_owners = BTreeMap::<String, Option<EntityId>>::new();
    for observation in observations.iter().filter(|observation| {
        observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
    }) {
        let Some(signature) = observation
            .to
            .as_str()
            .strip_prefix(&format!("{}/callback/", observation.from))
        else {
            continue;
        };
        callback_owners
            .entry(signature.to_owned())
            .and_modify(|candidate| {
                if candidate.as_ref() != Some(&observation.from) {
                    *candidate = None;
                }
            })
            .or_insert_with(|| Some(observation.from.clone()));
    }
    let definitions = observations
        .iter()
        .filter(|observation| {
            observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
        })
        .map(|observation| observation.to.as_str())
        .collect::<BTreeSet<_>>();
    let implementations = observations.iter().filter(|observation| {
        observation.relation == SemanticRelation::Dependency(DependencyRelation::Implements)
    });
    let dynamic_calls = observations
        .iter()
        .filter(|observation| {
            observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
        })
        .filter_map(|observation| {
            observation
                .to
                .as_str()
                .strip_prefix("elixir-dynamic-call://")
                .map(|signature| (observation, signature))
        });
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    let implementations = implementations.collect::<Vec<_>>();
    for (call, signature) in dynamic_calls {
        let Some(Some(callback_owner)) = callback_owners.get(signature) else {
            continue;
        };
        for implementation in &implementations {
            let Some(behaviour) = implementation
                .to
                .as_str()
                .strip_prefix("elixir-module://")
                .and_then(|name| behaviour_owners.get(name))
                .and_then(Option::as_ref)
            else {
                continue;
            };
            if behaviour != callback_owner {
                continue;
            }
            let target = format!("{}/{signature}", implementation.from);
            let target = EntityId::from(target);
            if definitions.contains(target.as_str())
                && seen.insert((call.from.clone(), target.clone()))
            {
                candidates.push((call, target));
            }
        }
    }
    candidates
}

pub fn workspace_dynamic_dispatch_observations(observations: &[Observation]) -> Vec<Observation> {
    dynamic_dispatch_candidates(observations)
        .into_iter()
        .map(|(call, target)| {
            let mut observation = Observation::dependency(
                call.from.clone(),
                DependencyRelation::Calls,
                target,
                call.evidence.clone(),
            );
            observation.confidence = Confidence::Inferred;
            observation.provenance = call.provenance;
            observation
        })
        .collect()
}

pub fn resolve_repository_calls(
    observations: &mut [Observation],
    sources: &[(&Path, &ElixirAnalysis)],
) {
    let definitions = observations
        .iter()
        .filter(|observation| {
            observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
        })
        .filter_map(|observation| {
            let symbol = observation.to.as_str().rsplit_once("/elixir/")?.1;
            symbol
                .contains('/')
                .then(|| (symbol.to_owned(), observation.to.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let imports = sources
        .iter()
        .flat_map(|(_, analysis)| &analysis.modules)
        .flat_map(|module| {
            module.functions.iter().map(|function| {
                (
                    format!("{}/{}/{}", module.name, function.name, function.arity),
                    &function.imports,
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    for observation in observations.iter_mut().filter(|observation| {
        observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
    }) {
        let Some(symbol) = observation.to.as_str().strip_prefix("elixir-call://") else {
            continue;
        };
        let target = if symbol.matches('/').count() >= 2 {
            definitions.get(symbol)
        } else {
            let Some((function, arity)) = symbol.rsplit_once('/') else {
                continue;
            };
            let Some(scoped_function) = observation
                .from
                .as_str()
                .rsplit_once("/elixir/")
                .map(|(_, function)| function)
            else {
                continue;
            };
            let caller = scoped_function;
            let Some((scoped_function, _)) = scoped_function.rsplit_once('/') else {
                continue;
            };
            let Some((module, _)) = scoped_function.rsplit_once('/') else {
                continue;
            };
            let signature = format!("{function}/{arity}");
            if let Some(target) = definitions.get(&format!("{module}/{signature}")) {
                observation.to = target.clone();
                continue;
            }
            let candidates = imports
                .get(caller)
                .copied()
                .into_iter()
                .flatten()
                .filter(|import| {
                    import
                        .only
                        .as_ref()
                        .is_none_or(|only| only.contains(&signature))
                        && !import.except.contains(&signature)
                })
                .collect::<Vec<_>>();
            let [import] = candidates.as_slice() else {
                continue;
            };
            let imported = format!("{}/{signature}", import.name);
            if let Some(target) = definitions.get(&imported) {
                observation.to = target.clone();
            } else {
                observation.to = EntityId::from(format!("elixir-call://{imported}"));
            }
            continue;
        };
        if let Some(target) = target {
            observation.to = target.clone();
        }
    }
}

pub fn observations(
    repository: &str,
    source: &str,
    path: &Path,
) -> Result<Vec<Observation>, Box<dyn Error>> {
    let analysis = analyze(source).map_err(|error| -> Box<dyn Error> { error })?;
    let mut observations = observations_from_analysis(repository, &analysis, source, path);
    observations.extend(generated_observations(
        repository,
        &[(path, &analysis)],
        &observations,
    ));
    observations.extend(super::grpc::configured_delegate_observations(
        repository,
        &[(path, &analysis)],
        &observations,
    ));
    resolve_repository_calls(&mut observations, &[(path, &analysis)]);
    Ok(observations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use beholder_domain::SemanticRelation;

    #[test]
    fn emits_typed_source_entities() {
        let analysis = analyze(
            "defmodule Example.Worker do\n  @callback work(term()) :: term()\n  def run, do: :ok\nend",
        )
        .unwrap();
        let entities = entities_from_analysis("example", &analysis, Path::new("lib/worker.ex"));
        assert!(entities.iter().any(|entity| {
            entity.id.as_str() == "repo://example/elixir/Example.Worker/run/0"
                && entity.kind == EntityKind::Callable
        }));
        assert!(entities.iter().any(|entity| {
            entity.id.as_str() == "repo://example/elixir/Example.Worker/callback/work/1"
                && entity.kind == EntityKind::Callable
        }));
    }

    #[test]
    fn resolves_struct_dispatch_and_captured_source_callbacks_from_behaviour_evidence() {
        let mut observations = observations(
            "example",
            r#"
            defmodule Example.Job do
              @callback load(term(), struct()) :: term()
            end

            defmodule Example.Client do
              def fetch(keys), do: keys
            end

            defmodule Example.Source do
              def new, do: Dataloader.KV.new(&fetch/2)
              defp fetch(batch, keys), do: Example.Client.fetch({batch, keys})
            end

            defmodule Example.Worker do
              @behaviour Example.Job
              defstruct [:id]
              @impl true
              def load(context, _job), do: {context, Example.Source.new()}
            end

            defmodule Example.Impostor do
              def load(context, job), do: {context, job}
            end

            defmodule Example.Dispatcher do
              def dispatch(context, job), do: job.__struct__.load(context, job)
            end
            "#,
            Path::new("lib/example.ex"),
        )
        .unwrap();
        let dynamic = workspace_dynamic_dispatch_observations(&observations);
        observations.extend(dynamic);
        let edges = observations
            .iter()
            .filter(|observation| {
                observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
            })
            .map(|observation| (observation.from.as_str(), observation.to.as_str()))
            .collect::<BTreeSet<_>>();

        assert!(edges.contains(&(
            "repo://example/elixir/Example.Dispatcher/dispatch/2",
            "repo://example/elixir/Example.Worker/load/2",
        )));
        assert!(!edges.contains(&(
            "repo://example/elixir/Example.Dispatcher/dispatch/2",
            "repo://example/elixir/Example.Impostor/load/2",
        )));
        assert!(edges.contains(&(
            "repo://example/elixir/Example.Source/new/0",
            "repo://example/elixir/Example.Source/fetch/2",
        )));
        assert!(edges.contains(&(
            "repo://example/elixir/Example.Source/fetch/2",
            "repo://example/elixir/Example.Client/fetch/1",
        )));
    }

    #[test]
    fn preserves_captures_from_every_function_clause() {
        let observations = observations(
            "example",
            r#"
            defmodule Example.Source do
              def new(:first), do: Dataloader.KV.new(&fetch_first/2)
              def new(:second), do: Dataloader.KV.new(&fetch_second/2)
              def aliased do
                callback = &fetch_aliased/2
                Dataloader.KV.new(callback)
              end
              defp fetch_first(batch, keys), do: {batch, keys}
              defp fetch_second(batch, keys), do: {batch, keys}
              defp fetch_aliased(batch, keys), do: {batch, keys}
            end
            "#,
            Path::new("lib/source.ex"),
        )
        .unwrap();
        let targets = observations
            .iter()
            .filter(|observation| {
                observation.from.as_str() == "repo://example/elixir/Example.Source/new/1"
                    && observation.relation
                        == SemanticRelation::Dependency(DependencyRelation::Calls)
            })
            .map(|observation| observation.to.as_str())
            .collect::<BTreeSet<_>>();

        assert!(targets.contains("repo://example/elixir/Example.Source/fetch_first/2"));
        assert!(targets.contains("repo://example/elixir/Example.Source/fetch_second/2"));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://example/elixir/Example.Source/aliased/0"
                && observation.to.as_str() == "repo://example/elixir/Example.Source/fetch_aliased/2"
        }));
    }

    #[test]
    fn prefers_exact_calls_over_earlier_capture_evidence() {
        let observations = observations(
            "example",
            "defmodule Example.Source do\n  def run(items, item) do\n    Enum.map(items, &fetch/1)\n    fetch(item)\n  end\n  defp fetch(item), do: item\nend",
            Path::new("lib/source.ex"),
        )
        .unwrap();
        let call = observations
            .iter()
            .find(|observation| {
                observation.from.as_str() == "repo://example/elixir/Example.Source/run/2"
                    && observation.to.as_str() == "repo://example/elixir/Example.Source/fetch/1"
            })
            .unwrap();

        assert_eq!(call.confidence, Confidence::Exact);
        assert_eq!(call.evidence.as_str(), "lib/source.ex:4");
    }

    #[test]
    fn resolves_dynamic_dispatch_against_a_workspace_behaviour() {
        let mut workspace_observations = observations(
            "contracts",
            "defmodule Example.Job do\n  @callback load(term(), struct()) :: term()\nend",
            Path::new("lib/job.ex"),
        )
        .unwrap();
        workspace_observations.extend(
            observations(
                "app",
                r#"
                defmodule Example.Worker do
                  @behaviour Example.Job
                  defstruct []
                  @impl true
                  def load(context, _job), do: context
                end

                defmodule Example.OtherWorker do
                  @behaviour Example.Job
                  defstruct []
                  @impl true
                  def load(context, _job), do: context
                end

                defmodule Example.Dispatcher do
                  def dispatch(context, job), do: job.__struct__.load(context, job)
                end
                "#,
                Path::new("lib/generated.pb.ex"),
            )
            .unwrap(),
        );
        assert!(!workspace_observations.iter().any(|observation| {
            observation.from.as_str() == "repo://app/elixir/Example.Dispatcher/dispatch/2"
                && observation.to.as_str().starts_with("repo://")
                && observation.to.as_str().ends_with("/load/2")
        }));

        let resolved = workspace_dynamic_dispatch_observations(&workspace_observations);
        let targets = resolved
            .iter()
            .filter(|observation| {
                observation.from.as_str() == "repo://app/elixir/Example.Dispatcher/dispatch/2"
                    && observation.confidence == Confidence::Inferred
                    && observation.provenance == Provenance::Generated
            })
            .map(|observation| observation.to.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(
            targets,
            BTreeSet::from([
                "repo://app/elixir/Example.OtherWorker/load/2",
                "repo://app/elixir/Example.Worker/load/2",
            ])
        );
    }

    #[test]
    fn skips_dynamic_dispatch_for_an_ambiguous_workspace_behaviour() {
        let behaviour =
            "defmodule Example.Job do\n  @callback load(term(), struct()) :: term()\nend";
        let mut workspace_observations =
            observations("contracts-a", behaviour, Path::new("lib/job.ex")).unwrap();
        workspace_observations.extend(
            observations(
                "contracts-b",
                "defmodule Example.Job do\nend",
                Path::new("lib/job.ex"),
            )
            .unwrap(),
        );
        workspace_observations.extend(
            observations(
                "app",
                r#"
                defmodule Example.Worker do
                  @behaviour Example.Job
                  defstruct []
                  @impl true
                  def load(context, _job), do: context
                end

                defmodule Example.Dispatcher do
                  def dispatch(context, job), do: job.__struct__.load(context, job)
                end
                "#,
                Path::new("lib/worker.ex"),
            )
            .unwrap(),
        );

        let resolved = workspace_dynamic_dispatch_observations(&workspace_observations);

        assert!(!resolved.iter().any(|observation| {
            observation.from.as_str() == "repo://app/elixir/Example.Dispatcher/dispatch/2"
        }));
    }

    #[test]
    fn skips_dynamic_dispatch_for_an_ambiguous_callback_signature() {
        let mut workspace_observations = observations(
            "contracts",
            r#"
            defmodule Example.Job do
              @callback load(term(), struct()) :: term()
            end

            defmodule Example.Serializer do
              @callback load(term(), struct()) :: term()
            end
            "#,
            Path::new("lib/contracts.ex"),
        )
        .unwrap();
        workspace_observations.extend(
            observations(
                "app",
                r#"
                defmodule Example.Worker do
                  @behaviour Example.Job
                  defstruct []
                  @impl true
                  def load(context, _job), do: context
                end

                defmodule Example.Encoder do
                  @behaviour Example.Serializer
                  defstruct []
                  @impl true
                  def load(context, _serializer), do: context
                end

                defmodule Example.Dispatcher do
                  def dispatch(context, job), do: job.__struct__.load(context, job)
                end
                "#,
                Path::new("lib/worker.ex"),
            )
            .unwrap(),
        );

        let resolved = workspace_dynamic_dispatch_observations(&workspace_observations);

        assert!(!resolved.iter().any(|observation| {
            observation.from.as_str() == "repo://app/elixir/Example.Dispatcher/dispatch/2"
        }));
    }

    #[test]
    fn emits_stable_module_and_function_definitions() {
        let observations = observations(
            "payments",
            r#"
            defmodule MyApp.Payments do
              def create_payment(account, amount), do: {:ok, account, amount}
              defp normalize(value \\ nil), do: value
              defdelegate lookup(id, opts \\ []), to: Backend
              def create_payment(account, amount) when amount > 0, do: {:ok, account, amount}
            end
            "#,
            Path::new("lib/my_app/payments.ex"),
        )
        .unwrap();

        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir-source/lib/my_app/payments.ex"
                && observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
                && observation.to.as_str() == "repo://payments/elixir/MyApp.Payments"
                && observation.evidence.as_str() == "lib/my_app/payments.ex:2"
        }));
        let functions = observations
            .iter()
            .filter(|observation| {
                observation.from.as_str() == "repo://payments/elixir/MyApp.Payments"
            })
            .map(|observation| observation.to.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            functions,
            vec![
                "repo://payments/elixir/MyApp.Payments/create_payment/2",
                "repo://payments/elixir/MyApp.Payments/normalize/0",
                "repo://payments/elixir/MyApp.Payments/normalize/1",
                "repo://payments/elixir/MyApp.Payments/lookup/1",
                "repo://payments/elixir/MyApp.Payments/lookup/2",
            ]
        );
    }

    #[test]
    fn classifies_only_explicitly_generated_sources() {
        let source = "defmodule Example.Message do\n  defstruct [:id]\nend";
        let protobuf = observations("example", source, Path::new("lib/example.pb.ex")).unwrap();
        assert!(
            protobuf
                .iter()
                .all(|observation| observation.provenance == Provenance::Generated)
        );

        let client = observations(
            "example",
            "defmodule Example.Client do\n  @moduledoc \"Autogenerated by `example-generator`\"\n  def call, do: :ok\nend",
            Path::new("lib/example_client.ex"),
        )
        .unwrap();
        assert!(
            client
                .iter()
                .all(|observation| observation.provenance == Provenance::Generated)
        );

        let source =
            observations("example", source, Path::new("lib/generated/example.ex")).unwrap();
        assert!(
            source
                .iter()
                .all(|observation| observation.provenance == Provenance::Ast)
        );
    }

    #[test]
    fn resolves_local_and_aliased_calls_without_emitting_control_macros() {
        let observations = observations(
            "payments",
            r#"
            defmodule MyApp.Payments do
              def before_alias, do: Late.run()
              alias MyApp.{Ledger, Unused}
              alias MyApp.Late, as: Late
              def after_alias, do: Late.run()
              import MyApp.Helpers, only: [audit: 0]
              require MyApp.Macros, as: Macros

              def create(amount) do
                amount |> normalize()
                Ledger.record(amount)
                if amount > 0 do
                  audit()
                  hidden()
                  Macros.expand(amount)
                end
              end

              def create(:fallback), do: fallback()
              defp normalize(amount), do: amount
              defp fallback, do: :ok
              defdelegate delegate(amount), to: Ledger, as: :record
            end

            defmodule MyApp.Helpers do
              def audit, do: :ok
              def hidden, do: :ok
            end

            defmodule MyApp.Late do
              def run, do: :ok
            end
            "#,
            Path::new("lib/my_app/payments.ex"),
        )
        .unwrap();

        let caller = "repo://payments/elixir/MyApp.Payments/create/1";
        let calls = observations
            .iter()
            .filter(|observation| {
                observation.from.as_str() == caller
                    && observation.relation
                        == SemanticRelation::Dependency(DependencyRelation::Calls)
            })
            .map(|observation| observation.to.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            vec![
                "repo://payments/elixir/MyApp.Payments/normalize/1",
                "elixir-call://MyApp.Ledger/record/1",
                "repo://payments/elixir/MyApp.Helpers/audit/0",
                "elixir-call://hidden/0",
                "elixir-call://MyApp.Macros/expand/1",
                "repo://payments/elixir/MyApp.Payments/fallback/0",
            ]
        );
        assert!(!calls.iter().any(|call| call.contains("/if/")));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Payments/delegate/1"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && observation.to.as_str() == "elixir-call://MyApp.Ledger/record/1"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Payments/before_alias/0"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && observation.to.as_str() == "elixir-call://Late/run/0"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Payments/after_alias/0"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && observation.to.as_str() == "repo://payments/elixir/MyApp.Late/run/0"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Payments"
                && observation.relation
                    == SemanticRelation::Dependency(DependencyRelation::Requires)
                && observation.to.as_str() == "elixir-module://MyApp.Macros"
        }));
    }

    #[test]
    fn resolves_function_scoped_imports() {
        let observations = observations(
            "payments",
            r#"
            defmodule MyApp.Consumer do
              def query do
                import Ecto.Query, only: [from: 2]
                from(item in Item, where: item.active)
              end

              def audit do
                import MyApp.Helpers, only: [record: 0]
                record()
              end
            end

            defmodule MyApp.Helpers do
              def record, do: :ok
            end
            "#,
            Path::new("lib/my_app/consumer.ex"),
        )
        .unwrap();

        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer/query/0"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && observation.to.as_str() == "elixir-call://Ecto.Query/from/2"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer/audit/0"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && observation.to.as_str() == "repo://payments/elixir/MyApp.Helpers/record/0"
        }));
    }

    #[test]
    fn resolves_repository_calls_and_generated_function_calls() {
        let macro_analysis = analyze(
            r#"
            defmodule MyApp.ServerMacro do
              alias MyApp.Backend, as: Backend
              defmacro __using__(_) do
                quote do
                  def generated(value), do: Backend.work(value)
                end
              end
            end
            "#,
        )
        .unwrap();
        let consumer_analysis = analyze(
            r#"
            defmodule MyApp.Consumer do
              use MyApp.ServerMacro
            end

            defmodule MyApp.Backend do
              def work(value), do: value
            end
            "#,
        )
        .unwrap();
        let macro_path = Path::new("lib/my_app/server_macro.ex");
        let consumer_path = Path::new("lib/my_app/consumer.ex");
        let mut observations =
            observations_from_analysis("payments", &macro_analysis, "", macro_path);
        observations.extend(observations_from_analysis(
            "payments",
            &consumer_analysis,
            "",
            consumer_path,
        ));
        observations.extend(generated_observations(
            "payments",
            &[
                (macro_path, &macro_analysis),
                (consumer_path, &consumer_analysis),
            ],
            &observations,
        ));
        resolve_repository_calls(
            &mut observations,
            &[
                (macro_path, &macro_analysis),
                (consumer_path, &consumer_analysis),
            ],
        );

        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer/generated/1"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
                && observation.to.as_str() == "repo://payments/elixir/MyApp.Backend/work/1"
                && observation.evidence.as_str() == "lib/my_app/server_macro.ex:6"
                && observation.provenance == Provenance::Generated
        }));
    }

    #[test]
    fn models_literal_using_definitions_as_generated() {
        let observations = observations(
            "payments",
            r#"
            defmodule MyApp.Macro do
              defmacro __using__(_) do
                quote do
                  @behaviour MyApp.Worker
                  def generated, do: :ok
                end
              end
            end
            defmodule MyApp do
              defmodule Consumer do
                use MyApp.Macro, mode: :strict
                import External.Helpers, only: [help: 1]
                require External.Macros, as: Macros
                def own, do: :ok
              end
            end

            defmodule MyApp.Worker do
              @callback work() :: :ok
            end
            "#,
            Path::new("lib/my_app/consumer.ex"),
        )
        .unwrap();
        let entities = generated_entities(&observations);

        assert!(
            !observations
                .iter()
                .any(|observation| observation.to.as_str().ends_with("/__using__/1"))
        );
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer"
                && observation.relation == SemanticRelation::Structural(StructuralRelation::Defines)
                && observation.to.as_str() == "repo://payments/elixir/MyApp.Consumer/generated/0"
                && observation.evidence.as_str() == "lib/my_app/consumer.ex:6"
                && observation.provenance == Provenance::Generated
        }));
        assert!(entities.iter().any(|entity| {
            entity.id.as_str() == "repo://payments/elixir/MyApp.Consumer/generated/0"
                && entity.kind == EntityKind::Callable
        }));
        assert!(!observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Macro"
                && observation.to.as_str().ends_with("/generated/0")
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer"
                && observation.relation
                    == SemanticRelation::Dependency(DependencyRelation::Implements)
                && observation.to.as_str() == "elixir-module://MyApp.Worker"
                && observation.provenance == Provenance::Generated
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Uses)
                && observation.to.as_str() == "elixir-module://MyApp.Macro"
                && observation.evidence.as_str() == "lib/my_app/consumer.ex:12"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer"
                && observation.relation == SemanticRelation::Dependency(DependencyRelation::Imports)
                && observation.to.as_str() == "elixir-module://External.Helpers"
                && observation.evidence.as_str() == "lib/my_app/consumer.ex:13"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://payments/elixir/MyApp.Consumer"
                && observation.relation
                    == SemanticRelation::Dependency(DependencyRelation::Requires)
                && observation.to.as_str() == "elixir-module://External.Macros"
                && observation.evidence.as_str() == "lib/my_app/consumer.ex:14"
        }));

        let overrides = resolve_workspace_modules(&observations);
        assert_eq!(overrides.len(), 2);
        assert!(overrides.iter().any(|override_| {
            override_.relation == DependencyRelation::Uses
                && override_.resolved_to.as_str() == "repo://payments/elixir/MyApp.Macro"
        }));
        assert!(overrides.iter().any(|override_| {
            override_.relation == DependencyRelation::Implements
                && override_.resolved_to.as_str() == "repo://payments/elixir/MyApp.Worker"
        }));
    }

    #[test]
    fn models_compiler_defined_behaviours_protocols_and_structs() {
        let observations = observations(
            "github.com/example/elixir",
            r#"
            defmodule Example.Worker do
              @callback run(term()) :: term()
            end

            defmodule Example.Data do
              @behaviour Example.Worker
              @behaviour :gen_server
              defstruct [:name, active: true]

              alias Example.Data, as: Data
              def build(name), do: %Data{name: name}
            end

            defprotocol Example.Printable do
              def print(value)
            end

            defimpl Example.Printable, for: Example.Data do
              def print(value), do: value.name
            end
            "#,
            Path::new("lib/example.ex"),
        )
        .unwrap();
        let triples = observations
            .iter()
            .map(|observation| {
                (
                    observation.from.as_str(),
                    observation.relation.as_str(),
                    observation.to.as_str(),
                )
            })
            .collect::<BTreeSet<_>>();

        assert!(triples.contains(&(
            "repo://github.com/example/elixir/elixir/Example.Worker",
            "defines",
            "repo://github.com/example/elixir/elixir/Example.Worker/callback/run/1",
        )));
        assert!(triples.contains(&(
            "repo://github.com/example/elixir/elixir/Example.Data",
            "implements",
            "elixir-module://Example.Worker",
        )));
        assert!(triples.contains(&(
            "repo://github.com/example/elixir/elixir/Example.Data",
            "implements",
            "erlang-module://gen_server",
        )));
        assert!(triples.contains(&(
            "repo://github.com/example/elixir/elixir/Example.Data/field/name",
            "field_of",
            "repo://github.com/example/elixir/elixir/Example.Data",
        )));
        assert!(triples.contains(&(
            "repo://github.com/example/elixir/elixir/Example.Data/build/1",
            "uses",
            "elixir-module://Example.Data",
        )));
        assert!(triples.contains(&(
            "repo://github.com/example/elixir/elixir/Example.Printable",
            "defines",
            "repo://github.com/example/elixir/elixir/Example.Printable/print/1",
        )));
        assert!(
            triples.contains(&(
                "repo://github.com/example/elixir/elixir/Example.Printable.Example.Data",
                "implements",
                "elixir-module://Example.Printable",
            )),
            "{triples:#?}"
        );
        let overrides = resolve_workspace_modules(&observations);
        assert!(overrides.iter().any(|override_| {
            override_.unresolved_to.as_str() == "elixir-module://Example.Printable"
                && override_.resolved_to.as_str()
                    == "repo://github.com/example/elixir/elixir/Example.Printable"
                && override_.relation == DependencyRelation::Implements
        }));
    }

    #[test]
    fn models_struct_patterns_and_nested_defimpl_context() {
        let observations = observations(
            "github.com/example/elixir",
            r#"defprotocol Example.Printable do
  def print(value)
end

defmodule Example.Context do
  def inspect(%Example.First{}), do: :first
  def inspect(%Example.Second{}), do: :second

  def classify(value) do
    case value do
      %Example.Third{} -> :third
    end

    Enum.map([], fn %Example.Fourth{} -> :fourth end)
  end

  defimpl Example.Printable, for: Example.First do
    def print(value), do: value
  end
end
"#,
            Path::new("lib/context.ex"),
        )
        .unwrap();
        let context = "repo://github.com/example/elixir/elixir/Example.Context";
        let inspect = format!("{context}/inspect/1");
        let classify = format!("{context}/classify/1");
        let implementation =
            "repo://github.com/example/elixir/elixir/Example.Printable.Example.First";
        let expected = [
            (
                inspect.as_str(),
                "uses",
                "elixir-module://Example.First",
                "lib/context.ex:6",
            ),
            (
                inspect.as_str(),
                "uses",
                "elixir-module://Example.Second",
                "lib/context.ex:7",
            ),
            (
                classify.as_str(),
                "uses",
                "elixir-module://Example.Third",
                "lib/context.ex:11",
            ),
            (
                classify.as_str(),
                "uses",
                "elixir-module://Example.Fourth",
                "lib/context.ex:14",
            ),
            (context, "defines", implementation, "lib/context.ex:17"),
            (
                implementation,
                "implements",
                "elixir-module://Example.Printable",
                "lib/context.ex:17",
            ),
        ];

        for (from, relation, to, evidence) in expected {
            assert_eq!(
                observations
                    .iter()
                    .filter(|observation| {
                        observation.from.as_str() == from
                            && observation.relation.as_str() == relation
                            && observation.to.as_str() == to
                            && observation.evidence.as_str() == evidence
                    })
                    .count(),
                1,
                "missing or duplicated {from} {relation} {to} at {evidence}"
            );
        }
    }

    #[test]
    fn reports_macro_expansion_as_a_known_limitation() {
        let analysis = analyze(
            r#"
            defmodule MyApp.Consumer do
              alias MyApp.ServerMacro, as: Server
              use Server, mode: :strict
            end
            "#,
        )
        .unwrap();

        assert_eq!(
            diagnostics_from_analysis(&analysis, Path::new("lib/my_app/consumer.ex")),
            vec![AnalysisDiagnostic {
                code: "elixir.macro_expansion_incomplete".into(),
                severity: AnalysisDiagnosticSeverity::KnownLimitation,
                path: std::path::PathBuf::from("lib/my_app/consumer.ex"),
                line: Some(4),
                detail: Some(
                    "use MyApp.ServerMacro is indexed without compiler macro expansion".into(),
                ),
            }]
        );
    }
}
