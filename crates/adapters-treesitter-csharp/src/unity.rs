use crate::{
    CsharpProject, CsharpSource, analysis::source_stem, model::DefinitionKind,
    project::assembly_visibility,
};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, DependencyRelation, EntityFact, EntityKind,
    Observation,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::{self, BufRead},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnityPrefab {
    pub path: PathBuf,
    pub scripts: Vec<UnityScriptReference>,
    pub source_prefabs: Vec<UnityReference>,
    pub fingerprint: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnityReference {
    pub guid: String,
    pub line: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnityScriptReference {
    pub guid: String,
    pub class_identifier: Option<String>,
    pub line: u32,
}

fn guid(line: &str) -> Option<&str> {
    line.split_once("guid:")?
        .1
        .trim_start()
        .split([',', '}', ' '])
        .find(|part| !part.is_empty())
}

pub fn parse_unity_meta(reader: impl BufRead) -> io::Result<Option<(String, Vec<u8>)>> {
    for line in reader.lines() {
        let line = line?;
        if let Some(guid) = line
            .trim_start_matches('\u{feff}')
            .trim()
            .strip_prefix("guid:")
        {
            let guid = guid.trim().to_owned();
            return Ok(Some((guid.clone(), format!("guid:{guid}\n").into_bytes())));
        }
    }
    Ok(None)
}

pub fn parse_unity_prefab(path: &Path, reader: impl BufRead) -> io::Result<UnityPrefab> {
    let mut scripts = Vec::new();
    let mut source_prefabs = Vec::new();
    let mut pending_script = None;
    let mut fingerprint = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let trimmed = line.trim_start_matches('\u{feff}').trim();
        if trimmed.starts_with("---") {
            if let Some(script) = pending_script.take() {
                scripts.push(script);
            }
        } else if trimmed.starts_with("m_Script:") {
            if let Some(script) = pending_script.take() {
                scripts.push(script);
            }
            if let Some(guid) = guid(trimmed) {
                fingerprint.extend_from_slice(format!("script:{line_number}:{guid}\n").as_bytes());
                pending_script = Some(UnityScriptReference {
                    guid: guid.into(),
                    class_identifier: None,
                    line: line_number,
                });
            }
        } else if let Some(identifier) = trimmed.strip_prefix("m_EditorClassIdentifier:") {
            let identifier = identifier.trim();
            if let Some(script) = pending_script.as_mut()
                && !identifier.is_empty()
            {
                script.class_identifier = Some(identifier.into());
                fingerprint.extend_from_slice(format!("class:{identifier}\n").as_bytes());
            }
        } else if trimmed.starts_with("m_SourcePrefab:")
            && let Some(guid) = guid(trimmed)
        {
            fingerprint.extend_from_slice(format!("prefab:{line_number}:{guid}\n").as_bytes());
            source_prefabs.push(UnityReference {
                guid: guid.into(),
                line: line_number,
            });
        }
    }
    if let Some(script) = pending_script {
        scripts.push(script);
    }
    Ok(UnityPrefab {
        path: path.into(),
        scripts,
        source_prefabs,
        fingerprint,
    })
}

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
) -> Result<Vec<CsharpProject>, Box<dyn Error + Send + Sync>> {
    let definitions = sources
        .iter()
        .map(|(path, source)| {
            let source = source.strip_prefix('\u{feff}').unwrap_or(source);
            Ok((path, serde_json::from_str::<AssemblyDefinition>(source)?))
        })
        .collect::<Result<Vec<_>, Box<dyn Error + Send + Sync>>>()?;
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

fn prefab_id(repository: &str, path: &Path) -> String {
    format!("repo://{repository}/unity-prefab/{}", path.display())
}

fn type_id(repository: &str, source: &CsharpSource<'_>, qualified_name: &str) -> String {
    format!(
        "repo://{repository}/csharp/{}/{}/{}",
        source.assembly,
        source_stem(source.path),
        qualified_name
    )
}

pub fn unity_prefab_dependencies(
    repository: &str,
    prefabs: &[UnityPrefab],
    script_paths: &BTreeMap<String, PathBuf>,
    prefab_paths: &BTreeMap<String, PathBuf>,
    sources: &[CsharpSource<'_>],
) -> (Vec<EntityFact>, Vec<Observation>, Vec<AnalysisDiagnostic>) {
    let mut entities: Vec<EntityFact> = prefabs
        .iter()
        .map(|prefab| {
            EntityFact::new(
                prefab_id(repository, &prefab.path),
                EntityKind::UnityPrefab,
                None,
            )
            .unwrap()
        })
        .collect();
    let mut observations = Vec::new();
    let mut diagnostics = Vec::new();
    let mut reported_script_guids = BTreeSet::new();
    for prefab in prefabs {
        let from = prefab_id(repository, &prefab.path);
        for script in &prefab.scripts {
            let Some(path) = script_paths.get(&script.guid) else {
                if let Some(identifier) = script.class_identifier.as_deref()
                    && let Some((assembly, name)) = identifier.split_once("::")
                {
                    let target = format!("unity://{assembly}/{}", name.replace('.', "/"));
                    entities.push(
                        EntityFact::new(target.clone(), EntityKind::Namespace, None).unwrap(),
                    );
                    observations.push(Observation::dependency(
                        from.clone(),
                        DependencyRelation::Uses,
                        target,
                        format!("{}:{}", prefab.path.display(), script.line),
                    ));
                } else if reported_script_guids.insert(script.guid.clone()) {
                    diagnostics.push(AnalysisDiagnostic {
                        code: "unity.prefab_script_unresolved".into(),
                        severity: AnalysisDiagnosticSeverity::Warning,
                        path: prefab.path.clone(),
                        line: Some(script.line),
                        detail: Some(format!(
                            "script GUID {} has no indexed metadata",
                            script.guid
                        )),
                    });
                }
                continue;
            };
            let expected = script
                .class_identifier
                .as_deref()
                .and_then(|identifier| identifier.split_once("::").map(|(_, name)| name))
                .map(|name| name.replace('.', "/"));
            let candidates = sources
                .iter()
                .filter(|source| source.path == path)
                .flat_map(|source| {
                    source
                        .analysis
                        .definitions
                        .iter()
                        .filter(|definition| definition.kind == DefinitionKind::Type)
                        .filter(|definition| {
                            expected.as_ref().map_or_else(
                                || {
                                    path.file_stem().and_then(|name| name.to_str()).is_some_and(
                                        |name| simple_type_name(&definition.qualified_name) == name,
                                    )
                                },
                                |expected| definition.qualified_name == *expected,
                            )
                        })
                        .map(move |definition| {
                            type_id(repository, source, &definition.qualified_name)
                        })
                })
                .collect::<BTreeSet<_>>();
            if candidates.len() == 1 {
                observations.push(Observation::dependency(
                    from.clone(),
                    DependencyRelation::Uses,
                    candidates.into_iter().next().unwrap(),
                    format!("{}:{}", prefab.path.display(), script.line),
                ));
            } else if reported_script_guids.insert(script.guid.clone()) {
                diagnostics.push(AnalysisDiagnostic {
                    code: "unity.prefab_script_unresolved".into(),
                    severity: AnalysisDiagnosticSeverity::Warning,
                    path: prefab.path.clone(),
                    line: Some(script.line),
                    detail: Some(format!(
                        "script GUID {} resolved to {} indexed C# types",
                        script.guid,
                        candidates.len()
                    )),
                });
            }
        }
        for source_prefab in &prefab.source_prefabs {
            if let Some(path) = prefab_paths.get(&source_prefab.guid) {
                observations.push(Observation::dependency(
                    from.clone(),
                    DependencyRelation::Uses,
                    prefab_id(repository, path),
                    format!("{}:{}", prefab.path.display(), source_prefab.line),
                ));
            }
        }
    }
    entities.sort_by(|left, right| left.id.cmp(&right.id));
    entities.dedup();
    (entities, observations, diagnostics)
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

    #[test]
    fn parses_and_links_prefab_scripts_and_source_prefabs() {
        let prefab = parse_unity_prefab(
            Path::new("Assets/Player.prefab"),
            r#"﻿--- !u!114 &1
MonoBehaviour:
  m_Script: {fileID: 11500000, guid: script-guid, type: 3}
  m_EditorClassIdentifier: Assembly-CSharp::Game.Player
  m_SourcePrefab: {fileID: 100100000, guid: prefab-guid, type: 3}
--- !u!114 &2
MonoBehaviour:
  m_Script: {fileID: 11500000, guid: missing-guid, type: 3}
"#
            .as_bytes(),
        )
        .unwrap();
        let analysis =
            crate::analyze("namespace Game; class Player : UnityEngine.MonoBehaviour {}").unwrap();
        let sources = [CsharpSource {
            path: Path::new("Assets/Player.cs"),
            assembly: "Assembly-CSharp",
            analysis: &analysis,
        }];
        let (entities, observations, diagnostics) = unity_prefab_dependencies(
            "example/game",
            std::slice::from_ref(&prefab),
            &BTreeMap::from([("script-guid".into(), PathBuf::from("Assets/Player.cs"))]),
            &BTreeMap::from([("prefab-guid".into(), PathBuf::from("Assets/Base.prefab"))]),
            &sources,
        );

        assert_eq!(entities[0].kind, EntityKind::UnityPrefab);
        assert_eq!(observations.len(), 2);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "unity.prefab_script_unresolved");
        assert!(observations.iter().any(|observation| {
            observation.to.as_str()
                == "repo://example/game/csharp/Assembly-CSharp/Assets/Player/Game/Player"
        }));
        assert!(observations.iter().any(|observation| {
            observation.to.as_str() == "repo://example/game/unity-prefab/Assets/Base.prefab"
        }));
    }
}
