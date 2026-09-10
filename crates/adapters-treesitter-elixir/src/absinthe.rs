use super::{analysis::expand_alias, model::ElixirAnalysis};
use beholder_domain::{CallableClauseRole, Evidence, EvidenceContext, EvidencePayload};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphqlResolverBinding {
    pub field: String,
    pub parent: Option<String>,
    pub resolver: String,
    pub evidence: String,
}

fn camel_case(name: &str) -> String {
    let mut parts = name.split('_');
    let mut result = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        let mut characters = part.chars();
        if let Some(first) = characters.next() {
            result.extend(first.to_uppercase());
            result.extend(characters);
        }
    }
    result
}

pub fn bindings(
    repository: &str,
    sources: &[(&Path, &ElixirAnalysis)],
) -> Vec<GraphqlResolverBinding> {
    let imported_parents = sources
        .iter()
        .flat_map(|(_, analysis)| &analysis.modules)
        .flat_map(|module| &module.absinthe_field_imports)
        .fold(BTreeMap::<_, BTreeSet<_>>::new(), |mut imports, import| {
            imports
                .entry(import.imported.as_str())
                .or_default()
                .insert(import.parent.as_str());
            imports
        });
    let mut bindings = Vec::new();
    for (path, analysis) in sources {
        for schema_module in &analysis.modules {
            for resolver in &schema_module.absinthe_resolvers {
                let module = expand_alias(
                    &resolver.module,
                    &schema_module.aliases,
                    &schema_module.name,
                );
                let parents = imported_parents
                    .get(resolver.owner.as_str())
                    .map(|parents| parents.iter().copied().map(Some).collect::<Vec<_>>())
                    .unwrap_or_else(|| vec![resolver.parent.as_deref()]);
                let mut evidences = schema_module
                    .functions
                    .iter()
                    .find(|function| {
                        function.name == resolver.function
                            && function.arity == resolver.arity
                            && function.line == resolver.line
                    })
                    .into_iter()
                    .flat_map(|function| &function.definition_contexts)
                    .filter_map(|context| match context {
                        context @ EvidenceContext::CallableClause {
                            role: CallableClauseRole::Declaration,
                            definition_range,
                            ..
                        } => Some(
                            Evidence::structured(EvidencePayload {
                                path: Some(path.display().to_string()),
                                line: Some(definition_range.start.line.saturating_add(1)),
                                detail: None,
                                range: Some(definition_range.clone()),
                                contexts: vec![context.clone()],
                            })
                            .expect("tree-sitter emits valid Absinthe resolver ranges")
                            .as_str()
                            .to_owned(),
                        ),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if evidences.is_empty() {
                    evidences.push(format!("{}:{}", path.display(), resolver.line));
                }
                for parent in parents {
                    for evidence in &evidences {
                        bindings.push(GraphqlResolverBinding {
                            field: resolver
                                .public_field
                                .clone()
                                .unwrap_or_else(|| camel_case(&resolver.field)),
                            parent: parent.map(str::to_owned),
                            resolver: format!(
                                "repo://{repository}/elixir/{module}/{}/{}",
                                resolver.function, resolver.arity
                            ),
                            evidence: evidence.clone(),
                        });
                    }
                }
            }
        }
    }
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze;

    #[test]
    fn maps_absinthe_field_to_captured_resolver() {
        let path = Path::new("lib/schema/mutations.ex");
        let analysis = analyze(
            r#"
            defmodule CheckoutGql.Schema.Mutations do
              field :initialize_order, :initialize_order_payload do
                resolve(&CheckoutGql.Resolvers.InitializeOrderResolver.run/3)
              end
            end
            "#,
        )
        .unwrap();
        assert_eq!(
            bindings("checkout", &[(path, &analysis)]),
            [GraphqlResolverBinding {
                field: "initializeOrder".into(),
                parent: None,
                resolver:
                    "repo://checkout/elixir/CheckoutGql.Resolvers.InitializeOrderResolver/run/3"
                        .into(),
                evidence: "lib/schema/mutations.ex:4".into(),
            }]
        );
    }

    #[test]
    fn maps_absinthe_field_to_inline_resolver_callable() {
        let path = Path::new("lib/schema.ex");
        let analysis = analyze(
            r#"
            defmodule CustomerConnect.Schema do
              alias CustomerConnect.Subscriptions

              subscription do
                field :typing_subscription, :typing_payload do
                  resolve(fn payload, args, resolution ->
                    Subscriptions.resolve_typing(payload, args, resolution)
                  end)
                end
              end
            end
            "#,
        )
        .unwrap();
        let bindings = bindings("customer-connect", &[(path, &analysis)]);
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].field, "typingSubscription");
        assert_eq!(bindings[0].parent.as_deref(), Some("Subscription"));
        assert_eq!(
            bindings[0].resolver,
            "repo://customer-connect/elixir/CustomerConnect.Schema/__absinthe_subscription_typing_subscription_resolver/3"
        );
        let evidence = Evidence::from(bindings[0].evidence.as_str()).decode();
        assert_eq!(evidence.path.as_deref(), Some("lib/schema.ex"));
        assert_eq!(evidence.line, Some(7));
        assert!(matches!(
            evidence.contexts.as_slice(),
            [EvidenceContext::CallableClause {
                role: CallableClauseRole::Declaration,
                signature,
                definition_range,
                ..
            }] if signature.text == "payload, args, resolution"
                && evidence.range.as_ref() == Some(definition_range)
        ));

        let observations =
            crate::observations_from_analysis("customer-connect", &analysis, "", path);
        assert!(observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://customer-connect/elixir/CustomerConnect.Schema/__absinthe_subscription_typing_subscription_resolver/3"
                && observation.relation
                    == beholder_domain::SemanticRelation::Dependency(
                        beholder_domain::DependencyRelation::Calls,
                    )
                && observation.to.as_str()
                    == "elixir-call://CustomerConnect.Subscriptions/resolve_typing/3"
        }));
    }

    #[test]
    fn maps_keyword_and_multi_clause_inline_resolver() {
        let path = Path::new("lib/types/payment_method.ex");
        let analysis = analyze(
            r#"
            defmodule PaymentMethod.Schema do
              object :payment_method, name: "PaymentInstrument" do
                field(:metadata, :json,
                  resolve: fn
                    %{metadata: nil}, _ -> Metadata.empty()
                    parent, context -> Metadata.load(parent, context)
                  end
                )
              end
            end
            "#,
        )
        .unwrap();

        let bindings = bindings("payments", &[(path, &analysis)]);
        assert_eq!(bindings.len(), 2);
        for binding in &bindings {
            assert_eq!(binding.field, "metadata");
            assert_eq!(binding.parent.as_deref(), Some("PaymentInstrument"));
            assert_eq!(
                binding.resolver,
                "repo://payments/elixir/PaymentMethod.Schema/__absinthe_payment_method_metadata_resolver/2"
            );
        }
        for (binding, (line, signature)) in bindings
            .iter()
            .zip([(6, "%{metadata: nil}, _"), (7, "parent, context")])
        {
            let evidence = Evidence::from(binding.evidence.as_str()).decode();
            assert_eq!(
                evidence.path.as_deref(),
                Some("lib/types/payment_method.ex")
            );
            assert_eq!(evidence.line, Some(line));
            assert!(matches!(
                evidence.contexts.as_slice(),
                [EvidenceContext::CallableClause {
                    role: CallableClauseRole::Declaration,
                    signature: actual,
                    definition_range,
                    ..
                }] if actual.text == signature && evidence.range.as_ref() == Some(definition_range)
            ));
        }
        let observations = crate::observations_from_analysis("payments", &analysis, "", path);
        let targets = observations
            .iter()
            .filter(|observation| {
                observation.from.as_str()
                    == "repo://payments/elixir/PaymentMethod.Schema/__absinthe_payment_method_metadata_resolver/2"
            })
            .map(|observation| observation.to.as_str())
            .collect::<Vec<_>>();
        assert!(targets.contains(&"elixir-call://Metadata/empty/0"));
        assert!(targets.contains(&"elixir-call://Metadata/load/2"));
    }

    #[test]
    fn maps_local_capture_and_resolver_factory() {
        let path = Path::new("lib/schema.ex");
        let analysis = analyze(
            r#"
            defmodule People.Schema do
              query do
                field :person, :person do
                  resolve lookup(:person)
                end
                field :people, list_of(:person), name: "allPeople", resolve: &list_people/2
              end

              def lookup(:person), do: fn args, _ -> People.find(args.id) end
              def list_people(args, _), do: People.list(args)
            end
            "#,
        )
        .unwrap();

        assert_eq!(
            bindings("people", &[(path, &analysis)]),
            [
                GraphqlResolverBinding {
                    field: "person".into(),
                    parent: Some("Query".into()),
                    resolver: "repo://people/elixir/People.Schema/lookup/1".into(),
                    evidence: "lib/schema.ex:5".into(),
                },
                GraphqlResolverBinding {
                    field: "allPeople".into(),
                    parent: Some("Query".into()),
                    resolver: "repo://people/elixir/People.Schema/list_people/2".into(),
                    evidence: "lib/schema.ex:7".into(),
                },
            ]
        );
    }

    #[test]
    fn maps_imported_field_objects_to_their_root_type() {
        let queries_path = Path::new("lib/schema/queries.ex");
        let queries = analyze(
            r#"
            defmodule Checkout.Schema.Queries do
              object :queries do
                field :order, :order do
                  resolve(&OrderResolver.run/3)
                end
              end
            end
            "#,
        )
        .unwrap();
        let schema_path = Path::new("lib/schema.ex");
        let schema = analyze(
            r#"
            defmodule Checkout.Schema do
              query do
                import_fields(:queries)
              end
              subscription do
                field :order, :order do
                  resolve(&SubscriptionResolver.run/3)
                end
              end
            end
            "#,
        )
        .unwrap();

        let bindings = bindings(
            "checkout",
            &[(queries_path, &queries), (schema_path, &schema)],
        );

        assert!(bindings.iter().any(|binding| {
            binding.field == "order"
                && binding.parent.as_deref() == Some("Query")
                && binding.resolver == "repo://checkout/elixir/OrderResolver/run/3"
        }));
        assert!(bindings.iter().any(|binding| {
            binding.field == "order"
                && binding.parent.as_deref() == Some("Subscription")
                && binding.resolver == "repo://checkout/elixir/SubscriptionResolver/run/3"
        }));
    }
}
