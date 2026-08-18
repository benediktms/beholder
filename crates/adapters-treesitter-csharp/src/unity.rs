use crate::{
    CsharpProject, CsharpSource, analysis::source_stem, model::DefinitionKind,
    project::assembly_visibility,
};
use beholder_domain::{DependencyRelation, EntityFact, EntityKind, Observation};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssemblyDefinition {
    name: String,
    #[serde(default)]
    references: Vec<String>,
    #[serde(default = "enabled")]
    auto_referenced: bool,
}

fn enabled() -> bool {
    true
}

fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    std::iter::repeat_n(Component::ParentDir, from.len() - common)
        .chain(to[common..].iter().copied())
        .collect()
}

pub fn parse_unity_assemblies(
    sources: &[(PathBuf, String)],
) -> Result<Vec<CsharpProject>, Box<dyn Error>> {
    let definitions = sources
        .iter()
        .map(|(path, source)| {
            let source = source.strip_prefix('\u{feff}').unwrap_or(source);
            Ok((path, serde_json::from_str::<AssemblyDefinition>(source)?))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let paths = definitions
        .iter()
        .map(|(path, definition)| (definition.name.as_str(), path.as_path()))
        .collect::<BTreeMap<_, _>>();
    let mut projects = definitions
        .iter()
        .map(|(path, definition)| {
            let directory = path.parent().unwrap_or(Path::new(""));
            let references = definition
                .references
                .iter()
                .filter_map(|reference| paths.get(reference.as_str()))
                .map(|reference| relative_path(directory, reference))
                .collect();
            CsharpProject {
                path: (*path).clone(),
                assembly: definition.name.clone(),
                references,
                includes: Vec::new(),
                removes: Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    let predefined_path = PathBuf::from("Assets/Assembly-CSharp.csproj");
    let predefined_directory = Path::new("Assets");
    projects.push(CsharpProject {
        path: predefined_path,
        assembly: "Assembly-CSharp".into(),
        references: definitions
            .iter()
            .filter(|(_, definition)| definition.auto_referenced)
            .map(|(path, _)| relative_path(predefined_directory, path))
            .collect(),
        includes: Vec::new(),
        removes: Vec::new(),
    });
    Ok(projects)
}

fn simple_type_name(name: &str) -> &str {
    name.rsplit(['.', '/']).next().unwrap_or(name)
}

pub fn unity_lifecycle(
    repository: &str,
    projects: &[CsharpProject],
    sources: &[CsharpSource<'_>],
) -> (Vec<EntityFact>, Vec<Observation>) {
    const MESSAGES: &[(&str, &[&str])] = &[
        ("Awake()", &["void"]),
        ("Start()", &["void", "IEnumerator"]),
        ("FixedUpdate()", &["void"]),
        ("Update()", &["void"]),
        ("LateUpdate()", &["void"]),
        ("OnEnable()", &["void"]),
        ("OnDisable()", &["void"]),
        ("OnDestroy()", &["void"]),
        ("OnGUI()", &["void"]),
        ("OnValidate()", &["void"]),
        ("OnDrawGizmos()", &["void"]),
        ("OnDrawGizmosSelected()", &["void"]),
    ];
    let visibility = assembly_visibility(projects);
    let mut behaviour_types = BTreeMap::<&str, BTreeSet<String>>::new();
    for assembly in sources
        .iter()
        .map(|source| source.assembly)
        .collect::<BTreeSet<_>>()
    {
        let visible = visibility
            .get(assembly)
            .cloned()
            .unwrap_or_else(|| BTreeSet::from([assembly.into()]));
        let direct = sources
            .iter()
            .filter(|candidate| visible.contains(candidate.assembly))
            .flat_map(|candidate| &candidate.analysis.definitions)
            .filter(|definition| definition.kind == DefinitionKind::Type)
            .map(|definition| {
                (
                    simple_type_name(&definition.qualified_name).to_owned(),
                    definition
                        .base_types
                        .iter()
                        .map(|base| simple_type_name(base).to_owned())
                        .collect::<BTreeSet<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut behaviours = BTreeSet::from(["MonoBehaviour".into()]);
        loop {
            let previous = behaviours.len();
            behaviours.extend(
                direct
                    .iter()
                    .filter(|(_, bases)| bases.iter().any(|base| behaviours.contains(base)))
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>(),
            );
            if behaviours.len() == previous {
                break;
            }
        }
        behaviour_types.insert(assembly, behaviours);
    }
    let callbacks = sources
        .iter()
        .flat_map(|source| {
            let behaviours = &behaviour_types[source.assembly];
            source
                .analysis
                .definitions
                .iter()
                .filter_map(move |definition| {
                    let (parent, method) = definition.qualified_name.rsplit_once('/')?;
                    let return_type = definition.return_type.as_deref()?.rsplit('.').next()?;
                    let message = MESSAGES.iter().find(|(name, returns)| {
                        method == *name && returns.contains(&return_type)
                    })?;
                    (definition.kind == DefinitionKind::Callable
                        && !definition.is_static
                        && behaviours.contains(simple_type_name(parent)))
                    .then_some((message.0, source, definition))
                })
        })
        .collect::<Vec<_>>();
    let observations = callbacks
        .iter()
        .map(|(message, source, definition)| {
            let module = format!(
                "repo://{repository}/csharp/{}/{}",
                source.assembly,
                source_stem(source.path)
            );
            Observation::dependency(
                format!("unity://UnityEngine.MonoBehaviour/{message}"),
                DependencyRelation::ImplementedBy,
                format!("{module}/{}", definition.qualified_name),
                format!("{}:{}", source.path.display(), definition.line),
            )
        })
        .collect::<Vec<_>>();
    let entities = callbacks
        .iter()
        .map(|(message, _, _)| message)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|message| {
            EntityFact::new(
                format!("unity://UnityEngine.MonoBehaviour/{message}"),
                EntityKind::Callable,
                None,
            )
            .unwrap()
        })
        .collect();
    (entities, observations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{assembly_visibility, source_assemblies};
    use std::collections::BTreeSet;

    #[test]
    fn maps_unity_sources_and_assembly_references() {
        let projects = parse_unity_assemblies(&[
            (
                PathBuf::from("Assets/Scripts/Core/Raven.Core.asmdef"),
                "\u{feff}{\"name\":\"Raven.Core\",\"references\":[],\"autoReferenced\":false}"
                    .into(),
            ),
            (
                PathBuf::from("Assets/Scripts/Definitions/Raven.Definitions.asmdef"),
                r#"{"name":"Raven.Definitions","references":["Raven.Core"],"autoReferenced":false}"#.into(),
            ),
        ])
        .unwrap();

        assert_eq!(
            source_assemblies(&projects, Path::new("Assets/Scripts/Core/DefinitionId.cs")),
            ["Raven.Core"]
        );
        assert_eq!(
            source_assemblies(&projects, Path::new("Assets/Scripts/Player.cs")),
            ["Assembly-CSharp"]
        );
        assert_eq!(
            assembly_visibility(&projects)["Raven.Definitions"],
            BTreeSet::from(["Raven.Core".into(), "Raven.Definitions".into()])
        );
        assert_eq!(
            assembly_visibility(&projects)["Raven.Core"],
            BTreeSet::from(["Raven.Core".into()])
        );
    }

    #[test]
    fn links_only_exact_mono_behaviour_lifecycle_callbacks() {
        let direct = crate::analyze(
            "using System.Collections; \
             class BaseView : UnityEngine.MonoBehaviour {} \
             class Player : UnityEngine.MonoBehaviour { \
                void Awake() {} IEnumerator Start() { yield break; } void FixedUpdate() {} \
                void Update() {} void LateUpdate() {} void OnEnable() {} void OnDisable() {} \
                void OnDestroy() {} \
             } \
             class StaticPlayer : MonoBehaviour { static void Update() {} } \
             class Utility { void Update() {} }",
        )
        .unwrap();
        let inherited = crate::analyze(
            "class InventoryView : BaseView { \
                void OnGUI() {} void OnValidate() {} \
                void OnDrawGizmos() {} void OnDrawGizmosSelected() {} \
             }",
        )
        .unwrap();
        let sources = [
            CsharpSource {
                path: Path::new("Assets/Player.cs"),
                assembly: "Assembly-CSharp",
                analysis: &direct,
            },
            CsharpSource {
                path: Path::new("Assets/InventoryView.cs"),
                assembly: "Assembly-CSharp",
                analysis: &inherited,
            },
        ];
        let (entities, observations) = unity_lifecycle("example/game", &[], &sources);

        assert_eq!(entities.len(), 12);
        assert_eq!(observations.len(), 12);
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "unity://UnityEngine.MonoBehaviour/Start()"
                && observation.to.as_str().ends_with("/Player/Start()")
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "unity://UnityEngine.MonoBehaviour/Update()"
                && observation.to.as_str().ends_with("/Player/Update()")
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "unity://UnityEngine.MonoBehaviour/OnDrawGizmosSelected()"
                && observation
                    .to
                    .as_str()
                    .ends_with("/InventoryView/OnDrawGizmosSelected()")
        }));
    }
}
