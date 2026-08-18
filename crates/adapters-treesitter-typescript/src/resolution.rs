use super::model::*;
use beholder_domain::{
    Confidence, DependencyOverride, DependencyRelation, EntityId, Observation, Provenance,
    SemanticRelation,
};
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

struct RepositoryIndex<'a> {
    files: BTreeMap<PathBuf, &'a TypescriptAnalysis>,
    packages: BTreeMap<String, PathBuf>,
    aliases: Vec<PathAliases>,
    exports: BTreeMap<(PathBuf, String), EntityId>,
    origins: BTreeMap<(PathBuf, String), (PathBuf, String)>,
    members: BTreeMap<(PathBuf, String, String), EntityId>,
    bases: BTreeMap<(PathBuf, String), (PathBuf, String)>,
    return_types: BTreeMap<(PathBuf, String), (PathBuf, String)>,
    caller_files: BTreeMap<String, PathBuf>,
    caller_owners: BTreeMap<String, (PathBuf, String)>,
    caller_bindings: BTreeMap<String, &'a [Binding]>,
    caller_factory_bindings: BTreeMap<String, &'a [FactoryBinding]>,
    file_imports: BTreeMap<PathBuf, &'a [Import]>,
}

fn repository_index<'a>(
    repository: &str,
    sources: &[(&Path, &'a TypescriptAnalysis)],
    manifests: &[(&Path, &str)],
    configs: &[(&Path, &str)],
) -> RepositoryIndex<'a> {
    let files = sources
        .iter()
        .map(|(path, analysis)| ((*path).to_path_buf(), *analysis))
        .collect::<BTreeMap<_, _>>();
    let packages = package_roots(manifests);
    let aliases = path_aliases(configs);
    let mut symbols = BTreeMap::<(PathBuf, String), EntityId>::new();
    let mut origins = BTreeMap::<(PathBuf, String), (PathBuf, String)>::new();
    let mut members = BTreeMap::<(PathBuf, String, String), EntityId>::new();
    let mut raw_bases = BTreeMap::<(PathBuf, String), String>::new();
    let mut raw_return_types = BTreeMap::<(PathBuf, String), String>::new();
    let mut factories = BTreeMap::<(PathBuf, String), String>::new();
    let mut caller_files = BTreeMap::<String, PathBuf>::new();
    let mut caller_owners = BTreeMap::<String, (PathBuf, String)>::new();
    let mut caller_bindings = BTreeMap::<String, &[Binding]>::new();
    let mut caller_factory_bindings = BTreeMap::<String, &[FactoryBinding]>::new();
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
        let member_owners = analysis
            .definitions
            .iter()
            .filter(|definition| {
                !definition.qualified_name.contains('/')
                    && (definition.kind == DefinitionKind::Namespace || definition.exported)
            })
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
                if let Some(base) = &definition.base {
                    raw_bases.insert(
                        ((*path).to_path_buf(), definition.qualified_name.clone()),
                        base.clone(),
                    );
                }
                if let Some(return_type) = &definition.return_type {
                    raw_return_types.insert(
                        ((*path).to_path_buf(), definition.qualified_name.clone()),
                        return_type.clone(),
                    );
                }
            }
            if let Some((namespace, member)) = definition.qualified_name.rsplit_once('/')
                && member_owners.contains(&namespace)
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
                caller_files.insert(id.clone(), (*path).to_path_buf());
                caller_bindings.insert(id.clone(), &definition.bindings);
                caller_factory_bindings.insert(id, &definition.factory_bindings);
                if let Some((owner, member)) = definition.qualified_name.rsplit_once('/')
                    && !member.contains('/')
                    && member_owners.contains(&owner)
                {
                    caller_owners.insert(
                        format!("{module_id}/{}", definition.qualified_name),
                        ((*path).to_path_buf(), owner.to_owned()),
                    );
                }
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
    let return_types = raw_return_types
        .into_iter()
        .filter_map(|(callable, return_type)| {
            let file = &callable.0;
            let origin = file_imports
                .get(file)
                .and_then(|imports| {
                    imports.iter().find_map(|import| {
                        let binding = import.bindings.iter().find(|binding| {
                            binding.local == return_type && binding.imported != "*"
                        })?;
                        let imported_file =
                            imported_file(file, &import.source, &packages, &aliases, &files)?;
                        origins
                            .get(&(imported_file, binding.imported.clone()))
                            .cloned()
                    })
                })
                .or_else(|| {
                    let key = (file.clone(), return_type);
                    symbols.contains_key(&key).then_some(key)
                })?;
            Some((callable, origin))
        })
        .collect::<BTreeMap<_, _>>();
    for ((file, namespace), factory) in &factories {
        let local_origin = origins
            .get(&(file.clone(), factory.clone()))
            .cloned()
            .or_else(|| {
                file_imports.get(file).and_then(|imports| {
                    imports.iter().find_map(|import| {
                        let binding = import
                            .bindings
                            .iter()
                            .find(|binding| binding.local == *factory)?;
                        let imported_file =
                            imported_file(file, &import.source, &packages, &aliases, &files)?;
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
    let mut bases = raw_bases
        .into_iter()
        .filter_map(|((file, owner), base)| {
            let origin = file_imports
                .get(&file)
                .and_then(|imports| {
                    imports.iter().find_map(|import| {
                        let binding = import
                            .bindings
                            .iter()
                            .find(|binding| binding.local == base)?;
                        let imported_file =
                            imported_file(&file, &import.source, &packages, &aliases, &files)?;
                        origins
                            .get(&(imported_file, binding.imported.clone()))
                            .cloned()
                    })
                })
                .or_else(|| {
                    let key = (file.clone(), base);
                    symbols.contains_key(&key).then_some(key)
                })?;
            Some(((file, owner), origin))
        })
        .collect::<BTreeMap<_, _>>();
    for ((file, namespace), factory) in factories {
        let factory_origin = origins
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
        if let Some(return_type) = factory_origin.and_then(|origin| return_types.get(&origin)) {
            bases.insert((file, namespace), return_type.clone());
        }
    }

    RepositoryIndex {
        files,
        packages,
        aliases,
        exports,
        origins,
        members,
        bases,
        return_types,
        caller_files,
        caller_owners,
        caller_bindings,
        caller_factory_bindings,
        file_imports,
    }
}

fn member_entity<'a>(
    index: &'a RepositoryIndex<'_>,
    mut file: PathBuf,
    mut owner: String,
    name: &str,
) -> Option<&'a EntityId> {
    for _ in 0..=index.bases.len() {
        if let Some(member) = index
            .members
            .get(&(file.clone(), owner.clone(), name.into()))
        {
            return Some(member);
        }
        let base = index.bases.get(&(file, owner))?;
        file = base.0.clone();
        owner = base.1.clone();
    }
    None
}

fn imported_origin(
    index: &RepositoryIndex<'_>,
    file: &Path,
    name: &str,
) -> Option<(PathBuf, String)> {
    index.file_imports.get(file)?.iter().find_map(|import| {
        let binding = import
            .bindings
            .iter()
            .find(|binding| binding.local == name && binding.imported != "*")?;
        let imported_file = imported_file(
            file,
            &import.source,
            &index.packages,
            &index.aliases,
            &index.files,
        )?;
        index
            .origins
            .get(&(imported_file, binding.imported.clone()))
            .cloned()
    })
}

fn receiver_type(
    index: &RepositoryIndex<'_>,
    caller: &str,
    receiver: &str,
) -> Option<(PathBuf, String)> {
    let file = index.caller_files.get(caller)?;
    if let Some(type_name) = index
        .caller_bindings
        .get(caller)
        .and_then(|bindings| bindings.iter().find(|binding| binding.receiver == receiver))
        .map(|binding| binding.type_name.as_str())
    {
        return imported_origin(index, file, type_name)
            .or_else(|| Some((file.clone(), type_name.to_owned())))
            .map(|origin| index.origins.get(&origin).cloned().unwrap_or(origin));
    }
    let factory = index
        .caller_factory_bindings
        .get(caller)?
        .iter()
        .find(|binding| binding.receiver == receiver)?
        .factory
        .as_str();
    let factory_origin = imported_origin(index, file, factory)
        .or_else(|| Some((file.clone(), factory.to_owned())))
        .map(|origin| index.origins.get(&origin).cloned().unwrap_or(origin))?;
    index.return_types.get(&factory_origin).cloned()
}

fn resolve_observations(observations: &mut [Observation], index: &RepositoryIndex<'_>) {
    for observation in observations.iter_mut().filter(|observation| {
        observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
    }) {
        let Some(caller_file) = index.caller_files.get(observation.from.as_str()) else {
            continue;
        };
        let imports = index
            .file_imports
            .get(caller_file)
            .copied()
            .unwrap_or_default();
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
                let file = imported_file(
                    caller_file,
                    &import.source,
                    &index.packages,
                    &index.aliases,
                    &index.files,
                )?;
                index.exports.get(&(file, imported.to_owned()))
            })
            .or_else(|| {
                (receiver == Some("this"))
                    .then(|| index.caller_owners.get(observation.from.as_str()))
                    .flatten()
                    .and_then(|(file, owner)| {
                        member_entity(index, file.clone(), owner.clone(), name)
                    })
            })
            .or_else(|| {
                let receiver = receiver?;
                let (file, type_name) = receiver_type(index, observation.from.as_str(), receiver)?;
                member_entity(index, file, type_name, name)
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
                            &index.packages,
                            &index.aliases,
                            &index.files,
                        )?;
                        Some((file, binding.imported.clone()))
                    })
                    .unwrap_or_else(|| (caller_file.to_path_buf(), receiver.to_owned()));
                let (file, namespace) = index
                    .origins
                    .get(&(file.clone(), namespace.clone()))
                    .cloned()
                    .unwrap_or((file, namespace));
                member_entity(index, file, namespace, name)
            });
        if let Some(target) = candidate {
            observation.to = target.clone();
        }
    }
}

pub fn resolve_repository_calls(
    repository: &str,
    observations: &mut [Observation],
    sources: &[(&Path, &TypescriptAnalysis)],
    manifests: &[(&Path, &str)],
    configs: &[(&Path, &str)],
) {
    resolve_observations(
        observations,
        &repository_index(repository, sources, manifests, configs),
    );
}

fn workspace_imported_file(
    source: &str,
    packages: &BTreeMap<String, Option<(usize, PathBuf)>>,
    indexes: &[RepositoryIndex<'_>],
) -> Option<(usize, PathBuf)> {
    let (index, base) = packages.iter().find_map(|(name, package)| {
        let (index, root) = package.as_ref()?;
        if source == name {
            Some((*index, root.join("src/index")))
        } else {
            source
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('/'))
                .map(|rest| (*index, root.join(rest)))
        }
    })?;
    let candidates = if SourceLanguage::from_path(&base).is_some() {
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
    };
    candidates
        .into_iter()
        .find(|candidate| indexes[index].files.contains_key(candidate))
        .map(|file| (index, file))
}

pub fn resolve_workspace_calls(
    observations: &mut [Observation],
    repositories: &[TypescriptRepository],
) -> Vec<DependencyOverride> {
    let indexes = repositories
        .iter()
        .map(|repository| {
            let sources = repository
                .sources
                .iter()
                .map(|(path, analysis)| (path.as_path(), analysis))
                .collect::<Vec<_>>();
            let manifests = repository
                .manifests
                .iter()
                .map(|(path, source)| (path.as_path(), source.as_str()))
                .collect::<Vec<_>>();
            let configs = repository
                .configs
                .iter()
                .map(|(path, source)| (path.as_path(), source.as_str()))
                .collect::<Vec<_>>();
            repository_index(&repository.repository, &sources, &manifests, &configs)
        })
        .collect::<Vec<_>>();
    let mut packages = BTreeMap::<String, Option<(usize, PathBuf)>>::new();
    for (index, repository) in repositories.iter().enumerate() {
        for (name, root) in package_roots(
            &repository
                .manifests
                .iter()
                .map(|(path, source)| (path.as_path(), source.as_str()))
                .collect::<Vec<_>>(),
        ) {
            packages
                .entry(name)
                .and_modify(|package| *package = None)
                .or_insert(Some((index, root)));
        }
    }
    let callers = indexes
        .iter()
        .enumerate()
        .flat_map(|(index, repository)| {
            repository
                .caller_files
                .keys()
                .map(move |caller| (caller.as_str(), index))
        })
        .collect::<BTreeMap<_, _>>();

    let mut overrides = Vec::new();
    for observation in observations.iter_mut().filter(|observation| {
        observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
    }) {
        let Some(&caller_index) = callers.get(observation.from.as_str()) else {
            continue;
        };
        let caller = &indexes[caller_index];
        let caller_file = &caller.caller_files[observation.from.as_str()];
        let imports = caller
            .file_imports
            .get(caller_file)
            .copied()
            .unwrap_or_default();
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
                let (target_index, file) =
                    workspace_imported_file(&import.source, &packages, &indexes)?;
                indexes[target_index]
                    .exports
                    .get(&(file, imported.to_owned()))
            })
            .or_else(|| {
                let receiver = receiver?;
                let type_name = caller
                    .caller_bindings
                    .get(observation.from.as_str())?
                    .iter()
                    .find(|binding| binding.receiver == receiver)?
                    .type_name
                    .as_str();
                let (target_index, file, imported) = imports.iter().find_map(|import| {
                    let binding = import
                        .bindings
                        .iter()
                        .find(|binding| binding.local == type_name && binding.imported != "*")?;
                    let (target_index, file) =
                        workspace_imported_file(&import.source, &packages, &indexes)?;
                    Some((target_index, file, binding.imported.as_str()))
                })?;
                let target = &indexes[target_index];
                let (file, owner) = target
                    .origins
                    .get(&(file.clone(), imported.to_owned()))
                    .cloned()
                    .unwrap_or((file, imported.to_owned()));
                member_entity(target, file, owner, name)
            })
            .or_else(|| {
                let receiver = receiver?;
                let (target_index, file, imported) = imports.iter().find_map(|import| {
                    let binding = import
                        .bindings
                        .iter()
                        .find(|binding| binding.local == receiver && binding.imported != "*")?;
                    let (target_index, file) =
                        workspace_imported_file(&import.source, &packages, &indexes)?;
                    Some((target_index, file, binding.imported.clone()))
                })?;
                let target = &indexes[target_index];
                let (file, owner) = target
                    .origins
                    .get(&(file.clone(), imported.clone()))
                    .cloned()
                    .unwrap_or((file, imported));
                member_entity(target, file, owner, name)
            });
        if let Some(target) = candidate {
            overrides.push(DependencyOverride {
                from: observation.from.clone(),
                relation: DependencyRelation::Calls,
                unresolved_to: observation.to.clone(),
                resolved_to: target.clone(),
                evidence: observation.evidence.clone(),
                confidence: Confidence::Exact,
                provenance: Provenance::Ast,
            });
            observation.to = target.clone();
        }
    }
    overrides
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
            (
                Path::new("packages/app/src/async-consumer.ts"),
                "import { Service } from './service'; export async function asyncConsumer(service: Service) { await Promise.resolve().then(() => service.execute()); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/base.ts"),
                "export class Base { execute() {} }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/child.ts"),
                "import { Base } from './base'; export class Child extends Base { run() { this.execute(); } }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/inheritance-consumer.ts"),
                "import { Child } from './child'; export function inherited(child: Child) { child.execute(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/service-factory.ts"),
                "import { Service } from './service'; export function makeService(): Service { return new Service(); } export function inferService() { return new Service(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/returned-service-consumer.ts"),
                "import { makeService, inferService } from './service-factory'; export function explicitReturn() { const service = makeService(); service.execute(); } export function inferredReturn() { const service = inferService(); service.execute(); }",
                SourceLanguage::TypeScript,
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
        assert!(observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://example/typescript/packages/app/src/async-consumer/asyncConsumer"
                && observation.to.as_str()
                    == "repo://example/typescript/packages/app/src/service/Service/execute"
        }));
        for caller in [
            "repo://example/typescript/packages/app/src/child/Child/run",
            "repo://example/typescript/packages/app/src/inheritance-consumer/inherited",
        ] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str() == caller
                    && observation.to.as_str()
                        == "repo://example/typescript/packages/app/src/base/Base/execute"
            }));
        }
        for caller in ["explicitReturn", "inferredReturn"] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str()
                    == format!(
                        "repo://example/typescript/packages/app/src/returned-service-consumer/{caller}"
                    )
                    && observation.to.as_str()
                        == "repo://example/typescript/packages/app/src/service/Service/execute"
            }));
        }
    }

    #[test]
    fn resolves_workspace_package_imports_across_repositories() {
        let consumer_source = "import { Service, work } from '@example/provider'; export function start(service: Service) { work(); service.execute(); }";
        let provider_sources = [
            (
                PathBuf::from("src/index.ts"),
                "export { Service, work } from './impl';",
            ),
            (
                PathBuf::from("src/impl.ts"),
                "export class Service { execute() {} } export function work() {}",
            ),
        ];
        let consumer_analysis = analyze(consumer_source, SourceLanguage::TypeScript).unwrap();
        let provider_analyses = provider_sources
            .iter()
            .map(|(path, source)| {
                (
                    path.clone(),
                    analyze(source, SourceLanguage::TypeScript).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let repositories = vec![
            TypescriptRepository::new(
                "consumer",
                vec![(PathBuf::from("src/start.ts"), consumer_analysis.clone())],
                vec![],
                vec![],
            ),
            TypescriptRepository::new(
                "provider",
                provider_analyses.clone(),
                vec![(
                    PathBuf::from("package.json"),
                    r#"{"name":"@example/provider"}"#.into(),
                )],
                vec![],
            ),
        ];
        let mut observations = observations_from_analysis(
            "consumer",
            &consumer_analysis,
            consumer_source,
            Path::new("src/start.ts"),
        );
        for ((path, source), (_, analysis)) in provider_sources.iter().zip(&provider_analyses) {
            observations.extend(observations_from_analysis(
                "provider", analysis, source, path,
            ));
        }

        let overrides = resolve_workspace_calls(&mut observations, &repositories);

        assert_eq!(overrides.len(), 2);
        for target in [
            "repo://provider/typescript/src/impl/work",
            "repo://provider/typescript/src/impl/Service/execute",
        ] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str() == "repo://consumer/typescript/src/start/start"
                    && observation.to.as_str() == target
            }));
        }
    }
}
