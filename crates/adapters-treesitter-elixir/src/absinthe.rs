use super::{analysis::expand_alias, model::ElixirAnalysis};
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphqlResolverBinding {
    pub field: String,
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
    sources
        .iter()
        .flat_map(|(path, analysis)| {
            analysis.modules.iter().flat_map(move |schema_module| {
                schema_module
                    .absinthe_resolvers
                    .iter()
                    .map(move |resolver| {
                        let module = expand_alias(
                            &resolver.module,
                            &schema_module.aliases,
                            &schema_module.name,
                        );
                        GraphqlResolverBinding {
                            field: camel_case(&resolver.field),
                            resolver: format!(
                                "repo://{repository}/elixir/{module}/{}/{}",
                                resolver.function, resolver.arity
                            ),
                            evidence: format!("{}:{}", path.display(), resolver.line),
                        }
                    })
            })
        })
        .collect()
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
                resolver:
                    "repo://checkout/elixir/CheckoutGql.Resolvers.InitializeOrderResolver/run/3"
                        .into(),
                evidence: "lib/schema/mutations.ex:4".into(),
            }]
        );
    }
}
