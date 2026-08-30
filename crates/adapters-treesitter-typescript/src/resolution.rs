use super::model::*;
use beholder_adapters_graphql::{GraphqlSource, facts as graphql_facts};
use beholder_domain::{
    AnalysisDiagnostic, AnalysisDiagnosticSeverity, Confidence, DependencyOverride,
    DependencyRelation, EntityId, EntityKind, Observation, Provenance, SemanticRelation,
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

#[derive(Clone)]
struct Package {
    root: PathBuf,
    manifest: Value,
}

fn package_index(manifests: &[(&Path, &str)]) -> BTreeMap<String, Package> {
    let mut packages = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    for (path, source) in manifests {
        let Ok(manifest) = serde_json::from_str::<Value>(source) else {
            continue;
        };
        let Some(name) = manifest
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        if ambiguous.contains(&name) {
            continue;
        }
        let package = Package {
            root: path.parent().unwrap_or(Path::new("")).to_path_buf(),
            manifest,
        };
        if packages.insert(name.clone(), package).is_some() {
            packages.remove(&name);
            ambiguous.insert(name);
        }
    }
    packages
}

fn string_targets(value: &Value, targets: &mut Vec<PathBuf>) {
    match value {
        Value::String(target) => targets.push(target.into()),
        Value::Object(conditions) => {
            for condition in ["source", "types", "import", "default", "require"] {
                if let Some(value) = conditions.get(condition) {
                    string_targets(value, targets);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                string_targets(value, targets);
            }
        }
        _ => {}
    }
}

fn package_bases(source: &str, packages: &BTreeMap<String, Package>) -> Vec<PathBuf> {
    let Some((name, package)) = packages
        .iter()
        .filter(|(name, _)| source == name.as_str() || source.starts_with(&format!("{name}/")))
        .max_by_key(|(name, _)| name.len())
    else {
        return Vec::new();
    };
    package_targets(source, name, package)
}

fn package_targets(source: &str, name: &str, package: &Package) -> Vec<PathBuf> {
    let rest = source
        .strip_prefix(name)
        .unwrap_or("")
        .trim_start_matches('/');
    let key = if rest.is_empty() {
        ".".to_owned()
    } else {
        format!("./{rest}")
    };
    let mut targets = Vec::new();
    if let Some(exports) = package.manifest.get("exports") {
        if let Some(value) = exports.get(&key) {
            string_targets(value, &mut targets);
        } else if key == "."
            && !exports
                .as_object()
                .is_some_and(|map| map.keys().any(|key| key.starts_with('.')))
        {
            string_targets(exports, &mut targets);
        } else if let Some(map) = exports.as_object() {
            for (pattern, value) in map {
                let Some((prefix, suffix)) = pattern.split_once('*') else {
                    continue;
                };
                let Some(capture) = key
                    .strip_prefix(prefix)
                    .and_then(|value| value.strip_suffix(suffix))
                else {
                    continue;
                };
                let mut wildcard_targets = Vec::new();
                string_targets(value, &mut wildcard_targets);
                targets.extend(wildcard_targets.into_iter().map(|target| {
                    PathBuf::from(target.to_string_lossy().replacen('*', capture, 1))
                }));
            }
        }
    }
    if rest.is_empty() {
        for field in ["source", "module", "main"] {
            if let Some(target) = package.manifest.get(field).and_then(Value::as_str) {
                targets.push(target.into());
            }
        }
    }
    targets
        .into_iter()
        .map(|target| normalized(&package.root.join(target)))
        .chain(if rest.is_empty() {
            vec![package.root.join("src/index"), package.root.join("index")]
        } else {
            vec![package.root.join(rest)]
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

fn import_bases(caller: &Path, source: &str, packages: &BTreeMap<String, Package>) -> Vec<PathBuf> {
    if source.starts_with('.') {
        return vec![normalized(
            &caller.parent().unwrap_or(Path::new("")).join(source),
        )];
    }
    package_bases(source, packages)
}

fn source_candidates(base: PathBuf) -> Vec<PathBuf> {
    if SourceLanguage::from_path(&base).is_some() {
        return vec![base];
    }
    ["ts", "tsx", "d.ts", "js", "jsx"]
        .into_iter()
        .map(|extension| {
            let mut path = base.as_os_str().to_os_string();
            path.push(".");
            path.push(extension);
            PathBuf::from(path)
        })
        .chain(
            ["ts", "tsx", "d.ts", "js", "jsx"]
                .into_iter()
                .map(|extension| base.join("index").with_extension(extension)),
        )
        .collect()
}

fn imported_file(
    caller: &Path,
    source: &str,
    packages: &BTreeMap<String, Package>,
    aliases: &[PathAliases],
    files: &BTreeMap<PathBuf, &TypescriptAnalysis>,
) -> Option<PathBuf> {
    alias_targets(caller, source, aliases)
        .into_iter()
        .chain(import_bases(caller, source, packages))
        .flat_map(source_candidates)
        .find(|candidate| files.contains_key(candidate))
}

struct RepositoryIndex<'a> {
    files: BTreeMap<PathBuf, &'a TypescriptAnalysis>,
    packages: BTreeMap<String, Package>,
    aliases: Vec<PathAliases>,
    exports: BTreeMap<(PathBuf, String), EntityId>,
    origins: BTreeMap<(PathBuf, String), (PathBuf, String)>,
    members: BTreeMap<(PathBuf, String, String), EntityId>,
    bases: BTreeMap<(PathBuf, String), (PathBuf, String)>,
    field_types: BTreeMap<(PathBuf, String, String), (PathBuf, String)>,
    return_types: BTreeMap<(PathBuf, String), (PathBuf, String)>,
    caller_files: BTreeMap<String, PathBuf>,
    caller_owners: BTreeMap<String, (PathBuf, String)>,
    caller_bindings: BTreeMap<String, &'a [Binding]>,
    caller_alias_bindings: BTreeMap<String, &'a [AliasBinding]>,
    caller_factory_bindings: BTreeMap<String, &'a [FactoryBinding]>,
    file_imports: BTreeMap<PathBuf, &'a [Import]>,
}

type Origin = (PathBuf, String);
type Origins = BTreeMap<Origin, Origin>;

#[derive(Clone, Eq, PartialEq)]
enum NestTarget {
    Concrete(Origin),
    Alias(Origin),
}

struct ResolvedNestModule {
    imports: Vec<Origin>,
    providers: Vec<Origin>,
    members: Vec<Origin>,
    exports: Vec<Origin>,
}

fn insert_origin(
    origins: &mut Origins,
    origins_by_file: &mut BTreeMap<PathBuf, BTreeMap<String, Origin>>,
    export: Origin,
    origin: Origin,
) -> bool {
    origins_by_file
        .entry(export.0.clone())
        .or_default()
        .insert(export.1.clone(), origin.clone());
    origins.insert(export, origin.clone()).as_ref() != Some(&origin)
}

fn nest_reference_origin(
    file: &Path,
    name: &str,
    files: &BTreeMap<PathBuf, &TypescriptAnalysis>,
    packages: &BTreeMap<String, Package>,
    aliases: &[PathAliases],
    imports: &BTreeMap<PathBuf, &[Import]>,
    origins: &Origins,
) -> Option<Origin> {
    if matches!(name.as_bytes().first(), Some(b'\'' | b'"' | b'`')) {
        return Some((PathBuf::new(), name.trim_matches(['\'', '"', '`']).into()));
    }
    if let Some(imported) = imports.get(file).and_then(|imports| {
        imports.iter().find_map(|import| {
            let binding = import
                .bindings
                .iter()
                .find(|binding| binding.local == name && binding.imported != "*")?;
            let imported_file = imported_file(file, &import.source, packages, aliases, files)?;
            let reference = (imported_file, binding.imported.clone());
            Some(origins.get(&reference).cloned().unwrap_or(reference))
        })
    }) {
        return Some(imported);
    }
    let local = (file.to_path_buf(), name.to_owned());
    Some(origins.get(&local).cloned().unwrap_or(local))
}

fn merge_nest_provider(
    providers: &mut BTreeMap<Origin, Option<NestTarget>>,
    token: Origin,
    target: NestTarget,
) {
    providers
        .entry(token)
        .and_modify(|current| {
            if current.as_ref() != Some(&target) {
                *current = None;
            }
        })
        .or_insert(Some(target));
}

fn nest_own_providers(
    module: &ResolvedNestModule,
    custom: &BTreeMap<Origin, (Origin, NestTarget)>,
    symbols: &BTreeMap<Origin, EntityId>,
) -> BTreeMap<Origin, Option<NestTarget>> {
    let mut providers = BTreeMap::new();
    for provider in &module.providers {
        if let Some((token, target)) = custom.get(provider) {
            merge_nest_provider(&mut providers, token.clone(), target.clone());
        } else if symbols.contains_key(provider) {
            merge_nest_provider(
                &mut providers,
                provider.clone(),
                NestTarget::Concrete(provider.clone()),
            );
        }
    }
    providers
}

fn nest_exported_providers(
    module: &Origin,
    modules: &BTreeMap<Origin, ResolvedNestModule>,
    custom: &BTreeMap<Origin, (Origin, NestTarget)>,
    symbols: &BTreeMap<Origin, EntityId>,
    cache: &mut BTreeMap<Origin, BTreeMap<Origin, Option<NestTarget>>>,
    visiting: &mut BTreeSet<Origin>,
) -> BTreeMap<Origin, Option<NestTarget>> {
    if let Some(cached) = cache.get(module) {
        return cached.clone();
    }
    if !visiting.insert(module.clone()) {
        return BTreeMap::new();
    }
    let Some(definition) = modules.get(module) else {
        visiting.remove(module);
        return BTreeMap::new();
    };
    let own = nest_own_providers(definition, custom, symbols);
    let mut available = own.clone();
    for imported in &definition.imports {
        for (token, target) in
            nest_exported_providers(imported, modules, custom, symbols, cache, visiting)
        {
            if let Some(target) = target {
                merge_nest_provider(&mut available, token, target);
            }
        }
    }
    let mut exported = BTreeMap::new();
    for reference in &definition.exports {
        if modules.contains_key(reference) {
            for (token, target) in
                nest_exported_providers(reference, modules, custom, symbols, cache, visiting)
            {
                if let Some(target) = target {
                    merge_nest_provider(&mut exported, token, target);
                }
            }
        } else {
            let token = custom
                .get(reference)
                .map(|(token, _)| token)
                .unwrap_or(reference);
            if let Some(target) = concrete_nest_target(token, &available, &mut BTreeSet::new()) {
                merge_nest_provider(&mut exported, token.clone(), NestTarget::Concrete(target));
            }
        }
    }
    visiting.remove(module);
    cache.insert(module.clone(), exported.clone());
    exported
}

fn nest_visible_providers(
    module: &Origin,
    modules: &BTreeMap<Origin, ResolvedNestModule>,
    custom: &BTreeMap<Origin, (Origin, NestTarget)>,
    symbols: &BTreeMap<Origin, EntityId>,
    exported_cache: &mut BTreeMap<Origin, BTreeMap<Origin, Option<NestTarget>>>,
) -> BTreeMap<Origin, Option<NestTarget>> {
    let Some(definition) = modules.get(module) else {
        return BTreeMap::new();
    };
    let mut visible = nest_own_providers(definition, custom, symbols);
    for imported in &definition.imports {
        for (token, target) in nest_exported_providers(
            imported,
            modules,
            custom,
            symbols,
            exported_cache,
            &mut BTreeSet::new(),
        ) {
            if let Some(target) = target {
                merge_nest_provider(&mut visible, token, target);
            }
        }
    }
    visible
}

fn concrete_nest_target(
    token: &Origin,
    providers: &BTreeMap<Origin, Option<NestTarget>>,
    visiting: &mut BTreeSet<Origin>,
) -> Option<Origin> {
    if !visiting.insert(token.clone()) {
        return None;
    }
    let target = match providers.get(token)?.as_ref()? {
        NestTarget::Concrete(target) => Some(target.clone()),
        NestTarget::Alias(target) => concrete_nest_target(target, providers, visiting),
    };
    visiting.remove(token);
    target
}

struct NestInjectionContext<'a> {
    sources: &'a [(&'a Path, &'a TypescriptAnalysis)],
    files: &'a BTreeMap<PathBuf, &'a TypescriptAnalysis>,
    packages: &'a BTreeMap<String, Package>,
    aliases: &'a [PathAliases],
    imports: &'a BTreeMap<PathBuf, &'a [Import]>,
    origins: &'a Origins,
    symbols: &'a BTreeMap<Origin, EntityId>,
}

fn nest_injected_field_types(
    context: NestInjectionContext<'_>,
) -> BTreeMap<(PathBuf, String, String), Origin> {
    let NestInjectionContext {
        sources,
        files,
        packages,
        aliases,
        imports,
        origins,
        symbols,
    } = context;
    let reference = |file: &Path, name: &str| {
        nest_reference_origin(file, name, files, packages, aliases, imports, origins)
    };
    let mut custom = BTreeMap::new();
    let mut modules = BTreeMap::new();
    for (path, analysis) in sources {
        for provider in &analysis.nest_providers {
            let provider_origin = reference(path, &provider.name).unwrap();
            let Some(token) = reference(path, &provider.token) else {
                continue;
            };
            let Some(implementation) = reference(path, &provider.implementation) else {
                continue;
            };
            let target = if provider.existing {
                NestTarget::Alias(implementation)
            } else {
                NestTarget::Concrete(implementation)
            };
            custom.insert(provider_origin, (token, target));
        }
        for module in &analysis.nest_modules {
            let Some(module_origin) = reference(path, &module.name) else {
                continue;
            };
            let resolve = |names: &[String]| {
                names
                    .iter()
                    .filter_map(|name| reference(path, name))
                    .collect()
            };
            modules.insert(
                module_origin,
                ResolvedNestModule {
                    imports: resolve(&module.imports),
                    providers: resolve(&module.providers),
                    members: resolve(&module.members),
                    exports: resolve(&module.exports),
                },
            );
        }
    }
    let mut member_modules = BTreeMap::<Origin, Vec<Origin>>::new();
    for (module, definition) in &modules {
        for member in &definition.members {
            member_modules
                .entry(member.clone())
                .or_default()
                .push(module.clone());
        }
        for provider in &definition.providers {
            if let Some((_, NestTarget::Concrete(implementation))) = custom.get(provider) {
                member_modules
                    .entry(implementation.clone())
                    .or_default()
                    .push(module.clone());
            }
        }
    }
    let mut exported_cache = BTreeMap::new();
    let mut field_types = BTreeMap::new();
    for (path, analysis) in sources {
        for definition in analysis
            .definitions
            .iter()
            .filter(|definition| !definition.qualified_name.contains('/'))
        {
            let Some(owner) = reference(path, &definition.qualified_name) else {
                continue;
            };
            let Some(owner_modules) = member_modules.get(&owner) else {
                continue;
            };
            for binding in &definition.bindings {
                let (Some(field), Some(token)) = (
                    binding.receiver.strip_prefix("this."),
                    binding.injection_token.as_deref(),
                ) else {
                    continue;
                };
                let Some(token) = reference(path, token) else {
                    continue;
                };
                let targets = owner_modules
                    .iter()
                    .filter_map(|module| {
                        concrete_nest_target(
                            &token,
                            &nest_visible_providers(
                                module,
                                &modules,
                                &custom,
                                symbols,
                                &mut exported_cache,
                            ),
                            &mut BTreeSet::new(),
                        )
                    })
                    .collect::<BTreeSet<_>>();
                if targets.len() == 1 {
                    field_types.insert(
                        (
                            path.to_path_buf(),
                            definition.qualified_name.clone(),
                            field.into(),
                        ),
                        targets.into_iter().next().unwrap(),
                    );
                }
            }
        }
    }
    field_types
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
    let packages = package_index(manifests);
    let aliases = path_aliases(configs);
    let mut symbols = BTreeMap::<(PathBuf, String), EntityId>::new();
    let mut origins = Origins::new();
    let mut origins_by_file = BTreeMap::<PathBuf, BTreeMap<String, Origin>>::new();
    let mut members = BTreeMap::<(PathBuf, String, String), EntityId>::new();
    let mut raw_bases = BTreeMap::<(PathBuf, String), String>::new();
    let mut raw_field_types = BTreeMap::<(PathBuf, String, String), String>::new();
    let mut raw_return_types = BTreeMap::<(PathBuf, String), String>::new();
    let mut factories = BTreeMap::<(PathBuf, String), String>::new();
    let mut caller_files = BTreeMap::<String, PathBuf>::new();
    let mut caller_owners = BTreeMap::<String, (PathBuf, String)>::new();
    let mut caller_bindings = BTreeMap::<String, &[Binding]>::new();
    let mut caller_alias_bindings = BTreeMap::<String, &[AliasBinding]>::new();
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
        caller_files.insert(module_id.clone(), (*path).to_path_buf());
        caller_bindings.insert(module_id.clone(), &[]);
        caller_alias_bindings.insert(module_id.clone(), &[]);
        caller_factory_bindings.insert(module_id.clone(), &[]);
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
                    insert_origin(&mut origins, &mut origins_by_file, key.clone(), key);
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
                for binding in &definition.bindings {
                    if let Some(field) = binding.receiver.strip_prefix("this.") {
                        raw_field_types.insert(
                            (
                                (*path).to_path_buf(),
                                definition.qualified_name.clone(),
                                field.into(),
                            ),
                            binding.type_name.clone(),
                        );
                    }
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
                caller_alias_bindings.insert(id.clone(), &definition.alias_bindings);
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
                        changed |= insert_origin(
                            &mut origins,
                            &mut origins_by_file,
                            (path.clone(), export.exported.clone()),
                            origin,
                        );
                    }
                    continue;
                };
                let Some(exported_file) = imported_file(path, source, &packages, &aliases, &files)
                else {
                    continue;
                };
                if export.local == "*" {
                    let exported = origins_by_file
                        .get(&exported_file)
                        .into_iter()
                        .flatten()
                        .filter(|(name, _)| name.as_str() != "default")
                        .map(|(name, origin)| (name.clone(), origin.clone()))
                        .collect::<Vec<_>>();
                    for (name, origin) in exported {
                        changed |= insert_origin(
                            &mut origins,
                            &mut origins_by_file,
                            (path.clone(), name),
                            origin,
                        );
                    }
                } else if let Some(origin) =
                    origins.get(&(exported_file, export.local.clone())).cloned()
                {
                    changed |= insert_origin(
                        &mut origins,
                        &mut origins_by_file,
                        (path.clone(), export.exported.clone()),
                        origin,
                    );
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
    let mut field_types = raw_field_types
        .into_iter()
        .filter_map(|((file, owner, field), type_name)| {
            let origin = file_imports
                .get(&file)
                .and_then(|imports| {
                    imports.iter().find_map(|import| {
                        let binding = import.bindings.iter().find(|binding| {
                            binding.local == type_name && binding.imported != "*"
                        })?;
                        let imported_file =
                            imported_file(&file, &import.source, &packages, &aliases, &files)?;
                        origins
                            .get(&(imported_file, binding.imported.clone()))
                            .cloned()
                    })
                })
                .or_else(|| {
                    let key = (file.clone(), type_name);
                    symbols.contains_key(&key).then_some(key)
                })?;
            Some(((file, owner, field), origin))
        })
        .collect::<BTreeMap<_, _>>();
    field_types.extend(nest_injected_field_types(NestInjectionContext {
        sources,
        files: &files,
        packages: &packages,
        aliases: &aliases,
        imports: &file_imports,
        origins: &origins,
        symbols: &symbols,
    }));
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
        field_types,
        return_types,
        caller_files,
        caller_owners,
        caller_bindings,
        caller_alias_bindings,
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

fn field_type(
    index: &RepositoryIndex<'_>,
    mut file: PathBuf,
    mut owner: String,
    name: &str,
) -> Option<(PathBuf, String)> {
    for _ in 0..=index.bases.len() {
        if let Some(field_type) = index
            .field_types
            .get(&(file.clone(), owner.clone(), name.into()))
        {
            return Some(field_type.clone());
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
    cache: &mut BTreeMap<(String, String), Option<(PathBuf, String)>>,
) -> Option<(PathBuf, String)> {
    let key = (caller.to_owned(), receiver.to_owned());
    if let Some(resolved) = cache.get(&key) {
        return resolved.clone();
    }
    cache.insert(key.clone(), None);
    let resolved = receiver_type_uncached(index, caller, receiver, cache);
    cache.insert(key, resolved.clone());
    resolved
}

fn receiver_type_uncached(
    index: &RepositoryIndex<'_>,
    caller: &str,
    receiver: &str,
    cache: &mut BTreeMap<(String, String), Option<(PathBuf, String)>>,
) -> Option<(PathBuf, String)> {
    let file = index.caller_files.get(caller)?;
    let receiver = aliased_receiver(index, caller, receiver);
    if let Some(field) = receiver.strip_prefix("this.")
        && !field.contains('.')
        && let Some((owner_file, owner)) = index.caller_owners.get(caller)
        && let Some(resolved) = field_type(index, owner_file.clone(), owner.clone(), field)
    {
        return Some(resolved);
    }
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
    if let Some(factory) = index
        .caller_factory_bindings
        .get(caller)
        .and_then(|bindings| bindings.iter().find(|binding| binding.receiver == receiver))
        .map(|binding| binding.factory.as_str())
    {
        let factory_origin = imported_origin(index, file, factory)
            .or_else(|| Some((file.clone(), factory.to_owned())))
            .map(|origin| index.origins.get(&origin).cloned().unwrap_or(origin))?;
        if let Some(return_type) = index.return_types.get(&factory_origin) {
            return Some(return_type.clone());
        }
    }
    let parts = receiver.split('.').collect::<Vec<_>>();
    for prefix_length in (1..parts.len()).rev() {
        let prefix = parts[..prefix_length].join(".");
        let Some(mut current) = receiver_type(index, caller, &prefix, cache) else {
            continue;
        };
        let mut resolved = true;
        for field in &parts[prefix_length..] {
            let Some(next) = field_type(index, current.0.clone(), current.1.clone(), field) else {
                resolved = false;
                break;
            };
            current = next;
        }
        if resolved {
            return Some(current);
        }
    }
    None
}

fn aliased_receiver(index: &RepositoryIndex<'_>, caller: &str, receiver: &str) -> String {
    let aliases = index
        .caller_alias_bindings
        .get(caller)
        .copied()
        .unwrap_or_default();
    let mut receiver = receiver.to_owned();
    for _ in 0..=aliases.len() {
        let Some(source) = aliases
            .iter()
            .find(|binding| binding.receiver == receiver)
            .map(|binding| binding.source.clone())
        else {
            break;
        };
        receiver = source;
    }
    receiver
}

fn resolve_observations(observations: &mut [Observation], index: &RepositoryIndex<'_>) {
    let mut receiver_types = BTreeMap::new();
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
                let (file, type_name) = receiver_type(
                    index,
                    observation.from.as_str(),
                    receiver,
                    &mut receiver_types,
                )?;
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

fn graphql_operation_calls(
    repository: &str,
    sources: &[(&Path, &TypescriptAnalysis)],
    index: &RepositoryIndex<'_>,
) -> Vec<Observation> {
    let mut operations = BTreeMap::<EntityId, Vec<EntityId>>::new();
    for (path, analysis) in sources {
        let module_id = format!(
            "repo://{}/{}/{}",
            repository,
            analysis.language.id_segment(),
            source_stem(path)
        );
        for document in &analysis.graphql_documents {
            let facts = graphql_facts(
                repository,
                &[GraphqlSource {
                    path,
                    source: &document.source,
                    owner: None,
                }],
            );
            operations.insert(
                EntityId::from(format!("{module_id}/{}", document.binding)),
                facts
                    .entities
                    .into_iter()
                    .filter(|entity| entity.kind == EntityKind::GraphqlOperation)
                    .map(|entity| entity.id)
                    .collect(),
            );
        }
    }
    let mut observations = Vec::new();
    for (path, analysis) in sources {
        let module_id = format!(
            "repo://{}/{}/{}",
            repository,
            analysis.language.id_segment(),
            source_stem(path)
        );
        let imports = index.file_imports.get(*path).copied().unwrap_or_default();
        for definition in &analysis.definitions {
            if definition.kind != DefinitionKind::Callable {
                continue;
            }
            let caller = format!("{module_id}/{}", definition.qualified_name);
            for call in &definition.calls {
                for argument in &call.arguments {
                    let local = EntityId::from(format!("{module_id}/{argument}"));
                    let target = operations
                        .contains_key(&local)
                        .then_some(local)
                        .or_else(|| {
                            imports.iter().find_map(|import| {
                                let binding = import.bindings.iter().find(|binding| {
                                    binding.local == *argument && binding.imported != "*"
                                })?;
                                let file = imported_file(
                                    path,
                                    &import.source,
                                    &index.packages,
                                    &index.aliases,
                                    &index.files,
                                )?;
                                index
                                    .exports
                                    .get(&(file, binding.imported.clone()))
                                    .cloned()
                            })
                        });
                    let Some(selected) = target.and_then(|target| operations.get(&target)) else {
                        continue;
                    };
                    observations.extend(selected.iter().map(|operation| {
                        Observation::dependency(
                            caller.clone(),
                            DependencyRelation::CallsGraphql,
                            operation.clone(),
                            format!("{}:{}", path.display(), call.line),
                        )
                    }));
                }
            }
        }
    }
    observations
}

pub fn resolve_repository_calls(
    repository: &str,
    observations: &mut Vec<Observation>,
    sources: &[(&Path, &TypescriptAnalysis)],
    manifests: &[(&Path, &str)],
    configs: &[(&Path, &str)],
) {
    let index = repository_index(repository, sources, manifests, configs);
    observations.extend(graphql_operation_calls(repository, sources, &index));
    resolve_observations(observations, &index);
}

fn workspace_imported_file(
    source: &str,
    packages: &BTreeMap<String, Option<(usize, Package)>>,
    indexes: &[RepositoryIndex<'_>],
) -> Option<(usize, PathBuf)> {
    let (index, package) = packages
        .iter()
        .filter_map(|(name, package)| {
            (source == name.as_str() || source.starts_with(&format!("{name}/")))
                .then_some((name.len(), package.as_ref()?))
        })
        .max_by_key(|(length, _)| *length)?
        .1;
    let index = *index;
    let name = package.manifest.get("name")?.as_str()?;
    package_targets(source, name, package)
        .into_iter()
        .flat_map(source_candidates)
        .find(|candidate| indexes[index].files.contains_key(candidate))
        .map(|file| (index, file))
}

pub fn resolve_workspace_calls(
    observations: &mut [Observation],
    repositories: &[TypescriptRepository],
) -> Vec<DependencyOverride> {
    if repositories.len() < 2 {
        return Vec::new();
    }
    let indexes = repositories
        .iter()
        .map(|repository| {
            let sources = repository
                .sources
                .iter()
                .map(|(path, analysis)| (path.as_path(), analysis.as_ref()))
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
    let mut packages = BTreeMap::<String, Option<(usize, Package)>>::new();
    for (index, repository) in repositories.iter().enumerate() {
        for (name, package) in package_index(
            &repository
                .manifests
                .iter()
                .map(|(path, source)| (path.as_path(), source.as_str()))
                .collect::<Vec<_>>(),
        ) {
            packages
                .entry(name)
                .and_modify(|package| *package = None)
                .or_insert(Some((index, package)));
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
                let receiver = aliased_receiver(caller, observation.from.as_str(), receiver?);
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
                let receiver = aliased_receiver(caller, observation.from.as_str(), receiver?);
                let factory = caller
                    .caller_factory_bindings
                    .get(observation.from.as_str())?
                    .iter()
                    .find(|binding| binding.receiver == receiver)?
                    .factory
                    .as_str();
                let (target_index, file, imported) = imports.iter().find_map(|import| {
                    let binding = import
                        .bindings
                        .iter()
                        .find(|binding| binding.local == factory && binding.imported != "*")?;
                    let (target_index, file) =
                        workspace_imported_file(&import.source, &packages, &indexes)?;
                    Some((target_index, file, binding.imported.clone()))
                })?;
                let target = &indexes[target_index];
                let factory_origin = target.origins.get(&(file, imported)).cloned()?;
                let (file, owner) = target.return_types.get(&factory_origin)?.clone();
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

pub fn unresolved_call_diagnostics(
    observations: &[Observation],
) -> Vec<(String, AnalysisDiagnostic)> {
    let mut unresolved = BTreeMap::<(String, PathBuf), (usize, Option<u32>)>::new();
    for observation in observations.iter().filter(|observation| {
        observation.relation == SemanticRelation::Dependency(DependencyRelation::Calls)
            && (observation.to.as_str().starts_with("typescript-method://")
                || observation.to.as_str().starts_with("javascript-method://"))
    }) {
        let Some(caller) = observation.from.as_str().strip_prefix("repo://") else {
            continue;
        };
        let Some((repository, _)) = caller
            .split_once("/typescript/")
            .or_else(|| caller.split_once("/javascript/"))
        else {
            continue;
        };
        let (path, line) = observation
            .evidence
            .as_str()
            .rsplit_once(':')
            .map(|(path, line)| (path, line.parse().ok()))
            .unwrap_or((observation.evidence.as_str(), None));
        unresolved
            .entry((repository.into(), path.into()))
            .and_modify(|(count, _)| *count += 1)
            .or_insert((1, line));
    }
    unresolved
        .into_iter()
        .map(|((repository, path), (count, line))| {
            (
                repository,
                AnalysisDiagnostic {
                    code: "typescript.receiver_resolution_incomplete".into(),
                    severity: AnalysisDiagnosticSeverity::KnownLimitation,
                    path,
                    line,
                    detail: Some(format!(
                        "{count} receiver method call(s) remain unresolved after workspace resolution"
                    )),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze, observations_from_analysis};

    #[test]
    fn rejects_ambiguous_repository_package_names() {
        let packages = package_index(&[
            (Path::new("one/package.json"), r#"{"name":"duplicate"}"#),
            (Path::new("two/package.json"), r#"{"name":"duplicate"}"#),
            (Path::new("three/package.json"), r#"{"name":"unique"}"#),
        ]);

        assert!(!packages.contains_key("duplicate"));
        assert!(packages.contains_key("unique"));
    }

    #[test]
    fn bounds_unresolved_nested_receiver_resolution() {
        let receiver = std::iter::once("root")
            .chain((0..24).map(|_| "field"))
            .collect::<Vec<_>>()
            .join(".");
        let source = format!("export function run() {{ {receiver}.execute(); }}");
        let path = Path::new("src/run.ts");
        let analysis = analyze(&source, SourceLanguage::TypeScript).unwrap();
        let mut observations = observations_from_analysis("example", &analysis, &source, path);

        resolve_repository_calls("example", &mut observations, &[(path, &analysis)], &[], &[]);

        assert!(observations.iter().any(|observation| {
            observation.to.as_str() == format!("typescript-method://{receiver}/execute")
        }));
    }

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
                Path::new("packages/app/src/instrumentation-client.ts"),
                "import { loadLocale } from '@example/shell/src/loadLocale'; loadLocale();",
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
                Path::new("packages/app/src/dotted.service.tsx"),
                "export class DottedService { execute() {} }",
                SourceLanguage::Tsx,
            ),
            (
                Path::new("packages/app/src/dotted.controller.ts"),
                "import { DottedService } from './dotted.service'; export class DottedController { constructor(private readonly service: DottedService) {} run() { this.service.execute(); } }",
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
            (
                Path::new("packages/app/src/service-singleton.ts"),
                "import { Service } from './service'; export const serviceSingleton = new Service();",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/singleton-consumer.ts"),
                "import { serviceSingleton } from './service-singleton'; export function singleton() { serviceSingleton.execute(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/sender.ts"),
                "export interface Sender { send(message: string): Promise<void>; }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/interface-consumer.ts"),
                "import { Sender } from './sender'; export function send(sender: Sender) { sender.send('hello'); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/alias-consumer.ts"),
                "import { Service } from './service'; export function aliasedService(service: Service) { const first = service; let second; second = first; second.execute(); } export class AssignedConsumer { private service; constructor(service: Service) { this.service = service; } run() { this.service.execute(); } }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/exported/lib/root.ts"),
                "export function packageRoot() {}",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/exported/lib/tool.ts"),
                "export function packageTool() {}",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/export-map-consumer.ts"),
                "import { packageRoot } from '@example/exported'; import { packageTool } from '@example/exported/tool'; export function useExportMap() { packageRoot(); packageTool(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/async-service-factory.ts"),
                "import { Service } from './service'; export async function makeAsyncService(): Promise<Service> { return new Service(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/awaited-service-consumer.ts"),
                "import { makeAsyncService } from './async-service-factory'; export async function awaitedService() { const service = await makeAsyncService(); service.execute(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/object-api.ts"),
                "export const api = { run() {}, stop: () => undefined };",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/object-consumer.ts"),
                "import { api } from './object-api'; export function useObject() { api.run(); api.stop(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/returned-closure.ts"),
                "function work() {} export function createWorker() { return () => work(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/service-alias.ts"),
                "import { Service } from './service'; export type ServiceAlias = Service;",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/type-alias-consumer.ts"),
                "import { ServiceAlias } from './service-alias'; export function useAlias(service: ServiceAlias) { service.execute(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/child-sender.ts"),
                "import { Sender } from './sender'; export interface ChildSender extends Sender { close(): void; }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/interface-inheritance-consumer.ts"),
                "import { ChildSender } from './child-sender'; export function inheritedInterface(sender: ChildSender) { sender.send('hello'); sender.close(); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/client.ts"),
                "import { Service } from './service'; export class Client { service: Service; }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/nested-consumer.ts"),
                "import { Client } from './client'; import { Service } from './service'; function callWithCache<T>(load: () => Promise<T>) { return load(); } export function nested(client: Client) { client.service.execute(); } export function optional(service: Service) { service?.execute(); } export function callback(service: Service) { return callWithCache(async () => service.execute()); }",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/types.d.ts"),
                "export declare class DeclaredService { execute(): void; } export declare function declaredWork(): void; export declare const declaredService: DeclaredService;",
                SourceLanguage::TypeScript,
            ),
            (
                Path::new("packages/app/src/declaration-consumer.ts"),
                "import { declaredService, declaredWork } from './types'; export function useDeclarations() { declaredWork(); declaredService.execute(); }",
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
            &[
                (
                    Path::new("packages/shell/package.json"),
                    r#"{"name":"@example/shell"}"#,
                ),
                (
                    Path::new("packages/exported/package.json"),
                    r#"{"name":"@example/exported","exports":{".":{"source":"./lib/root.ts"},"./*":"./lib/*.ts"}}"#,
                ),
            ],
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
                == "repo://example/typescript/packages/app/src/instrumentation-client"
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
        assert!(observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://example/typescript/packages/app/src/dotted.controller/DottedController/run"
                && observation.to.as_str()
                    == "repo://example/typescript/packages/app/src/dotted.service/DottedService/execute"
        }));
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
        assert!(observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://example/typescript/packages/app/src/singleton-consumer/singleton"
                && observation.to.as_str()
                    == "repo://example/typescript/packages/app/src/service/Service/execute"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://example/typescript/packages/app/src/interface-consumer/send"
                && observation.to.as_str()
                    == "repo://example/typescript/packages/app/src/sender/Sender/send"
        }));
        for caller in ["aliasedService", "AssignedConsumer/run"] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str()
                    == format!("repo://example/typescript/packages/app/src/alias-consumer/{caller}")
                    && observation.to.as_str()
                        == "repo://example/typescript/packages/app/src/service/Service/execute"
            }));
        }
        for target in [
            "repo://example/typescript/packages/exported/lib/root/packageRoot",
            "repo://example/typescript/packages/exported/lib/tool/packageTool",
        ] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str()
                    == "repo://example/typescript/packages/app/src/export-map-consumer/useExportMap"
                    && observation.to.as_str() == target
            }));
        }
        assert!(observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://example/typescript/packages/app/src/awaited-service-consumer/awaitedService"
                && observation.to.as_str()
                    == "repo://example/typescript/packages/app/src/service/Service/execute"
        }));
        for target in [
            "repo://example/typescript/packages/app/src/object-api/api/run",
            "repo://example/typescript/packages/app/src/object-api/api/stop",
        ] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str()
                    == "repo://example/typescript/packages/app/src/object-consumer/useObject"
                    && observation.to.as_str() == target
            }));
        }
        assert!(observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://example/typescript/packages/app/src/returned-closure/createWorker"
                && observation.to.as_str()
                    == "repo://example/typescript/packages/app/src/returned-closure/work"
        }));
        assert!(observations.iter().any(|observation| {
            observation.from.as_str()
                == "repo://example/typescript/packages/app/src/type-alias-consumer/useAlias"
                && observation.to.as_str()
                    == "repo://example/typescript/packages/app/src/service/Service/execute"
        }));
        for target in [
            "repo://example/typescript/packages/app/src/sender/Sender/send",
            "repo://example/typescript/packages/app/src/child-sender/ChildSender/close",
        ] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str()
                    == "repo://example/typescript/packages/app/src/interface-inheritance-consumer/inheritedInterface"
                    && observation.to.as_str() == target
            }));
        }
        for caller in ["nested", "optional", "callback"] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str()
                    == format!(
                        "repo://example/typescript/packages/app/src/nested-consumer/{caller}"
                    )
                    && observation.to.as_str()
                        == "repo://example/typescript/packages/app/src/service/Service/execute"
            }));
        }
        let declaration_caller =
            "repo://example/typescript/packages/app/src/declaration-consumer/useDeclarations";
        for target in [
            "repo://example/typescript/packages/app/src/types.d/declaredWork",
            "repo://example/typescript/packages/app/src/types.d/DeclaredService/execute",
        ] {
            assert!(
                observations.iter().any(|observation| {
                    observation.from.as_str() == declaration_caller
                        && observation.to.as_str() == target
                }),
                "missing {target}: {observations:?}"
            );
        }
    }

    #[test]
    fn resolves_workspace_package_imports_across_repositories() {
        let consumer_source = "import { Service, makeService, work } from '@example/provider'; export function start(service: Service) { work(); service.execute(); const made = makeService(); const alias = made; alias.execute(); }";
        let provider_sources = [
            (
                PathBuf::from("src/index.ts"),
                "export { Service, makeService, work } from './impl';",
            ),
            (
                PathBuf::from("src/impl.ts"),
                "export class Service { execute() {} } export function makeService(): Service { return new Service(); } export function work() {}",
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

        assert_eq!(overrides.len(), 4);
        for target in [
            "repo://provider/typescript/src/impl/work",
            "repo://provider/typescript/src/impl/Service/execute",
        ] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str() == "repo://consumer/typescript/src/start/start"
                    && observation.to.as_str() == target
            }));
        }
        assert_eq!(
            observations
                .iter()
                .filter(|observation| {
                    observation.from.as_str() == "repo://consumer/typescript/src/start/start"
                        && observation.to.as_str()
                            == "repo://provider/typescript/src/impl/Service/execute"
                })
                .count(),
            2
        );
    }

    #[test]
    fn resolves_nest_injection_tokens_through_imported_module_providers() {
        let fixtures = [
            (
                Path::new("src/tokens.ts"),
                "export const WORKER = 'worker'; export const ALIAS = 'alias'; export const FACTORY = 'factory';",
            ),
            (
                Path::new("src/workers.ts"),
                "export class Worker { execute() {} } export class FactoryWorker { execute() {} } export class WrongWorker { execute() {} }",
            ),
            (
                Path::new("src/providers.ts"),
                "import { WORKER, ALIAS, FACTORY } from './tokens'; import { Worker, FactoryWorker } from './workers'; export const WorkerProvider = { provide: WORKER, useClass: Worker }; export const AliasProvider = { provide: ALIAS, useExisting: WORKER }; export const FactoryProvider = { provide: FACTORY, useFactory: () => new FactoryWorker() };",
            ),
            (
                Path::new("src/providers.module.ts"),
                "import { Module } from '@nestjs/common'; import { WORKER, ALIAS, FACTORY } from './tokens'; import { Worker } from './workers'; import { WorkerProvider, AliasProvider, FactoryProvider } from './providers'; @Module({ providers: [WorkerProvider, AliasProvider, FactoryProvider, { provide: 'literal', useClass: Worker }], exports: [WORKER, ALIAS, FACTORY, 'literal'] }) export class ProvidersModule {}",
            ),
            (
                Path::new("src/consumer.ts"),
                "import { Inject, Optional } from '@nestjs/common'; import { WORKER, ALIAS, FACTORY } from './tokens'; interface Port { execute(): void } export class Consumer { constructor(@Inject(WORKER) private worker: Port, @Inject(ALIAS) private alias: Port, @Inject(FACTORY) private factory: Port, @Optional() @Inject('literal') private literal: Port) {} run() { this.worker.execute(); this.alias.execute(); this.factory.execute(); this.literal.execute(); } }",
            ),
            (
                Path::new("src/consumer.module.ts"),
                "import { Module } from '@nestjs/common'; import { ProvidersModule } from './providers.module'; import { Consumer } from './consumer'; @Module({ imports: [ProvidersModule], controllers: [Consumer] }) export class ConsumerModule {}",
            ),
            (
                Path::new("src/unrelated.module.ts"),
                "import { Module } from '@nestjs/common'; import { WORKER } from './tokens'; import { WrongWorker } from './workers'; @Module({ providers: [{ provide: WORKER, useClass: WrongWorker }] }) export class UnrelatedModule {}",
            ),
        ];
        let analyses = fixtures
            .iter()
            .map(|(_, source)| analyze(source, SourceLanguage::TypeScript).unwrap())
            .collect::<Vec<_>>();
        let sources = fixtures
            .iter()
            .zip(&analyses)
            .map(|((path, _), analysis)| (*path, analysis))
            .collect::<Vec<_>>();
        let mut observations = fixtures
            .iter()
            .zip(&analyses)
            .flat_map(|((path, source), analysis)| {
                observations_from_analysis("example", analysis, source, path)
            })
            .collect::<Vec<_>>();

        resolve_repository_calls("example", &mut observations, &sources, &[], &[]);

        let caller = "repo://example/typescript/src/consumer/Consumer/run";
        for target in [
            "repo://example/typescript/src/workers/Worker/execute",
            "repo://example/typescript/src/workers/FactoryWorker/execute",
        ] {
            assert!(observations.iter().any(|observation| {
                observation.from.as_str() == caller && observation.to.as_str() == target
            }));
        }
        assert_eq!(
            observations
                .iter()
                .filter(|observation| {
                    observation.from.as_str() == caller
                        && observation.to.as_str()
                            == "repo://example/typescript/src/workers/Worker/execute"
                })
                .count(),
            3
        );
        assert!(!observations.iter().any(|observation| {
            observation.from.as_str() == caller
                && observation.to.as_str()
                    == "repo://example/typescript/src/workers/WrongWorker/execute"
        }));
    }

    #[test]
    fn skips_workspace_resolution_for_one_repository() {
        let source = "export function run() { missing(); }";
        let analysis = analyze(source, SourceLanguage::TypeScript).unwrap();
        let repositories = vec![TypescriptRepository::new(
            "example",
            vec![(PathBuf::from("src/run.ts"), analysis.clone())],
            vec![],
            vec![],
        )];
        let mut observations =
            observations_from_analysis("example", &analysis, source, Path::new("src/run.ts"));
        let before = observations.clone();

        assert!(resolve_workspace_calls(&mut observations, &repositories).is_empty());
        assert_eq!(observations, before);
    }

    #[test]
    fn reports_only_receiver_calls_left_unresolved_after_workspace_resolution() {
        let observations = vec![
            Observation::dependency(
                "repo://example/typescript/src/client/run",
                DependencyRelation::Calls,
                "typescript-method://client/send",
                "src/client.ts:4",
            ),
            Observation::dependency(
                "repo://example/typescript/src/client/run",
                DependencyRelation::Calls,
                "typescript-method://client/close",
                "src/client.ts:5",
            ),
            Observation::dependency(
                "repo://example/typescript/src/client/run",
                DependencyRelation::Calls,
                "typescript-call://external",
                "src/client.ts:6",
            ),
        ];

        let diagnostics = unresolved_call_diagnostics(&observations);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].0, "example");
        assert_eq!(
            diagnostics[0].1.detail.as_deref(),
            Some("2 receiver method call(s) remain unresolved after workspace resolution")
        );
    }
}
