use beholder_indexing::{AnalysisInput, AnalysisInputKind};
use protox::{
    Compiler, Error as ProtoxError,
    file::{ChainFileResolver, DescriptorSetFileResolver, File, FileResolver, GoogleFileResolver},
};
use rayon::prelude::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

const COMPILER_ID: &str = "protox-0.9.1-v3";

pub struct SourceCompiler {
    cache_dir: PathBuf,
    memory: Mutex<BTreeMap<[u8; 32], Arc<Vec<u8>>>>,
}

#[derive(Deserialize)]
struct BufConfig {
    #[serde(default)]
    modules: Vec<BufModule>,
    #[serde(default)]
    deps: Vec<String>,
}

#[derive(Deserialize)]
struct BufModule {
    path: PathBuf,
    #[serde(default)]
    includes: Vec<PathBuf>,
    #[serde(default)]
    excludes: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct BufLock {
    #[serde(default)]
    deps: Vec<LockedDependency>,
}

#[derive(Clone, Deserialize)]
struct LockedDependency {
    name: String,
    commit: String,
    digest: String,
}

struct Module {
    name: PathBuf,
    roots: Vec<PathBuf>,
    protos: Vec<PathBuf>,
    inputs: Vec<AnalysisInput>,
    dependencies: Vec<LockedDependency>,
}

#[derive(Debug)]
struct SnapshotFileResolver {
    roots: Vec<PathBuf>,
    sources: BTreeMap<PathBuf, String>,
}

impl SnapshotFileResolver {
    fn new(roots: Vec<PathBuf>, inputs: &[AnalysisInput]) -> Result<Self, String> {
        let sources = inputs
            .iter()
            .filter(|input| input.kind == AnalysisInputKind::Source)
            .map(|input| {
                let source = std::str::from_utf8(&input.content).map_err(|error| {
                    format!("invalid UTF-8 in {}: {error}", input.path.display())
                })?;
                Ok((input.path.clone(), source.to_owned()))
            })
            .collect::<Result<_, String>>()?;
        Ok(Self { roots, sources })
    }
}

impl FileResolver for SnapshotFileResolver {
    fn resolve_path(&self, path: &Path) -> Option<String> {
        self.roots
            .iter()
            .find_map(|root| path.strip_prefix(root).ok().and_then(protobuf_file_name))
    }

    fn open_file(&self, name: &str) -> Result<File, ProtoxError> {
        let name_path = relative_path(Path::new(name)).ok();
        let source = name_path.as_ref().and_then(|name_path| {
            self.roots
                .iter()
                .find_map(|root| self.sources.get(&root.join(name_path)))
        });
        source.map_or_else(
            || Err(ProtoxError::file_not_found(name)),
            |source| File::from_source(name, source),
        )
    }
}

impl SourceCompiler {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir: cache_dir.join("protobuf"),
            memory: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn compile_repository(
        &self,
        inputs: &[AnalysisInput],
    ) -> Result<Vec<Arc<Vec<u8>>>, String> {
        let modules = modules(inputs)?;
        let mut dependencies = BTreeMap::new();
        for dependency in modules.iter().flat_map(|module| module.dependencies.iter()) {
            dependencies
                .entry((
                    dependency.name.clone(),
                    dependency.commit.clone(),
                    dependency.digest.clone(),
                ))
                .or_insert(dependency);
        }
        for dependency in dependencies.into_values() {
            self.dependency(dependency)?;
        }
        modules
            .into_par_iter()
            .map(|module| self.compile(module))
            .collect()
    }

    pub fn clear_memory(&self) -> Result<(), String> {
        self.memory
            .lock()
            .map_err(|_| "Protobuf compiler cache lock poisoned".to_owned())?
            .clear();
        Ok(())
    }

    fn compile(&self, module: Module) -> Result<Arc<Vec<u8>>, String> {
        let key = compilation_key(&module);
        self.cached("compiled", key, || {
            let mut resolver = ChainFileResolver::new();
            resolver.add(SnapshotFileResolver::new(
                module.roots.clone(),
                &module.inputs,
            )?);
            for dependency in &module.dependencies {
                let descriptor = self.dependency(dependency)?;
                resolver.add(
                    DescriptorSetFileResolver::decode(descriptor.as_slice()).map_err(|error| {
                        format!("invalid cached dependency {}: {error}", dependency.name)
                    })?,
                );
            }
            resolver.add(GoogleFileResolver::new());
            let mut compiler = Compiler::with_file_resolver(resolver);
            compiler.include_imports(true).include_source_info(false);
            compiler.open_files(&module.protos).map_err(|error| {
                format!(
                    "failed to compile Protobuf module {}: {error}",
                    module.name.display()
                )
            })?;
            Ok(compiler.encode_file_descriptor_set())
        })
    }

    fn dependency(&self, dependency: &LockedDependency) -> Result<Arc<Vec<u8>>, String> {
        let url = dependency_url(dependency)?;
        let key = content_key(
            "dependency",
            [
                (Path::new("name"), dependency.name.as_bytes()),
                (Path::new("commit"), dependency.commit.as_bytes()),
                (Path::new("digest"), dependency.digest.as_bytes()),
            ],
        );
        self.cached("dependencies", key, || {
            let response = reqwest::blocking::Client::new()
                .get(&url)
                .header("Accept", "application/proto")
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .map_err(|error| format!("failed to download {}: {error}", dependency.name))?;
            let bytes = response
                .bytes()
                .map_err(|error| format!("failed to read {}: {error}", dependency.name))?;
            let bytes = bytes.to_vec();
            DescriptorSetFileResolver::decode(bytes.as_slice()).map_err(|error| {
                format!("invalid descriptor set for {}: {error}", dependency.name)
            })?;
            Ok(bytes)
        })
    }

    fn cached(
        &self,
        kind: &str,
        key: [u8; 32],
        create: impl FnOnce() -> Result<Vec<u8>, String>,
    ) -> Result<Arc<Vec<u8>>, String> {
        if let Some(bytes) = self
            .memory
            .lock()
            .map_err(|_| "Protobuf compiler cache lock poisoned".to_owned())?
            .get(&key)
            .cloned()
        {
            return Ok(bytes);
        }
        let path = self
            .cache_dir
            .join(kind)
            .join(format!("{}.binpb", hex(key)));
        if let Ok(bytes) = fs::read(&path)
            && DescriptorSetFileResolver::decode(bytes.as_slice()).is_ok()
        {
            let bytes = Arc::new(bytes);
            self.memory
                .lock()
                .map_err(|_| "Protobuf compiler cache lock poisoned".to_owned())?
                .insert(key, bytes.clone());
            return Ok(bytes);
        }
        let bytes = Arc::new(create()?);
        if let Some(parent) = path.parent()
            && fs::create_dir_all(parent).is_ok()
        {
            let _ = fs::write(path, bytes.as_slice());
        }
        self.memory
            .lock()
            .map_err(|_| "Protobuf compiler cache lock poisoned".to_owned())?
            .insert(key, bytes.clone());
        Ok(bytes)
    }
}

fn dependency_url(dependency: &LockedDependency) -> Result<String, String> {
    let parts = dependency.name.split('/').collect::<Vec<_>>();
    if parts.len() != 3
        || parts[0] != "buf.build"
        || parts[1..].iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
        || dependency.commit.len() != 32
        || !dependency
            .commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(format!(
            "unsupported locked Protobuf dependency {}:{}",
            dependency.name, dependency.commit
        ));
    }
    Ok(format!(
        "https://{}/descriptor/{}",
        dependency.name, dependency.commit
    ))
}

fn modules(inputs: &[AnalysisInput]) -> Result<Vec<Module>, String> {
    let mut configs = inputs
        .iter()
        .filter(|input| input.kind == AnalysisInputKind::Configuration)
        .collect::<Vec<_>>();
    configs.sort_by(|left, right| left.path.cmp(&right.path));
    let config_directories = configs
        .iter()
        .map(|input| input.path.parent().unwrap_or(Path::new("")))
        .collect::<Vec<_>>();
    configs.retain(|input| {
        let directory = input.path.parent().unwrap_or(Path::new(""));
        !config_directories
            .iter()
            .any(|ancestor| *ancestor != directory && directory.starts_with(ancestor))
    });
    let mut protos = inputs
        .iter()
        .filter(|input| input.kind == AnalysisInputKind::Source)
        .collect::<Vec<_>>();
    protos.sort_by(|left, right| left.path.cmp(&right.path));
    if configs.is_empty() {
        return Ok((!protos.is_empty())
            .then(|| Module {
                name: PathBuf::from("."),
                roots: vec![PathBuf::new()],
                protos: protos.iter().map(|input| input.path.clone()).collect(),
                inputs: protos.into_iter().cloned().collect(),
                dependencies: Vec::new(),
            })
            .into_iter()
            .collect());
    }

    let configs = configs
        .into_iter()
        .map(|config_input| {
            let config_path = &config_input.path;
            let directory = config_path.parent().unwrap_or(Path::new("."));
            let config: BufConfig = serde_yaml::from_slice(&config_input.content)
                .map_err(|error| format!("invalid {}: {error}", config_path.display()))?;
            let mut module_configs = if config.modules.is_empty() {
                vec![BufModule {
                    path: PathBuf::from("."),
                    includes: Vec::new(),
                    excludes: Vec::new(),
                }]
            } else {
                config.modules
            };
            for module in &mut module_configs {
                module.path = relative_path(&module.path)?;
                module.includes = module
                    .includes
                    .iter()
                    .map(|path| relative_path(path))
                    .collect::<Result<_, _>>()?;
                module.excludes = module
                    .excludes
                    .iter()
                    .map(|path| relative_path(path))
                    .collect::<Result<_, _>>()?;
            }
            let roots = module_configs
                .iter()
                .map(|module| directory.join(&module.path))
                .collect::<Vec<_>>();
            Ok((
                config_input,
                directory.to_path_buf(),
                config.deps,
                module_configs,
                roots,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let module_roots = configs
        .iter()
        .flat_map(|(_, _, _, _, roots)| roots.iter().cloned())
        .collect::<Vec<_>>();
    let modules = configs
        .into_iter()
        .map(|(config_input, directory, deps, module_configs, roots)| {
            let config_path = &config_input.path;
            let lock_path = directory.join("buf.lock");
            let lock = inputs.iter().find(|input| {
                input.kind == AnalysisInputKind::Dependency && input.path == lock_path
            });
            let dependencies = if deps.is_empty() {
                Vec::new()
            } else {
                let lock = lock.ok_or_else(|| {
                    format!(
                        "{} declares dependencies but {} is missing",
                        config_path.display(),
                        lock_path.display()
                    )
                })?;
                let lock: BufLock = serde_yaml::from_slice(&lock.content)
                    .map_err(|error| format!("invalid {}: {error}", lock_path.display()))?;
                deps.into_iter()
                    .map(|declared| {
                        let name = declared
                            .split_once(':')
                            .map_or(declared.as_str(), |(name, _)| name);
                        lock.deps
                            .iter()
                            .find(|dependency| dependency.name == name)
                            .cloned()
                            .ok_or_else(|| {
                                format!("{name} is missing from {}", lock_path.display())
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };
            let mut config_inputs = vec![config_input.clone()];
            if let Some(lock) = lock {
                config_inputs.push(lock.clone());
            }
            config_inputs.extend(
                protos
                    .iter()
                    .filter(|input| owning_root(&input.path, &roots).is_some())
                    .map(|input| (*input).clone()),
            );
            config_inputs.sort_by(|left, right| left.path.cmp(&right.path));
            Ok(module_configs
                .into_iter()
                .zip(roots.iter().cloned())
                .filter_map(|(module, root)| {
                    let selected = protos
                        .iter()
                        .filter(|input| {
                            owning_root(&input.path, &module_roots) == Some(root.as_path())
                                && input.path.strip_prefix(&root).is_ok_and(|relative| {
                                    (module.includes.is_empty()
                                        || module
                                            .includes
                                            .iter()
                                            .any(|include| relative.starts_with(include)))
                                        && !module
                                            .excludes
                                            .iter()
                                            .any(|exclude| relative.starts_with(exclude))
                                })
                        })
                        .collect::<Vec<_>>();
                    if selected.is_empty() {
                        return None;
                    }
                    Some(Module {
                        name: root.clone(),
                        roots: roots.clone(),
                        protos: selected
                            .into_iter()
                            .map(|input| input.path.clone())
                            .collect(),
                        inputs: config_inputs.clone(),
                        dependencies: dependencies.clone(),
                    })
                })
                .collect::<Vec<_>>())
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(modules.into_iter().flatten().collect())
}

fn owning_root<'a>(path: &Path, roots: &'a [PathBuf]) -> Option<&'a Path> {
    roots
        .iter()
        .filter(|directory| path.starts_with(directory))
        .max_by_key(|directory| directory.components().count())
        .map(PathBuf::as_path)
}

fn relative_path(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            _ => return Err(format!("Buf path must be relative: {}", path.display())),
        }
    }
    Ok(normalized)
}

fn protobuf_file_name(path: &Path) -> Option<String> {
    let path = relative_path(path).ok()?;
    let mut name = String::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        if !name.is_empty() {
            name.push('/');
        }
        name.push_str(component.to_str()?);
    }
    (!name.is_empty()).then_some(name)
}

fn compilation_key(module: &Module) -> [u8; 32] {
    let mut hash = Sha256::new();
    framed_hash(&mut hash, b"compiled");
    framed_hash(&mut hash, COMPILER_ID.as_bytes());
    framed_hash(&mut hash, module.name.as_os_str().as_encoded_bytes());
    for input in &module.inputs {
        framed_hash(&mut hash, &[input_kind_tag(input.kind)]);
        framed_hash(&mut hash, input.path.as_os_str().as_encoded_bytes());
        framed_hash(&mut hash, &input.content);
    }
    hash.finalize().into()
}

fn input_kind_tag(kind: AnalysisInputKind) -> u8 {
    match kind {
        AnalysisInputKind::Source => 0,
        AnalysisInputKind::Configuration => 1,
        AnalysisInputKind::Dependency => 2,
        AnalysisInputKind::Toolchain => 3,
        AnalysisInputKind::Environment => 4,
    }
}

fn framed_hash(hash: &mut Sha256, bytes: &[u8]) {
    hash.update(bytes.len().to_le_bytes());
    hash.update(bytes);
}

fn content_key<'a>(kind: &str, inputs: impl IntoIterator<Item = (&'a Path, &'a [u8])>) -> [u8; 32] {
    let mut hash = Sha256::new();
    framed_hash(&mut hash, kind.as_bytes());
    for (path, bytes) in inputs {
        let path = path.as_os_str().as_encoded_bytes();
        framed_hash(&mut hash, path);
        framed_hash(&mut hash, bytes);
    }
    hash.finalize().into()
}

fn hex(key: [u8; 32]) -> String {
    key.into_iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts;
    use protox::prost::Message;
    use std::time::SystemTime;

    fn temporary(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("beholder-{name}-{unique}"))
    }

    fn input(path: impl Into<PathBuf>, content: impl Into<Vec<u8>>) -> AnalysisInput {
        let path = path.into();
        let kind = match path.file_name().and_then(|name| name.to_str()) {
            Some("buf.yaml") => AnalysisInputKind::Configuration,
            Some("buf.lock") => AnalysisInputKind::Dependency,
            _ => AnalysisInputKind::Source,
        };
        AnalysisInput {
            path,
            content: Arc::from(content.into()),
            kind,
        }
    }

    #[test]
    fn compilation_identity_includes_input_role() {
        let source = input("contract.proto", b"same bytes".to_vec());
        let mut configuration = source.clone();
        configuration.kind = AnalysisInputKind::Configuration;
        let module = |input| Module {
            name: PathBuf::from("."),
            roots: vec![PathBuf::new()],
            protos: vec![PathBuf::from("contract.proto")],
            inputs: vec![input],
            dependencies: Vec::new(),
        };

        assert_ne!(
            compilation_key(&module(source)),
            compilation_key(&module(configuration))
        );
    }

    #[test]
    fn compiles_immutable_snapshot_and_reuses_disk_cache() {
        let state = temporary("protobuf-compiler");
        let repository = state.join("repository");
        let cache = state.join("cache");
        fs::create_dir_all(&repository).unwrap();
        let path = PathBuf::from("contract.proto");
        let snapshot = b"syntax = \"proto3\"; package example; message Request {} service Example { rpc Snapshot(Request) returns (Request); }".to_vec();
        fs::write(
            repository.join(&path),
            b"syntax = \"proto3\"; package example; message Request {} service Example { rpc Live(Request) returns (Request); }",
        )
        .unwrap();
        let inputs = vec![input(path.clone(), snapshot)];

        let descriptors = SourceCompiler::new(cache.clone())
            .compile_repository(&inputs)
            .unwrap();
        let entities = facts(&descriptors[0]).unwrap().entities;
        assert!(
            entities
                .iter()
                .any(|entity| { entity.id.as_str() == "proto-method://example.Example/Snapshot" })
        );
        assert!(
            !entities
                .iter()
                .any(|entity| entity.id.as_str() == "proto-method://example.Example/Live")
        );
        assert_eq!(
            fs::read_dir(cache.join("protobuf/compiled"))
                .unwrap()
                .count(),
            1
        );

        fs::remove_file(repository.join(&path)).unwrap();
        assert!(
            SourceCompiler::new(cache.clone())
                .compile_repository(&inputs)
                .is_ok()
        );

        let changed = b"syntax = \"proto3\"; package example; message Request {} message Response {} service Example { rpc Get(Request) returns (Response); }".to_vec();
        SourceCompiler::new(cache.clone())
            .compile_repository(&[input(path, changed)])
            .unwrap();
        assert_eq!(
            fs::read_dir(cache.join("protobuf/compiled"))
                .unwrap()
                .count(),
            2
        );
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn requires_lock_for_declared_dependencies() {
        let state = temporary("protobuf-lock");
        fs::create_dir_all(&state).unwrap();
        let files = vec![
            input(
                "buf.yaml",
                b"version: v2\nmodules:\n  - path: .\ndeps:\n  - buf.build/fresha/common\n"
                    .to_vec(),
            ),
            input(
                "contract.proto",
                b"syntax = \"proto3\"; package example; message Request {}".to_vec(),
            ),
        ];

        let error = SourceCompiler::new(state.join("cache"))
            .compile_repository(&files)
            .unwrap_err();
        assert!(error.contains("declares dependencies but buf.lock is missing"));
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn compiles_buf_module_with_cached_locked_dependency() {
        let state = temporary("protobuf-dependency");
        let dependency = state.join("dependency");
        let cache = state.join("cache");
        fs::create_dir_all(dependency.join("fresha/types")).unwrap();
        let dependency_path = dependency.join("fresha/types/uuid.proto");
        fs::write(
            &dependency_path,
            "syntax = \"proto3\"; package fresha.types; message UUID { string value = 1; }",
        )
        .unwrap();
        let dependency_descriptor = protox::compile([&dependency_path], [&dependency])
            .unwrap()
            .encode_to_vec();
        let locked = LockedDependency {
            name: "buf.build/fresha/common".into(),
            commit: "00000000000000000000000000000000".into(),
            digest: "b5:locked-digest".into(),
        };
        let dependency_key = content_key(
            "dependency",
            [
                (Path::new("name"), locked.name.as_bytes()),
                (Path::new("commit"), locked.commit.as_bytes()),
                (Path::new("digest"), locked.digest.as_bytes()),
            ],
        );
        let dependency_cache = cache
            .join("protobuf/dependencies")
            .join(format!("{}.binpb", hex(dependency_key)));
        fs::create_dir_all(dependency_cache.parent().unwrap()).unwrap();
        fs::write(dependency_cache, dependency_descriptor).unwrap();

        let config = b"version: v2\nmodules:\n  - path: ./rpc/contracts\n    excludes:\n      - ignored\n  - path: events\ndeps:\n  - buf.build/fresha/common\n".to_vec();
        let nested_config = b"version: v1beta1\nbuild:\n  roots:\n    - contracts\n".to_vec();
        let lock = b"version: v2\ndeps:\n  - name: buf.build/fresha/common\n    commit: 00000000000000000000000000000000\n    digest: b5:locked-digest\n".to_vec();
        let proto = b"syntax = \"proto3\"; package example; import \"fresha/types/uuid.proto\"; message Request { fresha.types.UUID id = 1; } service Example { rpc Get(Request) returns (Request); }".to_vec();

        let descriptors = SourceCompiler::new(cache)
            .compile_repository(&[
                input("buf.yaml", config),
                input("rpc/buf.yaml", nested_config),
                input("buf.lock", lock),
                input("rpc/contracts/contract.proto", proto),
                input(
                    "events/event.proto",
                    b"syntax = \"proto3\"; package events; message Event {}".to_vec(),
                ),
                input(
                    "rpc/contracts/ignored/broken.proto",
                    b"not valid protobuf".to_vec(),
                ),
                input("rpc/legacy.proto", b"not valid protobuf".to_vec()),
            ])
            .unwrap();
        assert_eq!(descriptors.len(), 2);
        let compiled = facts(&descriptors[0]).unwrap();
        assert!(
            compiled
                .entities
                .iter()
                .any(|entity| { entity.id.as_str() == "proto-method://example.Example/Get" })
        );
        assert!(
            compiled
                .entities
                .iter()
                .any(|entity| { entity.id.as_str() == "proto-type://fresha.types.UUID" })
        );
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn resolves_imports_from_sibling_buf_modules() {
        let state = temporary("protobuf-sibling-modules");
        let config = b"version: v2\nmodules:\n  - path: api\n  - path: types\n".to_vec();
        let service = b"syntax = \"proto3\"; package example; import \"request.proto\"; service Example { rpc Get(Request) returns (Request); }".to_vec();
        let request = b"syntax = \"proto3\"; package example; message Request {}".to_vec();

        let descriptors = SourceCompiler::new(state.join("cache"))
            .compile_repository(&[
                input("buf.yaml", config),
                input("api/service.proto", service),
                input("types/request.proto", request),
            ])
            .unwrap();

        assert!(descriptors.iter().any(|descriptor| {
            facts(descriptor)
                .unwrap()
                .entities
                .iter()
                .any(|entity| entity.id.as_str() == "proto-method://example.Example/Get")
        }));
        fs::remove_dir_all(state).unwrap();
    }
}
