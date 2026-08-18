use super::model::*;
use beholder_domain::{DependencyRelation, EntityId, Observation, SemanticRelation};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TsConfig {
    #[serde(default)]
    extends: OneOrMany,
    #[serde(default)]
    compiler_options: CompilerOptions,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompilerOptions {
    base_url: Option<PathBuf>,
    #[serde(default)]
    paths: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
    #[default]
    None,
}

struct PathAliases {
    directory: PathBuf,
    paths: BTreeMap<String, Vec<PathBuf>>,
}

fn normalized(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            Component::Normal(component) => result.push(component),
            Component::Prefix(_) | Component::RootDir => result.push(component.as_os_str()),
        }
    }
    result
}

fn source_stem(path: &Path) -> String {
    path.with_extension("")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn package_roots(manifests: &[(&Path, &str)]) -> BTreeMap<String, PathBuf> {
    manifests
        .iter()
        .filter_map(|(path, source)| {
            let name = serde_json::from_str::<Value>(source)
                .ok()?
                .get("name")?
                .as_str()?
                .to_owned();
            Some((name, path.parent().unwrap_or(Path::new("")).to_path_buf()))
        })
        .collect()
}

fn path_aliases(configs: &[(&Path, &str)]) -> Vec<PathAliases> {
    let configs = configs
        .iter()
        .filter_map(|(path, source)| {
            let config =
                jsonc_parser::parse_to_serde_value::<TsConfig>(source, &Default::default()).ok()?;
            Some(((*path).to_path_buf(), config))
        })
        .collect::<BTreeMap<_, _>>();
    configs
        .keys()
        .filter(|path| {
            matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("tsconfig.json" | "jsconfig.json")
            )
        })
        .map(|path| PathAliases {
            directory: path.parent().unwrap_or(Path::new("")).to_path_buf(),
            paths: effective_paths(path, &configs, &mut BTreeSet::new()),
        })
        .collect()
}

fn effective_paths(
    path: &Path,
    configs: &BTreeMap<PathBuf, TsConfig>,
    visiting: &mut BTreeSet<PathBuf>,
) -> BTreeMap<String, Vec<PathBuf>> {
    if !visiting.insert(path.to_path_buf()) {
        return BTreeMap::new();
    }
    let Some(config) = configs.get(path) else {
        return BTreeMap::new();
    };
    let directory = path.parent().unwrap_or(Path::new(""));
    let extended = match &config.extends {
        OneOrMany::One(path) => std::slice::from_ref(path),
        OneOrMany::Many(paths) => paths,
        OneOrMany::None => &[],
    };
    let mut paths = extended
        .iter()
        .filter(|extended| extended.starts_with('.'))
        .filter_map(|extended| {
            let mut path = normalized(&directory.join(extended));
            if path.extension().is_none() {
                path.set_extension("json");
            }
            configs.contains_key(&path).then_some(path)
        })
        .flat_map(|parent| effective_paths(&parent, configs, visiting))
        .collect::<BTreeMap<_, _>>();
    let base_url = normalized(
        &directory.join(
            config
                .compiler_options
                .base_url
                .as_deref()
                .unwrap_or(Path::new(".")),
        ),
    );
    paths.extend(
        config
            .compiler_options
            .paths
            .iter()
            .map(|(pattern, targets)| {
                (
                    pattern.clone(),
                    targets.iter().map(|target| base_url.join(target)).collect(),
                )
            }),
    );
    visiting.remove(path);
    paths
}

fn alias_targets(caller: &Path, source: &str, aliases: &[PathAliases]) -> Vec<PathBuf> {
    let Some(config) = aliases
        .iter()
        .filter(|config| caller.starts_with(&config.directory))
        .max_by_key(|config| config.directory.components().count())
    else {
        return Vec::new();
    };
    let mut matches = config
        .paths
        .iter()
        .filter_map(|(pattern, targets)| {
            if pattern == source {
                Some((usize::MAX, "", targets))
            } else {
                let (prefix, suffix) = pattern.split_once('*')?;
                source
                    .strip_prefix(prefix)?
                    .strip_suffix(suffix)
                    .map(|capture| (prefix.len(), capture, targets))
            }
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|(specificity, _, _)| std::cmp::Reverse(*specificity));
    matches
        .into_iter()
        .flat_map(|(_, capture, targets)| {
            targets.iter().map(move |target| {
                normalized(&PathBuf::from(
                    target.to_string_lossy().replacen('*', capture, 1),
                ))
            })
        })
        .collect()
}

fn import_base(
    caller: &Path,
    source: &str,
    packages: &BTreeMap<String, PathBuf>,
) -> Option<PathBuf> {
    if source.starts_with('.') {
        return Some(normalized(
            &caller.parent().unwrap_or(Path::new("")).join(source),
        ));
    }
    packages.iter().find_map(|(name, root)| {
        if source == name {
            Some(root.join("src/index"))
        } else {
            source
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('/'))
                .map(|rest| root.join(rest))
        }
    })
}

fn imported_file(
    caller: &Path,
    source: &str,
    packages: &BTreeMap<String, PathBuf>,
    aliases: &[PathAliases],
    files: &BTreeMap<PathBuf, &TypescriptAnalysis>,
) -> Option<PathBuf> {
    alias_targets(caller, source, aliases)
        .into_iter()
        .chain(import_base(caller, source, packages))
        .flat_map(|base| {
            if SourceLanguage::from_path(&base).is_some() {
                vec![base]
            } else {
                ["ts", "tsx", "js", "jsx"]
                    .into_iter()
                    .map(|extension| base.with_extension(extension))
                    .chain(
                        ["ts", "tsx", "js", "jsx"]
                            .into_iter()
                            .map(|extension| base.join("index").with_extension(extension)),
                    )
                    .collect()
            }
        })
        .find(|candidate| files.contains_key(candidate))
}

pub fn resolve_repository_calls(
    repository: &str,
    observations: &mut [Observation],
    sources: &[(&Path, &TypescriptAnalysis)],
    manifests: &[(&Path, &str)],
    configs: &[(&Path, &str)],
) {
    let files = sources
        .iter()
        .map(|(path, analysis)| ((*path).to_path_buf(), *analysis))
        .collect::<BTreeMap<_, _>>();
    let packages = package_roots(manifests);
    let aliases = path_aliases(configs);
    let mut symbols = BTreeMap::<(PathBuf, String), EntityId>::new();
    let mut origins = BTreeMap::<(PathBuf, String), (PathBuf, String)>::new();
    let mut members = BTreeMap::<(PathBuf, String, String), EntityId>::new();
    let mut factories = BTreeMap::<(PathBuf, String), String>::new();
    let mut caller_files = BTreeMap::<String, &Path>::new();
    let mut caller_bindings = BTreeMap::<String, &[Binding]>::new();
    let mut file_imports = BTreeMap::<PathBuf, &[Import]>::new();
    let mut file_exports = BTreeMap::<PathBuf, &[Export]>::new();
    for (path, analysis) in sources {
        let module_id = format!(
            "repo://{}/{}/{}",
            repository,
            analysis.language.id_segment(),
            source_stem(path)
        );
        file_imports.insert((*path).to_path_buf(), &analysis.imports);
        file_exports.insert((*path).to_path_buf(), &analysis.exports);
        let exported_owners = analysis
            .definitions
            .iter()
            .filter(|definition| definition.exported && !definition.qualified_name.contains('/'))
            .map(|definition| definition.qualified_name.as_str())
            .collect::<Vec<_>>();
        for definition in &analysis.definitions {
            let id = format!("{module_id}/{}", definition.qualified_name);
            if !definition.qualified_name.contains('/') {
                let key = ((*path).to_path_buf(), definition.qualified_name.clone());
                symbols.insert(key.clone(), EntityId::from(id.clone()));
                if definition.exported {
                    origins.insert(key.clone(), key);
                }
                if let Some(factory) = &definition.factory {
                    factories.insert(
                        ((*path).to_path_buf(), definition.qualified_name.clone()),
                        factory.clone(),
                    );
                }
            }
            if let Some((namespace, member)) = definition.qualified_name.rsplit_once('/')
                && exported_owners.contains(&namespace)
            {
                members.insert(
                    (
                        (*path).to_path_buf(),
                        namespace.to_owned(),
                        member.to_owned(),
                    ),
                    EntityId::from(id.clone()),
                );
            }
            if definition.kind == DefinitionKind::Callable {
                caller_files.insert(id.clone(), path);
                caller_bindings.insert(id, &definition.bindings);
            }
        }
    }

    for _ in 0..=sources.len() {
        let mut changed = false;
        for (path, exports) in &file_exports {
            for export in *exports {
                let Some(source) = export.source.as_deref() else {
                    let key = (path.clone(), export.local.clone());
                    let origin = symbols.contains_key(&key).then_some(key).or_else(|| {
                        file_imports.get(path).and_then(|imports| {
                            imports.iter().find_map(|import| {
                                let binding = import
                                    .bindings
                                    .iter()
                                    .find(|binding| binding.local == export.local)?;
                                let imported_file = imported_file(
                                    path,
                                    &import.source,
                                    &packages,
                                    &aliases,
                                    &files,
                                )?;
                                origins
                                    .get(&(imported_file, binding.imported.clone()))
                                    .cloned()
                            })
                        })
                    });
                    if let Some(origin) = origin {
                        changed |= origins
                            .insert((path.clone(), export.exported.clone()), origin.clone())
                            .as_ref()
                            != Some(&origin);
                    }
                    continue;
                };
                let Some(exported_file) = imported_file(path, source, &packages, &aliases, &files)
                else {
                    continue;
                };
                if export.local == "*" {
                    let exported = origins
                        .iter()
                        .filter(|((origin_file, name), _)| {
                            origin_file == &exported_file && name != "default"
                        })
                        .map(|((_, name), origin)| (name.clone(), origin.clone()))
                        .collect::<Vec<_>>();
                    for (name, origin) in exported {
                        changed |= origins
                            .insert((path.clone(), name), origin.clone())
                            .as_ref()
                            != Some(&origin);
                    }
                } else if let Some(origin) =
                    origins.get(&(exported_file, export.local.clone())).cloned()
                {
                    changed |= origins
                        .insert((path.clone(), export.exported.clone()), origin.clone())
                        .as_ref()
                        != Some(&origin);
                }
            }
        }
        if !changed {
            break;
        }
    }
    let exports = origins
        .iter()
        .filter_map(|(export, origin)| {
            symbols
                .get(origin)
                .map(|entity| (export.clone(), entity.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    for ((file, namespace), factory) in factories {
        let local_origin = origins
            .get(&(file.clone(), factory.clone()))
            .cloned()
            .or_else(|| {
                file_imports.get(&file).and_then(|imports| {
                    imports.iter().find_map(|import| {
                        let binding = import
                            .bindings
                            .iter()
                            .find(|binding| binding.local == factory)?;
                        let imported_file =
                            imported_file(&file, &import.source, &packages, &aliases, &files)?;
                        origins
                            .get(&(imported_file, binding.imported.clone()))
                            .cloned()
                    })
                })
            });
        let Some((factory_file, factory_name)) = local_origin else {
            continue;
        };
        let returned_members = members
            .iter()
            .filter(|((member_file, owner, _), _)| {
                member_file == &factory_file && owner == &factory_name
            })
            .map(|((_, _, name), entity)| (name.clone(), entity.clone()))
            .collect::<Vec<_>>();
        for (name, entity) in returned_members {
            members.insert((file.clone(), namespace.clone(), name), entity);
        }
    }

    for observation in observations.iter_mut().filter(|observation| {
        observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
    }) {
        let Some(caller_file) = caller_files.get(observation.from.as_str()) else {
            continue;
        };
        let imports = file_imports.get(*caller_file).copied().unwrap_or_default();
        let direct = observation
            .to
            .as_str()
            .strip_prefix("typescript-call://")
            .or_else(|| observation.to.as_str().strip_prefix("javascript-call://"))
            .map(|name| (name, None));
        let member = observation
            .to
            .as_str()
            .strip_prefix("typescript-method://")
            .or_else(|| observation.to.as_str().strip_prefix("javascript-method://"))
            .and_then(|target| target.split_once('/'))
            .map(|(receiver, name)| (name, Some(receiver)));
        let constructor = observation
            .to
            .as_str()
            .strip_prefix("typescript-constructor://")
            .or_else(|| {
                observation
                    .to
                    .as_str()
                    .strip_prefix("javascript-constructor://")
            })
            .map(|name| (name, None));
        let Some((name, receiver)) = direct.or(member).or(constructor) else {
            continue;
        };
        let candidate = imports
            .iter()
            .find_map(|import| {
                let binding = import.bindings.iter().find(|binding| match receiver {
                    Some(receiver) => binding.local == receiver && binding.imported == "*",
                    None => binding.local == name && binding.imported != "*",
                })?;
                let imported = if binding.imported == "*" {
                    name
                } else {
                    &binding.imported
                };
                let file = imported_file(caller_file, &import.source, &packages, &aliases, &files)?;
                exports.get(&(file, imported.to_owned()))
            })
            .or_else(|| {
                let receiver = receiver?;
                let type_name = caller_bindings
                    .get(observation.from.as_str())?
                    .iter()
                    .find(|binding| binding.receiver == receiver)?
                    .type_name
                    .as_str();
                let imported_type = imports.iter().find_map(|import| {
                    let binding = import
                        .bindings
                        .iter()
                        .find(|binding| binding.local == type_name && binding.imported != "*")?;
                    let file =
                        imported_file(caller_file, &import.source, &packages, &aliases, &files)?;
                    Some((file, binding.imported.as_str()))
                });
                let (mut file, mut type_name) =
                    imported_type.unwrap_or_else(|| ((*caller_file).to_path_buf(), type_name));
                let origin = origins.get(&(file.clone(), type_name.to_owned()));
                if let Some((origin_file, origin_name)) = origin {
                    file = origin_file.clone();
                    type_name = origin_name;
                }
                members.get(&(file, type_name.to_owned(), name.to_owned()))
            })
            .or_else(|| {
                let receiver = receiver?;
                let (file, namespace) = imports
                    .iter()
                    .find_map(|import| {
                        let binding = import
                            .bindings
                            .iter()
                            .find(|binding| binding.local == receiver && binding.imported != "*")?;
                        let file = imported_file(
                            caller_file,
                            &import.source,
                            &packages,
                            &aliases,
                            &files,
                        )?;
                        Some((file, binding.imported.clone()))
                    })
                    .unwrap_or_else(|| ((*caller_file).to_path_buf(), receiver.to_owned()));
                let (file, namespace) = origins
                    .get(&(file.clone(), namespace.clone()))
                    .cloned()
                    .unwrap_or((file, namespace));
                members.get(&(file, namespace, name.to_owned()))
            });
        if let Some(target) = candidate {
            observation.to = target.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze, observations_from_analysis};

    #[test]
    fn resolves_relative_workspace_package_and_tsconfig_imports() {
        let fixtures = [
            (
                Path::new("packages/app/src/start.ts"),
                "import { loadLocale as load } from '@example/shell/src/loadLocale'; export function start() { load(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/shell/src/loadLocale.ts"),
                "import { setLocale } from './current'; export function loadLocale() { setLocale(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/shell/src/current.ts"),
                "export function setLocale() {}",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/aliased.ts"),
                "import { setLocale } from '@shell/current'; export function aliased() { setLocale(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/service.ts"),
                "export class Service { execute() {} }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/consumer.ts"),
                "import { Service } from './service'; export function injected(service: Service) { service.execute(); } export function created() { const service = new Service(); service.execute(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/class-consumer.ts"),
                "import { Service } from './service'; export class InjectedConsumer { constructor(private readonly service: Service) {} run() { this.service.execute(); } } export class FieldConsumer { private service: Service; run() { this.service.execute(); } }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/default-service.ts"),
                "export default class DefaultService { execute() {} }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/barrel-functions.ts"),
                "export function ping() {}",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/index.ts"),
                "import DefaultService from './default-service'; export { DefaultService as ImportedBarrelService }; export { default as BarrelService } from './default-service'; export * from './barrel-functions';",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/barrel-consumer.ts"),
                "import { BarrelService, ImportedBarrelService, ping } from './index'; export function useBarrel(service: BarrelService) { service.execute(); ping(); } export function useImportedBarrel(service: ImportedBarrelService) { service.execute(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/factory.tsx"),
                "export default function makeFactory() { return { Route: () => <main /> }; }",
                SourceLanguage::Tsx,
            ),
            (
                Path::new("packages/app/src/modal.ts"),
                "import makeFactory from './factory'; export const Modal = makeFactory();",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/modal-index.ts"),
                "export { Modal } from './modal';",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/factory-consumer.tsx"),
                "import { Modal } from './modal-index'; export const FactoryConsumer = () => <Modal.Route />;",
                SourceLanguage::Tsx,
            ),
        ];
        let analyses = fixtures
            .iter()
            .map(|(_, source, language)| analyze(source, *language).unwrap())
            .collect::<Vec<_>>();
        let sources = fixtures
            .iter()
            .zip(&analyses)
            .map(|((path, _, _), analysis)| (*path, analysis))
            .collect::<Vec<_>>();
        let mut observations = fixtures
            .iter()
            .zip(&analyses)
            .flat_map(|((path, source, _), analysis)| {
                observations_from_analysis("example", analysis, source, path)
            })
            .collect::<Vec<_>>();
        resolve_repository_calls(
            "example",
            &mut observations,
            &sources,
            &[(
                Path::new("packages/shell/package.json"),
                r#"{"name":"@example/shell"}"#,
            )],
            &[
                (
                    Path::new("tsconfig.json"),
                    r#"{
                        // TypeScript accepts comments and trailing commas.
                        "compilerOptions": {
                            "baseUrl": ".",
                            "paths": { "@shell/*": ["packages/shell/src/*"], },
                        },
                    }"#,
                ),
                (
                    Path::new("packages/app/tsconfig.json"),
                    r#"{ "extends": "../../tsconfig.json" }"#,
                ),
            ],
        );

        assert!(observations.iter().any(|observation| {
            observation.from.as_str() == "repo://example/typescript/packages/app/src/start/start"
                && observation.to.as_str()
                    == "repo://example/typescript/packages/shell/src/loadLocale/loadLocale"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://example/typescript/packages/shell/src/loadLocale/loadLocale"
                && observation.to.as_str()
                    == "repo://example/typescript/packages/shell/src/current/setLocale"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://example/typescript/packages/app/src/aliased/aliased"
                && observation.to.as_str()
                    == "repo://example/typescript/packages/shell/src/current/setLocale"
        }));
        for caller in ["injected", "created"] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str()
                    == format!("repo://example/typescript/packages/app/src/consumer/{caller}")
                    && observation.to.as_str()
                        == "repo://example/typescript/packages/app/src/service/Service/execute"
            }));
        }
        for caller in ["InjectedConsumer/run", "FieldConsumer/run"] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str()
                    == format!("repo://example/typescript/packages/app/src/class-consumer/{caller}")
                    && observation.to.as_str()
                        == "repo://example/typescript/packages/app/src/service/Service/execute"
            }));
        }
        let barrel_caller = "repo://example/typescript/packages/app/src/barrel-consumer/useBarrel";
        for target in [
            "repo://example/typescript/packages/app/src/default-service/DefaultService/execute",
            "repo://example/typescript/packages/app/src/barrel-functions/ping",
        ] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str() == barrel_caller && observation.to.as_str() == target
            }));
        }
        assert!(observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://example/typescript/packages/app/src/barrel-consumer/useImportedBarrel"
                && observation.to.as_str()
                    == "repo://example/typescript/packages/app/src/default-service/DefaultService/execute"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://example/typescript/packages/app/src/factory-consumer/FactoryConsumer"
                && observation.to.as_str()
                    == "repo://example/typescript/packages/app/src/factory/makeFactory/Route"
        }));
        assert!(
            observations
                .iter()
                .all(|observation| observation.to.as_str() != "typescript-call://main")
        );
    }
}
